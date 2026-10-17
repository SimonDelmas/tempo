import { useEffect, useState } from "react";
import { api, Config, InboxItem, Proposal, STATUS_LABELS, Task } from "../api";

type Props = {
  config: Config;
  revision: number;
  onOpen: (path: string) => void;
  notify: (message: string, error?: boolean) => void;
};

const SUMMARY_PROMPT = `Résume chaque mail/conversation au format suivant, séparés par "---" :
Source: mail|teams
Date: AAAA-MM-JJ
Sujet:
Participants:
Résumé: (3-5 lignes, faits et décisions)
Actions pour moi:
Actions attendues des autres:
Questions en suspens:`;

const csv = (values: string[]) => values.join(", ");
const fromCsv = (text: string) => text.split(",").map((s) => s.trim()).filter(Boolean);

export default function InboxView({ config, revision, onOpen, notify }: Props) {
  const [items, setItems] = useState<InboxItem[]>([]);
  const [tasks, setTasks] = useState<Task[]>([]);
  const [selected, setSelected] = useState<string | null>(null);
  const [text, setText] = useState("");
  const [proposal, setProposal] = useState<Proposal | null>(null);
  const [busy, setBusy] = useState(false);

  useEffect(() => {
    api.inbox().then((list) => {
      setItems(list);
      if (selected && !list.some((i) => i.path === selected)) {
        setSelected(null);
        setProposal(null);
      }
    }).catch(() => {});
    api.tasks().then(setTasks).catch(() => {});
  }, [revision]); // eslint-disable-line react-hooks/exhaustive-deps

  const item = items.find((i) => i.path === selected);
  const taskTitle = (id: string) => tasks.find((t) => t.id.toUpperCase() === id.toUpperCase())?.title ?? "";

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

  const add = () => run(async () => {
    const { paths } = await api.inboxAdd(text);
    setText("");
    notify(`${paths.length} résumé(s) ajouté(s) à l'inbox`);
    setItems(await api.inbox());
    setSelected(paths[0]);
    setProposal(null);
  });

  const analyze = () => run(async () => {
    if (!selected) return;
    setProposal(await api.inboxAnalyze(selected));
  });

  const apply = () => run(async () => {
    if (!selected || !proposal) return;
    const result = await api.inboxApply(selected, proposal);
    const parts = [
      result.created_tasks.length && `${result.created_tasks.length} tâche(s) créée(s)`,
      result.updated_tasks.length && `${result.updated_tasks.length} mise(s) à jour`,
    ].filter(Boolean);
    notify(`Échange enregistré${parts.length ? " · " + parts.join(" · ") : ""}`);
    setProposal(null);
    setSelected(null);
    setItems(await api.inbox());
    onOpen(result.echange);
  });

  const discard = () => run(async () => {
    if (!selected || !window.confirm("Ignorer ce résumé ? (déplacé dans .trash)")) return;
    await api.inboxDiscard(selected);
    setSelected(null);
    setProposal(null);
    setItems(await api.inbox());
  });

  const setEchange = (patch: Partial<Proposal["echange"]>) =>
    proposal && setProposal({ ...proposal, echange: { ...proposal.echange, ...patch } });
  const setNewTask = (i: number, patch: Partial<Proposal["new_tasks"][number]>) =>
    proposal && setProposal({ ...proposal, new_tasks: proposal.new_tasks.map((t, j) => (i === j ? { ...t, ...patch } : t)) });
  const setUpdate = (i: number, patch: Partial<Proposal["task_updates"][number]>) =>
    proposal && setProposal({ ...proposal, task_updates: proposal.task_updates.map((u, j) => (i === j ? { ...u, ...patch } : u)) });

  return (
    <div className="page inbox-view">
      <header className="page-header"><h2>Inbox — mails et Teams</h2></header>

      <div className="inbox-layout">
        <div className="inbox-left">
          <textarea rows={7} placeholder="Collez ici un ou plusieurs résumés (séparés par ---)" value={text}
            onChange={(e) => setText(e.target.value)} />
          <div className="row">
            <button className="primary" disabled={!text.trim() || busy} onClick={add}>Ajouter à l'inbox</button>
            <button className="ghost" onClick={() => navigator.clipboard.writeText(SUMMARY_PROMPT)
              .then(() => notify("Prompt de résumé copié"))}>Copier le prompt de résumé</button>
          </div>
          <h4>À traiter <span className="count">{items.length}</span></h4>
          {items.length === 0 && <p className="muted small">Inbox vide.</p>}
          <ul className="inbox-items">
            {items.map((i) => (
              <li key={i.path} className={i.path === selected ? "active" : ""}
                onClick={() => { setSelected(i.path); setProposal(null); }}>
                <div className="small muted">{i.path.replace("00-inbox/", "")}</div>
                <div>{i.preview}</div>
              </li>
            ))}
          </ul>
        </div>

        <div className="inbox-right">
          {!item && <p className="muted">Sélectionnez un résumé à traiter.</p>}
          {item && (
            <>
              <pre className="raw">{item.content}</pre>
              <div className="row">
                <button className="primary" disabled={busy || !config.llm_enabled} onClick={analyze}
                  title={config.llm_enabled ? "" : "API LLM non configurée (.env)"}>
                  {busy && !proposal ? "Analyse…" : proposal ? "Relancer l'analyse" : "Analyser avec l'IA"}
                </button>
                <button className="ghost" disabled={busy} onClick={discard}>Ignorer</button>
              </div>
              {!config.llm_enabled && <p className="warning">API LLM non configurée : renseignez VIBE_BASE_URL et VIBE_API_KEY dans .env.</p>}
            </>
          )}

          {proposal && (
            <div className="proposal">
              {proposal.duplicate_of && (
                <p className="warning">Ce résumé semble déjà importé : {proposal.duplicate_of}</p>
              )}
              <h4>Note d'échange</h4>
              <div className="grid-form">
                <label>Source
                  <select value={proposal.echange.source} onChange={(e) => setEchange({ source: e.target.value })}>
                    <option value="mail">mail</option><option value="teams">teams</option><option value="autre">autre</option>
                  </select>
                </label>
                <label>Date<input type="date" value={proposal.echange.date} onChange={(e) => setEchange({ date: e.target.value })} /></label>
                <label className="wide">Sujet<input value={proposal.echange.subject} onChange={(e) => setEchange({ subject: e.target.value })} /></label>
                <label className="wide">Participants<input value={csv(proposal.echange.participants)} onChange={(e) => setEchange({ participants: fromCsv(e.target.value) })} /></label>
                <label>Projets<input value={csv(proposal.echange.projects)} onChange={(e) => setEchange({ projects: fromCsv(e.target.value) })} /></label>
                <label>Fil<input value={proposal.echange.thread} onChange={(e) => setEchange({ thread: e.target.value })} /></label>
                <label className="wide">Résumé<textarea rows={5} value={proposal.echange.summary} onChange={(e) => setEchange({ summary: e.target.value })} /></label>
              </div>

              <h4>Nouvelles tâches <span className="count">{proposal.new_tasks.length}</span></h4>
              {proposal.new_tasks.length === 0 && <p className="muted small">Aucune.</p>}
              {proposal.new_tasks.map((t, i) => (
                <div key={i} className={`proposal-card ${t.accepted ? "" : "rejected"}`}>
                  <input type="checkbox" checked={t.accepted} onChange={(e) => setNewTask(i, { accepted: e.target.checked })} />
                  <input className="grow" value={t.title} onChange={(e) => setNewTask(i, { title: e.target.value })} />
                  <input placeholder="projet" value={t.project ?? ""} onChange={(e) => setNewTask(i, { project: e.target.value || null })} />
                  <select value={t.status} onChange={(e) => setNewTask(i, { status: e.target.value })}>
                    {config.statuses.map((s) => <option key={s} value={s}>{STATUS_LABELS[s]}</option>)}
                  </select>
                  <select value={t.priority} onChange={(e) => setNewTask(i, { priority: e.target.value })}>
                    {config.priorities.map((p) => <option key={p}>{p}</option>)}
                  </select>
                  <input type="date" value={t.due ?? ""} onChange={(e) => setNewTask(i, { due: e.target.value || null })} />
                </div>
              ))}

              <h4>Mises à jour de tâches <span className="count">{proposal.task_updates.length}</span></h4>
              {proposal.task_updates.length === 0 && <p className="muted small">Aucune.</p>}
              {proposal.task_updates.map((u, i) => (
                <div key={i} className={`proposal-card ${u.accepted ? "" : "rejected"}`}>
                  <input type="checkbox" checked={u.accepted} onChange={(e) => setUpdate(i, { accepted: e.target.checked })} />
                  <strong title={taskTitle(u.task_id)}>{u.task_id}</strong>
                  <span className="muted small truncate">{taskTitle(u.task_id)}</span>
                  <select value={u.status ?? ""} onChange={(e) => setUpdate(i, { status: e.target.value || null })}>
                    <option value="">(statut inchangé)</option>
                    {config.statuses.map((s) => <option key={s} value={s}>{STATUS_LABELS[s]}</option>)}
                  </select>
                  <input className="grow" value={u.note} onChange={(e) => setUpdate(i, { note: e.target.value })} />
                </div>
              ))}

              <h4>Journal du {proposal.echange.date}</h4>
              <input className="full" value={proposal.journal_line}
                onChange={(e) => setProposal({ ...proposal, journal_line: e.target.value })} />

              <div className="row">
                <button className="primary" disabled={busy} onClick={apply}>Appliquer</button>
              </div>
            </div>
          )}
        </div>
      </div>
    </div>
  );
}
