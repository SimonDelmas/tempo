import { useCallback, useEffect, useRef, useState } from "react";
import { api, Config, Draft, DraftProposal, drafts as draftsApi, NOTE_KINDS, NoteSummary, STATUS_LABELS,
  TYPE_LABELS } from "../api";
import MarkdownEditor from "./MarkdownEditor";
import ProjectSelect, { ProjectsSelect, useProjects } from "./ProjectSelect";
import { documentHref, navigate } from "../router";

type Props = {
  config: Config;
  files: NoteSummary[];
  /** brouillon affiché (paramètre `d` de l'adresse) */
  current: string | null;
  onSelect: (id: string | null) => void;
  onOpen: (path: string) => void;
  notify: (message: string, error?: boolean) => void;
};

type Pending = { title?: string; content?: string };

/** Libellé d'onglet : le titre, sinon la première ligne du texte. */
export function draftLabel(d: Draft) {
  if (d.title.trim()) return d.title.trim();
  const line = d.content.split("\n").map((l) => l.replace(/^[\s#>*\-+]*(\[[ x]\]\s*)?/i, "").trim()).find(Boolean);
  if (!line) return "Sans titre";
  return line.length > 32 ? line.slice(0, 31) + "…" : line;
}

/**
 * Espace Draft : un onglet par brouillon, enregistré automatiquement dans `.brouillons/` du
 * vault. « Analyser » demande à l'IA une note et/ou des tâches, créées après validation.
 */
export default function DraftsView({ config, files, current, onSelect, onOpen, notify }: Props) {
  const [list, setList] = useState<Draft[] | null>(null);
  const [proposals, setProposals] = useState<Record<string, DraftProposal>>({});
  const [busy, setBusy] = useState(false);
  const [close, setClose] = useState(true);
  const [saving, setSaving] = useState(false);
  const [projects, addProject] = useProjects(0);
  const pending = useRef(new Map<string, Pending>());
  const timer = useRef<number | undefined>(undefined);

  useEffect(() => {
    draftsApi.list().then(setList).catch((e) => notify(e.message, true));
  }, [notify]);

  /** Envoie les modifications en attente (changement d'onglet, analyse, fermeture de la vue). */
  const flush = useCallback(async () => {
    window.clearTimeout(timer.current);
    const entries = [...pending.current.entries()];
    pending.current.clear();
    if (!entries.length) return;
    setSaving(true);
    try {
      await Promise.all(entries.map(([id, changes]) => draftsApi.save(id, changes)));
    } catch (e) {
      notify(`Brouillon non enregistré : ${(e as Error).message}`, true);
    } finally {
      setSaving(false);
    }
  }, [notify]);

  useEffect(() => {
    const onUnload = () => { flush(); };
    window.addEventListener("beforeunload", onUnload);
    return () => {
      window.removeEventListener("beforeunload", onUnload);
      flush();
    };
  }, [flush]);

  const active = list?.find((d) => d.id === current) ?? list?.[0] ?? null;
  const proposal = active ? proposals[active.id] ?? null : null;

  const edit = (id: string, changes: Pending) => {
    setList((prev) => prev?.map((d) => (d.id === id ? { ...d, ...changes } : d)) ?? prev);
    pending.current.set(id, { ...pending.current.get(id), ...changes });
    window.clearTimeout(timer.current);
    timer.current = window.setTimeout(flush, 600);
  };

  const select = async (id: string) => {
    await flush();
    onSelect(id);
  };

  const add = async () => {
    try {
      await flush();
      const draft = await draftsApi.create();
      setList((prev) => [...(prev ?? []), draft]);
      onSelect(draft.id);
    } catch (e) {
      notify((e as Error).message, true);
    }
  };

  /** Retire l'onglet de la liste et sélectionne son voisin. */
  const drop = (id: string) => {
    const index = list?.findIndex((d) => d.id === id) ?? -1;
    const rest = (list ?? []).filter((d) => d.id !== id);
    pending.current.delete(id);
    setList(rest);
    setProposals(({ [id]: _, ...others }) => others);
    if (active?.id === id) onSelect(rest[Math.min(Math.max(index, 0), rest.length - 1)]?.id ?? null);
  };

  const remove = async (d: Draft) => {
    const filled = d.content.trim() || d.title.trim();
    if (filled && !window.confirm(`Fermer le brouillon « ${draftLabel(d)} » ?\n\nSon contenu sera perdu.`)) return;
    try {
      await draftsApi.remove(d.id);
      drop(d.id);
    } catch (e) {
      notify((e as Error).message, true);
    }
  };

  const run = async (action: () => Promise<void>) => {
    setBusy(true);
    try {
      await action();
    } catch (e) {
      notify((e as Error).message, true);
    } finally {
      setBusy(false);
    }
  };

  const analyze = () => run(async () => {
    if (!active) return;
    await flush();
    const result = await draftsApi.analyze(active.id);
    setProposals((prev) => ({ ...prev, [active.id]: result }));
  });

  const apply = () => run(async () => {
    if (!active || !proposal) return;
    await flush();
    const result = await draftsApi.apply(active.id, proposal, close);
    const parts = [
      result.note && "note créée",
      result.created_tasks.length && `${result.created_tasks.length} tâche(s) créée(s)`,
    ].filter(Boolean);
    notify(`Brouillon transformé : ${parts.join(" · ")}`);
    if (result.closed) drop(active.id);
    else setProposals(({ [active.id]: _, ...others }) => others);
    if (result.note) onOpen(result.note);
  });

  const setProposal = (patch: Partial<DraftProposal>) =>
    active && proposal && setProposals((prev) => ({ ...prev, [active.id]: { ...proposal, ...patch } }));
  const setNote = (patch: Partial<NonNullable<DraftProposal["note"]>>) =>
    proposal?.note && setProposal({ note: { ...proposal.note, ...patch } });
  const setTask = (i: number, patch: Partial<DraftProposal["tasks"][number]>) =>
    proposal && setProposal({ tasks: proposal.tasks.map((t, j) => (i === j ? { ...t, ...patch } : t)) });

  if (!list) return <div className="loading">Chargement…</div>;

  const accepted = proposal ? Number(Boolean(proposal.note?.accepted)) + proposal.tasks.filter((t) => t.accepted).length : 0;

  return (
    <div className="drafts-view">
      <div className="draft-tabs" role="tablist">
        {list.map((d) => (
          <div key={d.id} role="tab" aria-selected={d.id === active?.id}
            className={`draft-tab ${d.id === active?.id ? "active" : ""}`} title={draftLabel(d)}
            onClick={() => select(d.id)}
            onAuxClick={(e) => { if (e.button === 1) remove(d); }}>
            <span className="nav-label">{draftLabel(d)}</span>
            {proposals[d.id] && <span className="draft-dot" title="Proposition de l'IA en attente" />}
            <button className="draft-close" title="Fermer le brouillon" aria-label="Fermer le brouillon"
              onClick={(e) => { e.stopPropagation(); remove(d); }}>×</button>
          </div>
        ))}
        <button className="draft-add" title="Nouveau brouillon" aria-label="Nouveau brouillon" onClick={add}>+</button>
      </div>

      {!active && (
        <div className="empty">
          <p>Aucun brouillon. Écrivez au fil de l'eau, puis laissez l'IA en faire une note ou des tâches.</p>
          <button className="primary" onClick={add}>Nouveau brouillon</button>
        </div>
      )}

      {active && (
        <div className={`draft-body ${proposal ? "with-proposal" : ""}`}>
          <div className="draft-editor">
            <div className="draft-toolbar">
              <input className="draft-title" placeholder={draftLabel({ ...active, title: "" })} value={active.title}
                onChange={(e) => edit(active.id, { title: e.target.value })} />
              <span className="save-status">{saving ? "enregistrement…" : "enregistré"}</span>
              <button className="primary" disabled={busy || !config.llm_enabled || !active.content.trim()}
                onClick={analyze} title={config.llm_enabled ? "Proposer une note ou des tâches" : "API LLM non configurée (.env)"}>
                {busy && !proposal ? "Analyse…" : proposal ? "Relancer l'analyse" : "Analyser"}
              </button>
            </div>
            <MarkdownEditor key={active.id} initial={active.content} files={files} preview
              onChange={(content) => edit(active.id, { content })}
              onOpenLink={(name) => api.resolve(name)
                .then((r) => r.path ? onOpen(r.path) : r.document && navigate(documentHref(r.document)))
                .catch((e) => notify(e.message, true))}
              onCreateTask={async (title) => (await api.createTask({ title })).id} />
          </div>

          {proposal && (
            <aside className="draft-proposal">
              <div className="suggestion-head">
                <h3>Proposition de l'IA</h3>
                <button className="ghost mini" title="Ignorer la proposition"
                  onClick={() => setProposals(({ [active.id]: _, ...others }) => others)}>×</button>
              </div>
              {proposal.comment && <p className="muted small">{proposal.comment}</p>}

              <h4>Note</h4>
              {!proposal.note && <p className="muted small">Aucune note proposée.</p>}
              {proposal.note && (
                <div className={proposal.note.accepted ? "" : "rejected"}>
                  <label className="check">
                    <input type="checkbox" checked={proposal.note.accepted}
                      onChange={(e) => setNote({ accepted: e.target.checked })} />
                    Créer la note
                  </label>
                  <div className="grid-form">
                    <label>Type
                      <select value={proposal.note.type} onChange={(e) => setNote({ type: e.target.value })}>
                        {NOTE_KINDS.map((k) => <option key={k} value={k}>{TYPE_LABELS[k] ?? k}</option>)}
                      </select>
                    </label>
                    <label>Titre<input value={proposal.note.title} onChange={(e) => setNote({ title: e.target.value })} /></label>
                    {proposal.note.type !== "projet" && (
                      <label className="wide">Projets
                        <ProjectsSelect projects={projects} value={proposal.note.projects ?? []}
                          onChange={(ids) => setNote({ projects: ids })} onCreated={addProject} notify={notify} />
                      </label>
                    )}
                    <label className="wide">Contenu
                      <textarea rows={10} value={proposal.note.content} onChange={(e) => setNote({ content: e.target.value })} />
                    </label>
                  </div>
                </div>
              )}

              <h4>Tâches <span className="count">{proposal.tasks.length}</span></h4>
              {proposal.tasks.length === 0 && <p className="muted small">Aucune.</p>}
              {proposal.tasks.map((t, i) => (
                <div key={i} className={`proposal-card ${t.accepted ? "" : "rejected"}`}>
                  <input type="checkbox" checked={t.accepted} onChange={(e) => setTask(i, { accepted: e.target.checked })} />
                  <input className="grow" value={t.title} onChange={(e) => setTask(i, { title: e.target.value })} />
                  <ProjectSelect projects={projects} value={t.project ?? ""} onChange={(id) => setTask(i, { project: id || null })}
                    onCreated={addProject} notify={notify} />
                  <select value={t.status} onChange={(e) => setTask(i, { status: e.target.value })}>
                    {config.statuses.map((s) => <option key={s} value={s}>{STATUS_LABELS[s]}</option>)}
                  </select>
                  <select value={t.priority} onChange={(e) => setTask(i, { priority: e.target.value })}>
                    {config.priorities.map((p) => <option key={p}>{p}</option>)}
                  </select>
                  <input type="date" value={t.due ?? ""} onChange={(e) => setTask(i, { due: e.target.value || null })} />
                  {t.duplicate_of && <span className="tag" title="Une tâche ouverte couvre déjà cette action">
                    déjà : {t.duplicate_of}</span>}
                  {t.description && <div className="muted small full-row">{t.description}</div>}
                </div>
              ))}

              <label className="check draft-close-option">
                <input type="checkbox" checked={close} onChange={(e) => setClose(e.target.checked)} />
                Fermer le brouillon après création
              </label>
              <div className="row">
                <button className="primary" disabled={busy || accepted === 0} onClick={apply}>
                  {busy ? "Création…" : "Créer"}
                </button>
                <span className="muted small">Rien n'est écrit dans le vault avant validation.</span>
              </div>
            </aside>
          )}
        </div>
      )}
    </div>
  );
}
