import { useEffect, useState } from "react";
import { api, Board, Config, Project, STATUS_LABELS, today } from "../api";
import ProjectSelect, { projectTitle } from "./ProjectSelect";

type Props = {
  config: Config;
  revision: number;
  /** semaine affichée, "" pour la semaine courante */
  week: string;
  project: string;
  projects: Project[];
  addProject: (project: Project) => void;
  onWeek: (week: string | null) => void;
  onOpen: (path: string) => void;
  notify: (message: string, error?: boolean) => void;
};

const shortDate = (iso: string) => iso.slice(8, 10) + "/" + iso.slice(5, 7);

/** Board d'une semaine : colonnes par statut dans l'ordre du backlog ; glisser une carte la
 *  place dans une colonne (statut) et dans l'ordre, repris par le backlog. */
export default function BoardView({ config, revision, week, project, projects, addProject, onWeek, onOpen, notify }: Props) {
  const [board, setBoard] = useState<Board | null>(null);
  const [showDone, setShowDone] = useState(true);
  const [draft, setDraft] = useState({ title: "", project: "", priority: "normale", due: "" });
  const [dragged, setDragged] = useState<string | null>(null);
  /** colonne survolée et carte devant laquelle déposer (null : fin de colonne) */
  const [over, setOver] = useState<{ status: string; before: string | null } | null>(null);

  const reload = () => api.board(week || null).then(setBoard).catch((e) => notify(e.message, true));
  useEffect(() => { reload(); }, [revision, week]); // eslint-disable-line react-hooks/exhaustive-deps

  if (!board) return <div className="loading">Chargement…</div>;
  const visible = board.tasks.filter((t) => !project || (project === "-" ? !t.project : t.project === project));
  const columns = config.statuses.filter((s) => showDone || s !== "terminee");
  const past = board.week < board.current;
  const select = (w: string) => onWeek(w === board.current ? null : w);

  const create = async () => {
    if (!draft.title.trim()) return;
    try {
      const task = await api.createTask({
        title: draft.title, project: draft.project || (project !== "-" && project) || null,
        priority: draft.priority, due: draft.due || null, iteration: board.week,
      });
      notify(`Tâche ${task.id} créée en ${board.week}`);
      setDraft({ title: "", project: "", priority: "normale", due: "" });
      reload();
    } catch (e) {
      notify((e as Error).message, true);
    }
  };

  /** dépose la carte dans la colonne `status`, avant la carte `before` (fin de colonne sans) */
  const move = async (id: string, status: string, before: string | null) => {
    const task = board.tasks.find((t) => t.id === id);
    if (!task || before === id) return;
    const column = visible.filter((t) => t.status === status && t.id !== id);
    const where = before ? { before } : column.length ? { after: column[column.length - 1].id } : null;
    // affichage immédiat : la carte change de colonne et prend sa place
    const others = board.tasks.filter((t) => t.id !== id);
    const anchor = where && ("before" in where ? where.before : where.after);
    const at = anchor ? others.findIndex((t) => t.id === anchor) + (where && "after" in where ? 1 : 0) : others.length;
    setBoard({ ...board, tasks: [...others.slice(0, at), { ...task, status }, ...others.slice(at)] });
    try {
      if (task.status !== status) await api.updateTask(id, { status });
      if (where) await api.moveTask(id, where);
    } catch (e) {
      notify((e as Error).message, true);
    }
    reload();
  };

  const endDrag = () => {
    setDragged(null);
    setOver(null);
  };

  return (
    <>
      <div className="week-bar">
        <button onClick={() => select(board.previous)} title="Semaine précédente">‹</button>
        <select value={board.week} onChange={(e) => select(e.target.value)}>
          {board.weeks.map((w) => <option key={w} value={w}>{w}{w === board.current ? " (courante)" : ""}</option>)}
          {!board.weeks.includes(board.next) && <option value={board.next}>{board.next}</option>}
        </select>
        <button onClick={() => select(board.next)} title="Semaine suivante">›</button>
        <span className="muted">du {shortDate(board.start)} au {shortDate(board.end)}</span>
        {board.week !== board.current && <button className="link" onClick={() => onWeek(null)}>Semaine courante</button>}
        <span className="spacer" />
        <label className="check">
          <input type="checkbox" checked={showDone} onChange={(e) => setShowDone(e.target.checked)} /> Terminées
        </label>
      </div>
      {past && <p className="muted small">Semaine passée : les tâches non terminées ont été reportées, leur statut est le statut actuel.</p>}

      {!past && (
        <div className="task-create">
          <input placeholder={`Nouvelle tâche en ${board.week}…`} value={draft.title}
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
      )}

      {board.tasks.length === 0 && (
        <p className="muted">Aucune tâche planifiée en {board.week} : planifiez-en depuis le backlog.</p>
      )}

      <div className="kanban" onDragEnd={endDrag}>
        {columns.map((status) => {
          const items = visible.filter((t) => t.status === status);
          const target = over?.status === status ? over : null;
          const drop = (e: React.DragEvent) => {
            e.preventDefault();
            e.stopPropagation();
            const id = dragged || e.dataTransfer.getData("text/plain");
            const before = target?.before ?? null;
            endDrag();
            if (id) move(id, status, before);
          };
          /** survol : avant la carte `before`, ou en fin de colonne (null) */
          const hover = (before: string | null) => (e: React.DragEvent) => {
            if (!dragged) return;
            e.preventDefault();
            e.stopPropagation();
            e.dataTransfer.dropEffect = "move";
            if (target?.before !== before || !target) setOver({ status, before });
          };
          return (
            <section key={status} className={`column status-${status}${target ? " drop-target" : ""}`}
              onDragOver={hover(null)}
              onDragLeave={(e) => {
                // ignore les passages d'un enfant à l'autre de la colonne
                if (!e.currentTarget.contains(e.relatedTarget as Node | null)) setOver(null);
              }}
              onDrop={drop}>
              <h3>{STATUS_LABELS[status]} <span className="count">{items.length}</span></h3>
              {items.map((t) => {
                const overdue = t.due && t.due < today() && t.status !== "terminee";
                const carried = t.iterations.indexOf(board.week) > 0;
                const before = target?.before === t.id && dragged !== t.id;
                return (
                  <article key={t.id} draggable
                    className={`card priority-${t.priority}${dragged === t.id ? " dragging" : ""}${before ? " drop-before" : ""}`}
                    onDragStart={(e) => {
                      e.dataTransfer.effectAllowed = "move";
                      e.dataTransfer.setData("text/plain", t.id);
                      setDragged(t.id);
                    }}
                    onDragOver={hover(t.id)}
                    onDrop={drop}
                    onClick={() => onOpen(t.path)}>
                    <div className="card-top">
                      <span className="muted small">{t.id}</span>
                      {t.priority === "haute" && <span className="tag high">haute</span>}
                    </div>
                    <div className="card-title">{t.title}</div>
                    <div className="card-meta">
                      {t.project && <span className="tag">{projectTitle(projects, t.project)}</span>}
                      {t.due && <span className={`small ${overdue ? "overdue" : "muted"}`}>⏱ {t.due}</span>}
                      {carried && <span className="small muted" title={t.iterations.join(" → ")}>↻ reportée</span>}
                      {t.iteration !== board.week && <span className="small muted">→ {t.iteration}</span>}
                    </div>
                  </article>
                );
              })}
              {target && target.before === null && dragged && <div className="card-slot" />}
            </section>
          );
        })}
      </div>
    </>
  );
}
