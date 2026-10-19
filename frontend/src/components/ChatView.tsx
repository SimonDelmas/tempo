import { useEffect, useRef, useState } from "react";
import { api, ChatStep, Config } from "../api";
import { ChatEntry, Conversation } from "../chats";
import Markdown from "./Markdown";

type Props = {
  config: Config;
  conversation: Conversation | null;
  /** ajoute un message à une conversation (créée si id est null) et renvoie son id */
  onAppend: (id: string | null, entry: ChatEntry) => string;
  onOpen: (path: string) => void;
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
};

const stepLabel = (step: ChatStep) =>
  `${TOOL_LABELS[step.tool] ?? step.tool} ${Object.values(step.args).filter(Boolean).join(", ")}`.trim();

export default function ChatView({ config, conversation, onAppend, onOpen }: Props) {
  const [draft, setDraft] = useState("");
  const [pending, setPending] = useState<string | null>(null);
  const bottom = useRef<HTMLDivElement>(null);
  const messages = conversation?.messages ?? [];
  const busy = pending !== null && pending === (conversation?.id ?? null);

  useEffect(() => { bottom.current?.scrollIntoView({ block: "end" }); }, [messages.length, busy]);
  // la vue reste montée d'une conversation à l'autre pour garder la requête en cours
  useEffect(() => setDraft(""), [conversation?.id]);

  const send = async (text: string) => {
    const question = text.trim();
    if (!question || pending) return;
    setDraft("");
    const history = [...messages, { role: "user" as const, content: question }]
      .filter((m) => !m.error)
      .map(({ role, content }) => ({ role, content }));
    const id = onAppend(conversation?.id ?? null, { role: "user", content: question });
    setPending(id);
    try {
      const reply = await api.chat(history);
      onAppend(id, { role: "assistant", content: reply.answer, steps: reply.steps, links: reply.links });
    } catch (e) {
      onAppend(id, { role: "assistant", content: (e as Error).message, error: true });
    } finally {
      setPending(null);
    }
  };

  return (
    <div className="chat-view">
      <div className="chat-messages">
        {messages.length === 0 && (
          <div className="chat-empty">
            <h2>Demander à l'agent</h2>
            <p className="muted">
              L'agent consulte vos notes et vos tâches (lecture seule) et cite ses sources en [[liens]].
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
        {busy && <div className="chat-message assistant thinking">L'agent consulte vos notes…</div>}
        <div ref={bottom} />
      </div>

      <div className="chat-input">
        <textarea rows={2} value={draft} disabled={!config.llm_enabled}
          placeholder="Poser une question sur vos notes et tâches…  (Entrée : envoyer, Maj+Entrée : retour à la ligne)"
          onChange={(e) => setDraft(e.target.value)}
          onKeyDown={(e) => {
            if (e.key === "Enter" && !e.shiftKey) {
              e.preventDefault();
              send(draft);
            }
          }} />
        <button className="primary" disabled={!draft.trim() || !!pending || !config.llm_enabled}
          onClick={() => send(draft)}>
          Envoyer
        </button>
      </div>
    </div>
  );
}
