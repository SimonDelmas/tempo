import { Fragment, useEffect, useState } from "react";
import { api, Backlog, Config, Project, STATUS_LABELS, Task, today } from "../api";
import ProjectSelect from "./ProjectSelect";

type Props = {
  config: Config;
  revision: number;
  /** filtre projet, "" pour tous */
  project: string;
  projects: Project[];
  /** projets cités par des tâches sans note dans projets/ */
  orphans: string[];
  addProject: (project: Project) => void;
  /** ouvre le board d'une semaine */
  onPlan: (week: string) => void;
  onOpen: (path: string) => void;
  notify: (message: string, error?: boolean) => void;
};

/** groupe du backlog : un projet (feature), "" pour les tâches sans projet */
type Group = { id: string; title: string; missing?: boolean };

/** cible du glisser-déposer : le groupe et la tâche devant laquelle se placer (null : à la fin) */
type Target = { group: string; before: string | null };

/** Backlog : tâches regroupées par projet, ordonnées par glisser-déposer, planifiées par semaine. */
export default function BacklogView({ config, revision, project, projects, orphans, addProject, onPlan, onOpen, notify }: Props) {
  const [backlog, setBacklog] = useState<Backlog | null>(null);
  const [showDone, setShowDone] = useState(false);
  const [collapsed, setCollapsed] = useState<Set<string>>(new Set());
  const [draft, setDraft] = useState({ title: "", project: "", priority: "normale" });
  const [dragged, setDragged] = useState<string | null>(null);
  const [target, setTarget] = useState<Target | null>(null);
  /** projet glissé et projet devant lequel le déposer (null : à la fin) */
  const [draggedProject, setDraggedProject] = useState<string | null>(null);
  const [projectTarget, setProjectTarget] = useState<string | null | undefined>(undefined);
  /** ordre des projets renvoyé par le dernier déplacement, en attendant le rechargement */
  const [moved, setMoved] = useState<Project[] | null>(null);
  useEffect(() => setMoved(null), [projects]);

  const reload = () => api.backlog().then(setBacklog).catch((e) => notify(e.message, true));
  useEffect(() => { reload(); }, [revision]); // eslint-disable-line react-hooks/exhaustive-deps

  if (!backlog) return <div className="loading">Chargement…</div>;

  /** projets dans l'ordre du backlog : rang croissant, jamais ordonnés à la fin par titre */
  const ordered = moved ?? [...projects].sort((a, b) =>
    (a.rank ?? Infinity) - (b.rank ?? Infinity) || a.title.localeCompare(b.title, "fr"));
  const groups: Group[] = [
    ...ordered.map((p) => ({ id: p.id, title: p.title })),
    ...orphans.map((id) => ({ id, title: id, missing: true })),
    { id: "", title: "Sans projet" },
  ].filter((g) => !project || g.id === project);
  const tasksOf = (group: string) => backlog.tasks.filter((t) =>
    (t.project ?? "") === group && (showDone || t.status !== "terminee"));

  const create = async () => {
    if (!draft.title.trim()) return;
    try {
      const task = await api.createTask({
        title: draft.title, project: draft.project || project || null, priority: draft.priority,
      });
      notify(`Tâche ${task.id} ajoutée au backlog`);
      setDraft({ title: "", project: "", priority: "normale" });
      reload();
    } catch (e) {
      notify((e as Error).message, true);
    }
  };

  const plan = async (task: Task, week: string) => {
    try {
      await api.updateTask(task.id, { iteration: week || null });
      notify(week ? `${task.id} planifiée en ${week}` : `${task.id} retirée de ${task.iteration}`);
    } catch (e) {
      notify((e as Error).message, true);
    }
    reload();
  };

  const drop = async () => {
    const id = dragged, where = target;
    setDragged(null);
    setTarget(null);
    if (!id || !where || where.before === id) return;
    try {
      const tasks = await api.moveTask(id, where.group || null, where.before);
      setBacklog({ ...backlog, tasks });
    } catch (e) {
      notify((e as Error).message, true);
      reload();
    }
  };

  const endDrag = () => {
    setDragged(null);
    setTarget(null);
    setDraggedProject(null);
    setProjectTarget(undefined);
  };

  const dropProject = async () => {
    const id = draggedProject, before = projectTarget;
    endDrag();
    if (!id || before === undefined || before === id) return;
    try {
      setMoved(await api.moveProject(id, before));
    } catch (e) {
      notify((e as Error).message, true);
    }
  };

  /** en-tête de feature : dépôt d'un projet glissé avant ce projet (null : à la fin) */
  const projectZone = (before: string | null) => ({
    onDragOver: (e: React.DragEvent) => {
      e.preventDefault();
      e.stopPropagation();
      if (projectTarget !== before) setProjectTarget(before);
    },
    onDrop: (e: React.DragEvent) => { e.preventDefault(); e.stopPropagation(); dropProject(); },
  });

  /** zone de dépôt : survol et dépôt d'une tâche glissée */
  const dropZone = (where: Target) => ({
    onDragOver: (e: React.DragEvent) => {
      if (!dragged) return;
      e.preventDefault();
      e.stopPropagation();
      if (target?.group !== where.group || target?.before !== where.before) setTarget(where);
    },
    onDrop: (e: React.DragEvent) => { e.preventDefault(); e.stopPropagation(); drop(); },
  });

  /** semaines du sélecteur : à venir, plus la semaine passée encore portée par la tâche */
  const weekOptions = (task: Task) => {
    const weeks = backlog.weeks.map((w) => w.label);
    return task.iteration && !weeks.includes(task.iteration) ? [task.iteration, ...weeks] : weeks;
  };

  return (
    <>
      <div className="task-create">
        <input placeholder="Nouvelle tâche dans le backlog…" value={draft.title}
          onChange={(e) => setDraft({ ...draft, title: e.target.value })}
          onKeyDown={(e) => e.key === "Enter" && create()} />
        <ProjectSelect projects={projects} value={draft.project || project}
          onChange={(id) => setDraft((d) => ({ ...d, project: id }))} onCreated={addProject} notify={notify} />
        <select value={draft.priority} onChange={(e) => setDraft({ ...draft, priority: e.target.value })}>
          {config.priorities.map((p) => <option key={p}>{p}</option>)}
        </select>
        <button className="primary" onClick={create}>Ajouter</button>
        <span className="spacer" />
        <label className="check">
          <input type="checkbox" checked={showDone} onChange={(e) => setShowDone(e.target.checked)} /> Terminées
        </label>
      </div>

      <table className="backlog" onDragEnd={endDrag}>
        <thead>
          <tr>
            <th className="grip" />
            <th>Id</th>
            <th>Titre</th>
            <th>Statut</th>
            <th>Priorité</th>
            <th>Itération</th>
            <th>Échéance</th>
          </tr>
        </thead>
        <tbody>
          {groups.map((g) => {
            const items = tasksOf(g.id);
            const open = !collapsed.has(g.id);
            const toggle = () => setCollapsed((prev) => {
              const next = new Set(prev);
              if (next.has(g.id)) next.delete(g.id); else next.add(g.id);
              return next;
            });
            const atEnd = target?.group === g.id && target.before === null;
            /** seuls les projets ayant une note s'ordonnent (le rang est écrit dans la note) */
            const movable = Boolean(g.id) && !g.missing;
            const firstFixed = !movable && groups.findIndex((x) => !x.id || x.missing) === groups.indexOf(g);
            const projectBefore = movable ? g.id : null;
            const projectDrop = draggedProject !== null && (movable || firstFixed) && projectTarget === projectBefore;
            return (
              <Fragment key={g.id || "-"}>
                <tr className={`feature ${(atEnd && !open) || projectDrop ? "drop-before" : ""} ${draggedProject === g.id ? "dragging" : ""}`}
                  draggable={movable}
                  onDragStart={(e) => { e.dataTransfer.effectAllowed = "move"; setDraggedProject(g.id); }}
                  {...(draggedProject !== null
                    ? (movable || firstFixed ? projectZone(projectBefore) : {})
                    : dropZone({ group: g.id, before: open ? items[0]?.id ?? null : null }))}>
                  <td colSpan={7}>
                    {movable && <span className="feature-grip" title="Glisser pour ordonner les features">⋮⋮</span>}
                    <button className="ghost feature-toggle" onClick={toggle}>{open ? "▾" : "▸"}</button>
                    <span className={g.id ? "feature-title" : "feature-title muted"}>{g.title}</span>
                    {g.missing && <span className="muted small"> (sans note)</span>}
                    <span className="count">{items.length}</span>
                  </td>
                </tr>
                {open && items.map((t) => {
                  const overdue = t.due && t.due < today() && t.status !== "terminee";
                  const before = target?.group === g.id && target.before === t.id;
                  return (
                    <tr key={t.id} draggable
                      className={`task-row ${dragged === t.id ? "dragging" : ""} ${before ? "drop-before" : ""} ${t.status === "terminee" ? "done" : ""}`}
                      onDragStart={(e) => { e.dataTransfer.effectAllowed = "move"; setDragged(t.id); }}
                      {...dropZone({ group: g.id, before: t.id })}>
                      <td className="grip" title="Glisser pour ordonner ou changer de projet">⋮⋮</td>
                      <td className="muted small">{t.id}</td>
                      <td><button className="link" onClick={() => onOpen(t.path)}>{t.title}</button></td>
                      <td><span className={`status-pill status-${t.status}`}>{STATUS_LABELS[t.status] ?? t.status}</span></td>
                      <td className={t.priority === "haute" ? "overdue small" : "small"}>{t.priority}</td>
                      <td className="iteration">
                        <select value={t.iteration ?? ""} onChange={(e) => plan(t, e.target.value)}>
                          <option value="">—</option>
                          {weekOptions(t).map((w) => (
                            <option key={w} value={w}>{w}{w === backlog.current ? " (courante)" : ""}</option>
                          ))}
                        </select>
                        {t.iteration && (
                          <button className="ghost small" title={`Ouvrir le board ${t.iteration}`}
                            onClick={() => onPlan(t.iteration!)}>↗</button>
                        )}
                      </td>
                      <td className={`small ${overdue ? "overdue" : "muted"}`}>{t.due ?? ""}</td>
                    </tr>
                  );
                })}
                {open && (
                  <tr className={`feature-end ${atEnd ? "drop-before" : ""} ${dragged ? "visible" : ""}`}
                    {...dropZone({ group: g.id, before: null })}>
                    <td colSpan={7}>{items.length === 0 && !dragged ? <span className="muted small">Aucune tâche</span> : ""}</td>
                  </tr>
                )}
              </Fragment>
            );
          })}
        </tbody>
      </table>
    </>
  );
}
