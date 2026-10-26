import { useEffect, useState } from "react";
import { api, Backlog, Config, Project, STATUS_LABELS, Task, today } from "../api";
import ProjectSelect, { projectTitle } from "./ProjectSelect";

type Props = {
  config: Config;
  revision: number;
  /** filtre projet, "" pour tous, "-" pour les tâches sans projet */
  project: string;
  /** change le filtre projet (porté par l'adresse, partagé avec le board) */
  onProject: (project: string) => void;
  projects: Project[];
  /** projets cités par des tâches sans note dans projets/ */
  orphans: string[];
  addProject: (project: Project) => void;
  /** ouvre le board d'une semaine */
  onPlan: (week: string) => void;
  onOpen: (path: string) => void;
  notify: (message: string, error?: boolean) => void;
};

/** filtres des colonnes, "" pour ne pas filtrer */
type Filters = { search: string; status: string; priority: string; iteration: string; due: string };
const NO_FILTER: Filters = { search: "", status: "", priority: "", iteration: "", due: "" };

/** cible du glisser-déposer : avant une tâche, ou après la dernière tâche affichée */
type Target = { before: string } | { after: string };

/** texte comparable pour la recherche : minuscules, sans accents */
const fold = (s: string) => s.normalize("NFD").replace(/[̀-ͯ]/g, "").toLowerCase();

/**
 * Backlog : toutes les tâches dans un seul ordre (celui des colonnes du board), ordonnées par
 * glisser-déposer, filtrables par colonne et planifiées par semaine.
 */
