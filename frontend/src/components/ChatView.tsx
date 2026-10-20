import { useEffect, useRef, useState } from "react";
import { api, ChatProposal, ChatStep, Config, STATUS_LABELS } from "../api";
import { ChatEntry, Conversation, LiveReply, ProposalState } from "../chats";
import Markdown from "./Markdown";

type Props = {
  config: Config;
  conversation: Conversation | null;
  /** réponse en cours de diffusion pour cette conversation (tenue par l'application) */
  live: LiveReply | null;
  /** envoie une question dans la conversation (créée si elle n'existe pas encore) */
  onSend: (question: string) => void;
  /** remplace le message n° index d'une conversation */
  onReplace: (id: string, index: number, entry: ChatEntry) => void;
  onOpen: (path: string) => void;
  notify: (message: string, error?: boolean) => void;
};

const SUGGESTIONS = [
  "Quelles tâches sont en retard ou bloquées ?",
  "Qu'ai-je fait cette semaine ?",
  "Résume la dernière réunion du projet SAP",
  "Qu'est-ce que j'attends des autres en ce moment ?",
];

const TOOL_LABELS: Record<string, string> = {
  search_notes: "Recherche",
  read_note: "Lecture",
  list_notes: "Notes",
  list_tasks: "Tâches",
  get_task: "Tâche",
  task_events: "Historique des tâches",
  propose_task: "Proposition de tâche",
  propose_task_update: "Proposition de mise à jour",
  propose_journal_line: "Proposition de journal",
};

const stepLabel = (step: ChatStep) =>
  `${TOOL_LABELS[step.tool] ?? step.tool} ${Object.values(step.args).filter(Boolean).join(", ")}`.trim();

function describe(p: ChatProposal): { title: string; detail: string } {
  if (p.kind === "create_task") {
    const detail = [p.project && `projet ${p.project}`, p.priority && `priorité ${p.priority}`,
      p.due && `échéance ${p.due}`].filter(Boolean).join(" · ");
    return { title: `Créer la tâche « ${p.title} »`, detail: [detail, p.description].filter(Boolean).join(" — ") };
  }
  if (p.kind === "update_task") {
    const detail = [p.status && `statut → ${STATUS_LABELS[p.status] ?? p.status}`,
      p.priority && `priorité → ${p.priority}`, p.due && `échéance → ${p.due}`, p.note && `« ${p.note} »`]
      .filter(Boolean).join(" · ");
    return { title: `Mettre à jour ${p.task_id} · ${p.title}`, detail };
  }
  return { title: "Ajouter au journal du jour", detail: p.line };
}

function ProposalCard({ item, onApply, onIgnore, onOpen }: {
  item: ProposalState; onApply: () => Promise<void>; onIgnore: () => void; onOpen: (path: string) => void;
}) {
  const [busy, setBusy] = useState(false);
  const { title, detail } = describe(item.proposal);
  return (
    <div className={`proposal-item ${item.state}`}>
      <div className="grow">
        <div className="proposal-title">{title}</div>
        {detail && <div className="muted small">{detail}</div>}
      </div>
      {item.state === "pending" && (
        <>
          <button className="primary" disabled={busy}
            onClick={() => { setBusy(true); onApply().finally(() => setBusy(false)); }}>
            {busy ? "…" : "Appliquer"}
          </button>
          <button className="ghost" disabled={busy} onClick={onIgnore}>Ignorer</button>
        </>
      )}
      {item.state === "applied" && (
        <button className="link small" onClick={() => item.path && onOpen(item.path)}>✓ {item.message ?? "Appliqué"}</button>
      )}
      {item.state === "ignored" && <span className="muted small">Ignorée</span>}
    </div>
  );
}

