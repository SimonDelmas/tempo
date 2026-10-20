import { useEffect, useRef, useState } from "react";
import { api, ChatProposal, ChatStep, Config, STATUS_LABELS, TYPE_LABELS } from "../api";
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
  propose_note_create: "Proposition de note",
  propose_note_edit: "Proposition de modification",
  propose_note_append: "Proposition d'ajout",
  propose_note_meta: "Proposition de propriétés",
  propose_note_delete: "Proposition de suppression",
  propose_link: "Proposition de lien",
  propose_unlink: "Proposition de retrait de lien",
};

const stepLabel = (step: ChatStep) => {
  const args = typeof step.args === "string" ? step.args
    : Object.values(step.args).filter((v) => typeof v === "string" && v).join(", ");
  return `${TOOL_LABELS[step.tool] ?? step.tool} ${args}`.trim();
};

/** ligne d'aperçu d'une modification : retirée (-), ajoutée (+) ou séparateur (…) */
type DiffLine = { sign: "-" | "+" | "…"; text: string };
const MAX_PREVIEW_LINES = 14;

const lines = (sign: "-" | "+", text: string): DiffLine[] =>
  text.replace(/\n+$/, "").split("\n").map((line) => ({ sign, text: line }));

const formatValue = (value: unknown): string =>
  value == null ? "∅" : Array.isArray(value) ? `[${value.map(formatValue).join(", ")}]`
    : typeof value === "string" ? value : JSON.stringify(value);

function describe(p: ChatProposal): { title: string; detail?: string; diff?: DiffLine[] } {
  switch (p.kind) {
    case "create_task": {
      const detail = [p.project && `projet ${p.project}`, p.priority && `priorité ${p.priority}`,
        p.due && `échéance ${p.due}`].filter(Boolean).join(" · ");
      return { title: `Créer la tâche « ${p.title} »`, detail: [detail, p.description].filter(Boolean).join(" — ") };
    }
    case "update_task": {
      const detail = [p.new_title && `titre → « ${p.new_title} »`, p.status && `statut → ${STATUS_LABELS[p.status] ?? p.status}`,
        p.priority && `priorité → ${p.priority}`, p.due && `échéance → ${p.due}`, p.project && `projet → ${p.project}`,
        p.iteration && `planifiée en ${p.iteration}`, p.note && `« ${p.note} »`]
        .filter(Boolean).join(" · ");
      return { title: `Mettre à jour ${p.task_id} · ${p.title}`, detail };
    }
    case "journal":
      return { title: "Ajouter au journal du jour", detail: p.line };
    case "create_note":
      return { title: `Créer la note « ${p.title} »`,
        detail: [TYPE_LABELS[p.type] ?? p.type, p.date].filter(Boolean).join(" · "),
        diff: p.content.trim() ? lines("+", p.content.trim()) : undefined };
    case "edit_note":
      return { title: `Modifier « ${p.title} »`,
        diff: p.edits.flatMap((e, i) => [...(i > 0 ? [{ sign: "…" as const, text: "" }] : []),
          ...lines("-", e.find), ...(e.replace ? lines("+", e.replace) : [])]) };
    case "append_note":
      return { title: `Compléter « ${p.title} »`, detail: p.section ? `section ${p.section}` : "à la fin de la note",
        diff: lines("+", p.text) };
    case "update_meta":
      return { title: `Modifier les propriétés de « ${p.title} »`,
        diff: [
          ...Object.entries(p.set).flatMap(([key, value]) => [
            ...(p.before[key] != null ? [{ sign: "-" as const, text: `${key}: ${formatValue(p.before[key])}` }] : []),
            { sign: "+" as const, text: `${key}: ${formatValue(value)}` }]),
          ...p.remove.map((key) => ({ sign: "-" as const, text: `${key}: ${formatValue(p.before[key])}` })),
        ] };
    case "delete_note":
      return { title: `Supprimer « ${p.title} »`,
        detail: [p.backlinks.length ? `mentionnée dans ${p.backlinks.join(", ")} (liens cassés)` : "",
          "récupérable dans .trash"].filter(Boolean).join(" · ") };
    case "link":
      return { title: `Lier « ${p.title} » à ${p.link}`,
        detail: p.field ? `propriété ${p.field}` : `section ${p.section ?? "## Liens"}` };
    case "unlink":
      return { title: `Retirer de « ${p.title} » les liens vers ${p.link}` };
  }
}

function Diff({ diff }: { diff: DiffLine[] }) {
  const [open, setOpen] = useState(false);
  const shown = open ? diff : diff.slice(0, MAX_PREVIEW_LINES);
  return (
    <div className="proposal-diff">
      {shown.map((line, i) => (
        <div key={i} className={`diff-line ${line.sign === "-" ? "removed" : line.sign === "+" ? "added" : "gap"}`}>
          <span className="diff-sign">{line.sign}</span>{line.text || " "}
        </div>
      ))}
      {diff.length > shown.length && (
        <button className="link small" onClick={() => setOpen(true)}>… {diff.length - shown.length} lignes de plus</button>
      )}
    </div>
  );
}

function ProposalCard({ item, onApply, onIgnore, onOpen }: {
  item: ProposalState; onApply: () => Promise<void>; onIgnore: () => void; onOpen: (path: string) => void;
}) {
  const [busy, setBusy] = useState(false);
  const { title, detail, diff } = describe(item.proposal);
  return (
    <div className={`proposal-item ${item.state}`}>
      <div className="grow">
        <div className="proposal-title">{title}</div>
        {detail && <div className="muted small">{detail}</div>}
        {diff && diff.length > 0 && item.state !== "ignored" && <Diff diff={diff} />}
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
      setProposal(index, j, { ...item, state: "applied", path: result.path ?? undefined, message: result.message });
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
              L'agent consulte vos notes et vos tâches, cite ses sources en [[liens]] et peut proposer de créer
              ou modifier des tâches, des notes et leurs liens : rien n'est écrit avant votre validation.
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
