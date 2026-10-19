import { useEffect, useMemo, useState } from "react";
import { api, Config, STATUS_LABELS, Task, today } from "../api";
import ProjectSelect, { projectTitle, useProjects } from "./ProjectSelect";

type Props = {
  config: Config;
  revision: number;
  /** filtre projet, porté par l'adresse (#/taches?projet=…) */
  project: string;
  onProject: (project: string | null) => void;
  onOpen: (path: string) => void;
  notify: (message: string, error?: boolean) => void;
};

export default function TasksView({ config, revision, project, onProject, onOpen, notify }: Props) {
  const [tasks, setTasks] = useState<Task[]>([]);
  const [showDone, setShowDone] = useState(true);
  const [draft, setDraft] = useState({ title: "", project: "", priority: "normale", due: "" });
  const [dragged, setDragged] = useState<string | null>(null);

  const reload = () => api.tasks().then(setTasks).catch((e) => notify(e.message, true));
  useEffect(() => { reload(); }, [revision]); // eslint-disable-line react-hooks/exhaustive-deps

  const [projects, addProject] = useProjects(revision);
  /** projets cités par des tâches sans note dans projets/ */
  const orphans = useMemo(() => [...new Set(tasks.map((t) => t.project).filter(Boolean) as string[])]
    .filter((id) => !projects.some((p) => p.id === id)).sort(), [tasks, projects]);
  const visible = tasks.filter((t) => !project || t.project === project);
  const columns = config.statuses.filter((s) => showDone || s !== "terminee");

  const create = async () => {
    if (!draft.title.trim()) return;
    try {
      const task = await api.createTask({
        title: draft.title, project: draft.project || project || null,
        priority: draft.priority, due: draft.due || null,
      });
      notify(`Tâche ${task.id} créée`);
      setDraft({ title: "", project: "", priority: "normale", due: "" });
      reload();
    } catch (e) {
      notify((e as Error).message, true);
    }
  };

  const move = async (id: string, status: string) => {
    const task = tasks.find((t) => t.id === id);
    if (!task || task.status === status) return;
    setTasks((prev) => prev.map((t) => (t.id === id ? { ...t, status } : t)));
    try {
      await api.updateTask(id, { status });
    } catch (e) {
      notify((e as Error).message, true);
    }
    reload();
  };

  return (
    <div className="page tasks-view">
      <header className="page-header">
        <h2>Tâches</h2>
        <select value={project} onChange={(e) => onProject(e.target.value || null)}>
          <option value="">Tous les projets</option>
          {projects.map((p) => <option key={p.id} value={p.id}>{p.title}</option>)}
          {orphans.map((p) => <option key={p} value={p}>{p} (sans note)</option>)}
        </select>
        <label className="check">
          <input type="checkbox" checked={showDone} onChange={(e) => setShowDone(e.target.checked)} /> Terminées
        </label>
      </header>

      <div className="task-create">
        <input placeholder="Nouvelle tâche…" value={draft.title}
          onChange={(e) => setDraft({ ...draft, title: e.target.value })}
          onKeyDown={(e) => e.key === "Enter" && create()} />
        <ProjectSelect projects={projects} value={draft.project || project}
          onChange={(id) => setDraft((d) => ({ ...d, project: id }))} onCreated={addProject} notify={notify} />
        <select value={draft.priority} onChange={(e) => setDraft({ ...draft, priority: e.target.value })}>
          {config.priorities.map((p) => <option key={p}>{p}</option>)}
        </select>
        <input type="date" value={draft.due} onChange={(e) => setDraft({ ...draft, due: e.target.value })} />
        <button className="primary" onClick={create}>Ajouter</button>
      </div>

      <div className="kanban">
        {columns.map((status) => {
          const items = visible.filter((t) => t.status === status);
          return (
            <section key={status} className={`column status-${status}`}
              onDragOver={(e) => e.preventDefault()}
              onDrop={() => { if (dragged) move(dragged, status); setDragged(null); }}>
              <h3>{STATUS_LABELS[status]} <span className="count">{items.length}</span></h3>
              {items.map((t) => {
                const overdue = t.due && t.due < today() && t.status !== "terminee";
                return (
                  <article key={t.id} className={`card priority-${t.priority}`} draggable
                    onDragStart={() => setDragged(t.id)} onClick={() => onOpen(t.path)}>
                    <div className="card-top">
                      <span className="muted small">{t.id}</span>
                      {t.priority === "haute" && <span className="tag high">haute</span>}
                    </div>
                    <div className="card-title">{t.title}</div>
                    <div className="card-meta">
                      {t.project && <span className="tag">{projectTitle(projects, t.project)}</span>}
                      {t.due && <span className={`small ${overdue ? "overdue" : "muted"}`}>⏱ {t.due}</span>}
                    </div>
                  </article>
                );
              })}
            </section>
          );
        })}
      </div>
    </div>
  );
}