export default function ChatView({ config, conversation, live, onSend, onReplace, onOpen, notify }: Props) {
  const [draft, setDraft] = useState("");
  const bottom = useRef<HTMLDivElement>(null);
  const messages = conversation?.messages ?? [];

  useEffect(() => { bottom.current?.scrollIntoView({ block: "end" }); },
    [conversation?.id, messages.length, live?.text.length, live?.steps.length]);
  useEffect(() => setDraft(""), [conversation?.id]);

  const send = (text: string) => {
    const question = text.trim();
    if (!question || live) return;
    setDraft("");
    onSend(question);
  };

  const setProposal = (index: number, j: number, item: ProposalState) => {
    if (!conversation) return;
    const entry = conversation.messages[index];
    const proposals = [...(entry.proposals ?? [])];
    proposals[j] = item;
    onReplace(conversation.id, index, { ...entry, proposals });
  };

  const apply = async (index: number, j: number, item: ProposalState) => {
    try {
      const result = await api.chatApply(item.proposal);
      setProposal(index, j, { ...item, state: "applied", path: result.path, message: result.message });
      notify(result.message);
    } catch (e) {
      notify((e as Error).message, true);
    }
  };

  return (
    <div className="chat-view">
      <div className="chat-messages">
        {messages.length === 0 && !live && (
          <div className="chat-empty">
            <h2>Demander à l'agent</h2>
            <p className="muted">
              L'agent consulte vos notes et vos tâches, cite ses sources en [[liens]] et peut proposer des tâches,
              des mises à jour ou des lignes de journal, que vous validez une à une.
            </p>
            {!config.llm_enabled && (
              <p className="warning">API LLM non configurée : renseignez VIBE_BASE_URL et VIBE_API_KEY dans .env.</p>
            )}
            <div className="chat-suggestions">
              {SUGGESTIONS.map((s) => (
                <button key={s} disabled={!config.llm_enabled} onClick={() => send(s)}>{s}</button>
              ))}
            </div>
          </div>
        )}
        {messages.map((m, i) => (
          <div key={i} className={`chat-message ${m.role} ${m.error ? "error" : ""}`}>
            {m.role === "user" ? <p>{m.content}</p> : <Markdown text={m.content} links={m.links ?? {}} onOpen={onOpen} />}
            {!!m.proposals?.length && (
              <div className="proposal-list">
                {m.proposals.map((item, j) => (
                  <ProposalCard key={j} item={item} onOpen={onOpen}
                    onApply={() => apply(i, j, item)}
                    onIgnore={() => setProposal(i, j, { ...item, state: "ignored" })} />
                ))}
              </div>
            )}
            {!!m.steps?.length && (
              <details className="chat-steps">
                <summary>{m.steps.length} consultation{m.steps.length > 1 ? "s" : ""} des notes</summary>
                <ul>
                  {m.steps.map((s, j) => (
                    <li key={j}>{stepLabel(s)}{s.summary && <span className="muted"> — {s.summary}</span>}</li>
                  ))}
                </ul>
              </details>
            )}
          </div>
        ))}
        {live && (
          <div className="chat-message assistant">
            {live.text
              ? <Markdown text={live.text} links={null} onOpen={onOpen} />
              : <p className="thinking">L'agent consulte vos notes…</p>}
            {live.steps.length > 0 && (
              <ul className="chat-live-steps">
                {live.steps.map((s, j) => <li key={j}>{stepLabel(s)}</li>)}
              </ul>
            )}
          </div>
        )}
        <div ref={bottom} />
      </div>

      <div className="chat-input">
        <textarea rows={2} value={draft} disabled={!config.llm_enabled}
          placeholder="Poser une question ou demander une action…  (Entrée : envoyer, Maj+Entrée : retour à la ligne)"
          onChange={(e) => setDraft(e.target.value)}
          onKeyDown={(e) => {
            if (e.key === "Enter" && !e.shiftKey) {
              e.preventDefault();
              send(draft);
            }
          }} />
        <button className="primary" disabled={!draft.trim() || !!live || !config.llm_enabled}
          onClick={() => send(draft)}>
          Envoyer
        </button>
      </div>
    </div>
  );
}