export default function BacklogView({ config, revision, project, onProject, projects, orphans, addProject, onPlan, onOpen, notify }: Props) {
  const [backlog, setBacklog] = useState<Backlog | null>(null);
  const [showDone, setShowDone] = useState(false);
  const [filters, setFilters] = useState<Filters>(NO_FILTER);
  const [draft, setDraft] = useState({ title: "", project: "", priority: "normale" });
  const [dragged, setDragged] = useState<string | null>(null);
  const [target, setTarget] = useState<Target | null>(null);

  const reload = () => api.backlog().then(setBacklog).catch((e) => notify(e.message, true));
  useEffect(() => { reload(); }, [revision]); // eslint-disable-line react-hooks/exhaustive-deps

  if (!backlog) return <div className="loading">Chargement…</div>;

  const filter = (key: keyof Filters) => (value: string) => setFilters((f) => ({ ...f, [key]: value }));
  const filtered = Boolean(project) || Object.values(filters).some(Boolean);
  const search = fold(filters.search.trim());
  const visible = backlog.tasks.filter((t) => {
    if (filters.status ? t.status !== filters.status : !showDone && t.status === "terminee") return false;
    if (project && (project === "-" ? t.project : t.project !== project)) return false;
    if (filters.priority && t.priority !== filters.priority) return false;
    if (filters.iteration && (filters.iteration === "-" ? t.iteration : t.iteration !== filters.iteration)) return false;
    if (filters.due === "retard" && !(t.due && t.due < today() && t.status !== "terminee")) return false;
    if (filters.due === "avec" && !t.due) return false;
    if (filters.due === "sans" && t.due) return false;
    if (search) {
      const text = [t.id, t.title, t.project ? projectTitle(projects, t.project) : "", t.iteration ?? ""].join(" ");
      if (!fold(text).includes(search)) return false;
    }
    return true;
  });
  /** semaines du filtre Itération : planifiables et déjà portées par des tâches */
  const iterations = [...new Set([...backlog.weeks.map((w) => w.label),
    ...backlog.tasks.map((t) => t.iteration).filter(Boolean) as string[]])].sort();

  const create = async () => {
    if (!draft.title.trim()) return;
    try {
      const task = await api.createTask({
        title: draft.title, project: draft.project || (project !== "-" && project) || null, priority: draft.priority,
      });
      notify(`Tâche ${task.id} ajoutée au backlog`);
      setDraft({ title: "", project: "", priority: "normale" });
      reload();
    } catch (e) {
      notify((e as Error).message, true);
    }
  };

  const update = async (task: Task, changes: Record<string, string | null>, message: string) => {
    try {
      await api.updateTask(task.id, changes);
      notify(message);
    } catch (e) {
      notify((e as Error).message, true);
    }
    reload();
  };

  const archive = async () => {
    try {
      const { archived } = await api.archiveTasks();
      notify(archived.length ? `${archived.length} tâche(s) archivée(s) : ${archived.join(", ")}` : "Aucune tâche terminée à archiver");
    } catch (e) {
      notify((e as Error).message, true);
    }
    reload();
  };

  const plan = (task: Task, week: string) => update(task, { iteration: week || null },
    week ? `${task.id} planifiée en ${week}` : `${task.id} retirée de ${task.iteration}`);

  const setProject = (task: Task, id: string) => update(task, { project: id || null },
    id ? `${task.id} rattachée à ${projectTitle(projects, id)}` : `${task.id} sans projet`);

  const setStatus = (task: Task, status: string) => update(task, { status },
    `${task.id} : ${STATUS_LABELS[status] ?? status}`);

  const setPriority = (task: Task, priority: string) => update(task, { priority },
    `${task.id} : priorité ${priority}`);

  const setDue = (task: Task, due: string) => {
    if (due === (task.due ?? "")) return;
    update(task, { due: due || null }, due ? `${task.id} : échéance au ${due}` : `${task.id} sans échéance`);
  };

  const endDrag = () => {
    setDragged(null);
    setTarget(null);
  };

  const drop = async () => {
    const id = dragged, where = target;
    endDrag();
    if (!id || !where || ("before" in where ? where.before : where.after) === id) return;
    try {
      setBacklog({ ...backlog, tasks: await api.moveTask(id, where) });
    } catch (e) {
      notify((e as Error).message, true);
      reload();
    }
  };

  /** zone de dépôt : survol et dépôt d'une tâche glissée */
  const dropZone = (where: Target) => ({
    onDragOver: (e: React.DragEvent) => {
      if (!dragged) return;
      e.preventDefault();
      if (JSON.stringify(target) !== JSON.stringify(where)) setTarget(where);
    },
    onDrop: (e: React.DragEvent) => { e.preventDefault(); drop(); },
  });

  /** semaines du sélecteur : à venir, plus la semaine passée encore portée par la tâche */
  const weekOptions = (task: Task) => {
    const weeks = backlog.weeks.map((w) => w.label);
    return task.iteration && !weeks.includes(task.iteration) ? [task.iteration, ...weeks] : weeks;
  };

  const last = visible[visible.length - 1];
  const atEnd = Boolean(last && target && "after" in target);

  return (
    <>
      <div className="task-create">
        <input placeholder="Nouvelle tâche dans le backlog…" value={draft.title}
          onChange={(e) => setDraft({ ...draft, title: e.target.value })}
          onKeyDown={(e) => e.key === "Enter" && create()} />
        <ProjectSelect projects={projects} value={draft.project || (project !== "-" ? project : "")}
          onChange={(id) => setDraft((d) => ({ ...d, project: id }))} onCreated={addProject} notify={notify} />
        <select value={draft.priority} onChange={(e) => setDraft({ ...draft, priority: e.target.value })}>
          {config.priorities.map((p) => <option key={p}>{p}</option>)}
        </select>
        <button className="primary" onClick={create}>Ajouter</button>
        <span className="spacer" />
        <label className="check">
          <input type="checkbox" checked={showDone} onChange={(e) => setShowDone(e.target.checked)} /> Terminées
        </label>
        <button disabled={!backlog.tasks.some((t) => t.status === "terminee")} onClick={archive}
          title="Range les tâches terminées dans taches/archives/ : elles restent liées et visibles dans les boards passés">
          Archiver les terminées
        </button>
      </div>

      <table className="backlog" onDragEnd={endDrag}>
        <thead>
          <tr>
            <th className="grip" />
            <th>Id</th>
            <th>Titre</th>
            <th>Feature</th>
            <th>Statut</th>
            <th>Priorité</th>
            <th>Itération</th>
            <th>Échéance</th>
          </tr>
          <tr className="filters">
            <th />
            <th colSpan={2}>
              <input type="search" placeholder="Rechercher…" value={filters.search}
                onChange={(e) => filter("search")(e.target.value)} />
            </th>
            <th>
              <select value={project} onChange={(e) => onProject(e.target.value)}>
                <option value="">Toutes</option>
                {projects.map((p) => <option key={p.id} value={p.id}>{p.title}</option>)}
                {orphans.map((p) => <option key={p} value={p}>{p} (sans note)</option>)}
                <option value="-">Sans projet</option>
              </select>
            </th>
            <th>
              <select value={filters.status} onChange={(e) => filter("status")(e.target.value)}>
                <option value="">{showDone ? "Tous" : "Non terminées"}</option>
                {config.statuses.map((s) => <option key={s} value={s}>{STATUS_LABELS[s] ?? s}</option>)}
              </select>
            </th>
            <th>
              <select value={filters.priority} onChange={(e) => filter("priority")(e.target.value)}>
                <option value="">Toutes</option>
                {config.priorities.map((p) => <option key={p}>{p}</option>)}
              </select>
            </th>
            <th>
              <select value={filters.iteration} onChange={(e) => filter("iteration")(e.target.value)}>
                <option value="">Toutes</option>
                <option value="-">Non planifiées</option>
                {iterations.map((w) => <option key={w} value={w}>{w}{w === backlog.current ? " (courante)" : ""}</option>)}
              </select>
            </th>
            <th>
              <select value={filters.due} onChange={(e) => filter("due")(e.target.value)}>
                <option value="">Toutes</option>
                <option value="retard">En retard</option>
                <option value="avec">Avec échéance</option>
                <option value="sans">Sans échéance</option>
              </select>
            </th>
          </tr>
        </thead>
        <tbody>
          {visible.map((t) => {
            const overdue = t.due && t.due < today() && t.status !== "terminee";
            const before = target && "before" in target && target.before === t.id;
            return (
              <tr key={t.id} draggable
                className={`task-row ${dragged === t.id ? "dragging" : ""} ${before ? "drop-before" : ""} ${t.status === "terminee" ? "done" : ""}`}
                onDragStart={(e) => { e.dataTransfer.effectAllowed = "move"; setDragged(t.id); }}
                {...dropZone({ before: t.id })}>
                <td className="grip" title="Glisser pour ordonner (l'ordre est repris par le board)">⋮⋮</td>
                <td className="id muted small">{t.id}</td>
                <td className="title"><button className="link" onClick={() => onOpen(t.path)}>{t.title}</button></td>
                <td className="feature">
                  <ProjectSelect projects={projects} value={t.project ?? ""} emptyLabel="—"
                    onChange={(id) => setProject(t, id)} onCreated={addProject} notify={notify} />
                </td>
                <td className="status">
                  <select className={`status-pill status-${t.status}`} value={t.status}
                    onChange={(e) => setStatus(t, e.target.value)}>
                    {config.statuses.map((s) => <option key={s} value={s}>{STATUS_LABELS[s] ?? s}</option>)}
                  </select>
                </td>
                <td className="priority">
                  <select className={t.priority === "haute" ? "overdue" : ""} value={t.priority}
                    onChange={(e) => setPriority(t, e.target.value)}>
                    {config.priorities.map((p) => <option key={p}>{p}</option>)}
                  </select>
                </td>
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
                <td className="due">
                  {/* clé sur l'échéance : le champ, non contrôlé, repart de la valeur enregistrée après chaque rechargement */}
                  <input type="date" key={t.due ?? ""} defaultValue={t.due ?? ""} className={overdue ? "overdue" : ""}
                    onBlur={(e) => setDue(t, e.target.value)}
                    onKeyDown={(e) => e.key === "Enter" && e.currentTarget.blur()} />
                </td>
              </tr>
            );
          })}
          <tr className={`backlog-end ${atEnd ? "drop-before" : ""} ${dragged ? "visible" : ""}`}
            {...(last ? dropZone({ after: last.id }) : {})}>
            <td colSpan={8}>
              {visible.length === 0 && (
                <span className="muted small">{filtered ? "Aucune tâche ne correspond aux filtres" : "Aucune tâche"}</span>
              )}
            </td>
          </tr>
        </tbody>
      </table>
      <p className="muted small backlog-footer">
        {visible.length} tâche{visible.length > 1 ? "s" : ""} affichée{visible.length > 1 ? "s" : ""} sur {backlog.tasks.length}
        {filtered && (
          <button className="link" onClick={() => { setFilters(NO_FILTER); onProject(""); }}>Effacer les filtres</button>
        )}
      </p>
    </>
  );
}
