import { useEffect, useRef, useState } from "react";
import { AccountCategory, api, CATEGORY_LABELS, ChatProposal, ChatStep, Citation, Config, Hours, Project, STATUS_LABELS,
  TYPE_LABELS } from "../api";
import { ChatEntry, Conversation, LiveReply, ProposalState } from "../chats";
import Markdown, { citationTitle, openCitation } from "./Markdown";
import { ProjectsSelect, useProjects } from "./ProjectSelect";

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
  grep_notes: "Recherche exacte",
  find_notes: "Recherche de notes",
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
  task_board: "Board",
  task_backlog: "Backlog",
  list_projects: "Projets",
  list_accounts: "Comptes",
  get_imputations: "Imputations",
  suggest_imputations: "Répartition des imputations",
  imputation_tracking: "Saisies des imputations",
  list_inbox: "Inbox",
  list_drafts: "Brouillons",
  propose_task_move: "Proposition de déplacement",
  propose_project_create: "Proposition de projet",
  propose_project_move: "Proposition d'ordre des projets",
  propose_project_account: "Proposition de compte du projet",
  propose_account_create: "Proposition de compte",
  propose_account_update: "Proposition de modification du compte",
  propose_imputations: "Proposition d'imputations",
  propose_saisie: "Proposition de saisie",
  propose_inbox_add: "Proposition d'ajout à l'inbox",
  propose_inbox_discard: "Proposition d'écarter",
  propose_inbox_process: "Analyse de l'inbox",
  propose_weekly_report: "Proposition de rapport",
  propose_task_report: "Proposition de rapport de tâche",
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
        detail: [TYPE_LABELS[p.type] ?? p.type, p.date, p.projects?.length && `projets ${p.projects.join(", ")}`]
          .filter(Boolean).join(" · "),
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
    case "move_task":
      return { title: `Déplacer ${p.task_id} · ${p.title}`,
        detail: p.before ? `avant ${p.before}` : "en fin de backlog" };
    case "create_project":
      return { title: `Créer le projet « ${p.title} »` };
    case "move_project":
      return { title: `Déplacer le projet « ${p.title} »`, detail: p.before ? `avant ${p.before}` : "en fin de backlog" };
    case "set_project_account":
      return { title: p.compte ? `Rattacher le projet « ${p.title} » au compte ${p.compte}`
        : `Détacher le projet « ${p.title} » de son compte` };
    case "create_account":
      return { title: `Créer le compte ${p.alias} · ${p.title}`,
        detail: [CATEGORY_LABELS[p.categorie as AccountCategory] ?? p.categorie, `${p.days} j`, p.description]
          .filter(Boolean).join(" · ") };
    case "update_account": {
      const after: Record<string, unknown> = { title: p.title, categorie: p.categorie, days: p.days, description: p.description };
      return { title: `Modifier le compte ${p.alias}`,
        diff: Object.keys(p.before).flatMap((key) => [
          { sign: "-" as const, text: `${key}: ${formatValue(p.before[key])}` },
          { sign: "+" as const, text: `${key}: ${formatValue(after[key])}` }]) };
    }
    case "save_imputations": {
      const day = (hours: Hours, date: string) =>
        Object.entries(hours[date] ?? {}).map(([alias, h]) => `${alias} ${h} h`).join(", ") || "rien";
      const dates = [...new Set([...Object.keys(p.before), ...Object.keys(p.hours)])].sort()
        .filter((date) => day(p.before, date) !== day(p.hours, date));
      return { title: `Imputations de la semaine ${p.week}`, detail: p.comment || undefined,
        diff: dates.flatMap((date) => [
          ...(p.before[date] ? [{ sign: "-" as const, text: `${date} : ${day(p.before, date)}` }] : []),
          { sign: "+" as const, text: `${date} : ${day(p.hours, date)}` }]) };
    }
    case "saisie":
      return { title: p.done ? `Enregistrer la saisie des imputations du ${p.date}` : `Retirer la saisie du ${p.date}` };
    case "inbox_add":
      return { title: "Ajouter à l'inbox", diff: lines("+", p.text) };
    case "inbox_discard":
      return { title: "Écarter un élément de l'inbox", detail: p.preview || p.path };
    case "inbox_apply": {
      const { echange, new_tasks, task_updates, journal_line, duplicate_of } = p.proposal;
      return { title: `Traiter l'échange « ${echange.subject} »`,
        detail: [echange.source, echange.date, duplicate_of && `doublon possible de [[${duplicate_of}]]`].filter(Boolean).join(" · "),
        diff: [
          { sign: "+", text: `échange : ${echange.subject}` },
          ...new_tasks.filter((t) => t.accepted).map((t) => ({ sign: "+" as const, text: `tâche : ${t.title}` })),
          ...task_updates.filter((u) => u.accepted).map((u) => ({ sign: "+" as const,
            text: [u.task_id, u.status && `statut → ${STATUS_LABELS[u.status] ?? u.status}`, u.note && `« ${u.note} »`]
              .filter(Boolean).join(" · ") })),
          ...(journal_line ? [{ sign: "+" as const, text: `journal : ${journal_line}` }] : []),
        ] };
    }
    case "weekly_report":
      return { title: `Générer le rapport hebdomadaire ${p.week}`, detail: p.extra ? `à intégrer : ${p.extra}` : undefined };
    case "task_report":
      return { title: `Générer le rapport d'avancement ${p.task_id} · ${p.title}` };
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

