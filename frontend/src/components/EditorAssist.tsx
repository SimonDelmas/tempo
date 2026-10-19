import { useState } from "react";
import { api, Config, TaskDraft, TextAction } from "../api";
import ProjectSelect, { useProjects } from "./ProjectSelect";

/** Fenêtres des actions IA de l'éditeur : rien n'est écrit sans validation. */

type Notify = (message: string, error?: boolean) => void;

type DraftRow = TaskDraft & { selected: boolean };

export function ExtractTasksDialog({ drafts, config, origin, onCreated, onClose, notify }: {
  drafts: TaskDraft[];
  config: Config;
  /** lien de la note d'origine, tracé dans l'historique des tâches */
  origin: string;
  /** tâches créées : id et titre, pour insérer leurs liens dans la note */
  onCreated: (tasks: { id: string; title: string }[]) => void;
  onClose: () => void;
  notify: Notify;
}) {
  const [rows, setRows] = useState<DraftRow[]>(() => drafts.map((d) => ({ ...d, selected: !d.duplicate_of })));
  const [busy, setBusy] = useState(false);
  const [projects, addProject] = useProjects(0);
  const set = (i: number, patch: Partial<DraftRow>) =>
    setRows((prev) => prev.map((r, j) => (j === i ? { ...r, ...patch } : r)));

  const create = async () => {
    setBusy(true);
    const created: { id: string; title: string }[] = [];
    try {
      for (const r of rows.filter((r) => r.selected && r.title.trim())) {
        const task = await api.createTask({
          title: r.title, description: r.description, project: r.project || null,
          priority: r.priority, due: r.due || null, origin,
        });
        created.push({ id: task.id, title: task.title });
      }
      notify(`${created.length} tâche(s) créée(s)`);
      onCreated(created);
    } catch (e) {
      notify((e as Error).message, true);
      if (created.length) onCreated(created);
    } finally {
      setBusy(false);
    }
  };

  const count = rows.filter((r) => r.selected).length;
  return (
    <div className="modal-backdrop" onMouseDown={onClose}>
      <div className="modal assist-modal" onMouseDown={(e) => e.stopPropagation()}>
        <h3>Tâches repérées dans la note</h3>
        {rows.length === 0 && <p className="muted">Aucune action pour vous dans cette note.</p>}
        {rows.map((r, i) => (
          <div key={i} className={`proposal-card ${r.selected ? "" : "rejected"}`}>
            <input type="checkbox" checked={r.selected} onChange={(e) => set(i, { selected: e.target.checked })} />
            <input className="grow" value={r.title} onChange={(e) => set(i, { title: e.target.value })} />
            <ProjectSelect projects={projects} value={r.project ?? ""} onChange={(id) => set(i, { project: id })}
              onCreated={addProject} notify={notify} />
            <select value={r.priority} onChange={(e) => set(i, { priority: e.target.value })}>
              {config.priorities.map((p) => <option key={p}>{p}</option>)}
            </select>
            <input type="date" value={r.due ?? ""} onChange={(e) => set(i, { due: e.target.value })} />
            {r.duplicate_of && <span className="tag" title="Une tâche ouverte couvre déjà cette action">
              déjà : {r.duplicate_of}</span>}
            {r.description && <div className="muted small full-row">{r.description}</div>}
          </div>
        ))}
        <div className="row">
          <button className="primary" disabled={busy || count === 0} onClick={create}>
            {busy ? "Création…" : `Créer ${count} tâche${count > 1 ? "s" : ""}`}
          </button>
          <button className="ghost" onClick={onClose}>Annuler</button>
          <span className="muted small">Les liens des tâches créées sont ajoutés à la fin de la note.</span>
        </div>
      </div>
    </div>
  );
}

export const TEXT_ACTION_LABELS: Record<TextAction, string> = {
  resumer: "Résumer",
  reformuler: "Reformuler",
  actions: "Lister les actions",
};

export function TextResultDialog({ action, result, hasSelection, onReplace, onInsert, onClose, notify }: {
  action: TextAction;
  result: string;
  hasSelection: boolean;
  onReplace: (text: string) => void;
  onInsert: (text: string) => void;
  onClose: () => void;
  notify: Notify;
}) {
  const [text, setText] = useState(result);
  return (
    <div className="modal-backdrop" onMouseDown={onClose}>
      <div className="modal assist-modal" onMouseDown={(e) => e.stopPropagation()}>
        <h3>{TEXT_ACTION_LABELS[action]} — proposition</h3>
        <textarea rows={12} value={text} onChange={(e) => setText(e.target.value)} />
        <div className="row">
          {hasSelection && <button className="primary" onClick={() => onReplace(text)}>Remplacer la sélection</button>}
          <button className={hasSelection ? "" : "primary"} onClick={() => onInsert(text)}>
            {hasSelection ? "Insérer après la sélection" : "Insérer à la fin de la note"}
          </button>
          <button onClick={() => navigator.clipboard.writeText(text).then(() => notify("Copié"))}>Copier</button>
          <button className="ghost" onClick={onClose}>Fermer</button>
        </div>
      </div>
    </div>
  );
}