function ProposalCard({ item, projects, onApply, onIgnore, onEdit, onOpen, onProjectCreated, notify }: {
  item: ProposalState; projects: Project[]; onApply: () => Promise<void>; onIgnore: () => void;
  /** modifie la proposition avant validation */
  onEdit: (proposal: ChatProposal) => void;
  onOpen: (path: string) => void; onProjectCreated: (project: Project) => void;
  notify: (message: string, error?: boolean) => void;
}) {
  const [busy, setBusy] = useState(false);
  const { title, detail, diff } = describe(item.proposal);
  const p = item.proposal;
  return (
    <div className={`proposal-item ${item.state}`}>
      <div className="grow">
        <div className="proposal-title">{title}</div>
        {detail && <div className="muted small">{detail}</div>}
        {p.kind === "create_note" && p.type !== "projet" && item.state === "pending" && (
          <label className="proposal-field small">Projets
            <ProjectsSelect projects={projects} value={p.projects ?? []} onChange={(ids) => onEdit({ ...p, projects: ids })}
              onCreated={onProjectCreated} notify={notify} />
          </label>
        )}
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
  const [projects, addProject] = useProjects(0);
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
            {m.role === "user" ? <p>{m.content}</p> : <Markdown text={m.content} links={m.links ?? {}} citations={m.citations} onOpen={onOpen} />}
            {!!m.citations?.length && <Sources citations={m.citations} />}
            {!!m.proposals?.length && (
              <div className="proposal-list">
                {m.proposals.map((item, j) => (
                  <ProposalCard key={j} item={item} onOpen={onOpen} projects={projects}
                    onProjectCreated={addProject} notify={notify}
                    onEdit={(proposal) => setProposal(i, j, { ...item, proposal })}
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
        <textarea rows={3} value={draft} disabled={!config.llm_enabled}
          placeholder="Poser une question ou demander une action…"
          title="Entrée : envoyer, Maj+Entrée : retour à la ligne, Alt+C : réduire le chat"
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

/** Documents cités dans une réponse, avec le résultat de la vérification de chaque citation. */
function Sources({ citations }: { citations: Citation[] }) {
  const unverified = citations.filter((c) => !c.verified).length;
  return (
    <details className="chat-sources" open={unverified > 0}>
      <summary>
        Sources : {citations.length} passage(s) cité(s)
        {unverified > 0 && <span className="unverified-count"> · {unverified} non vérifié(s)</span>}
      </summary>
      <ul>
        {citations.map((c) => (
          <li key={c.link} className={c.verified ? "" : "unverified"}>
            <button className="link" title={citationTitle(c)} onClick={() => openCitation(c)}>
              {c.verified ? "✓" : "⚠"} {[c.title, c.section, c.page ? `p. ${c.page}` : ""].filter(Boolean).join(" › ")}
            </button>
            {!c.verified && <span className="small"> — {c.reason}</span>}
          </li>
        ))}
      </ul>
    </details>
  );
}
