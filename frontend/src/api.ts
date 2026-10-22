export type NoteSummary = {
  path: string;
  stem: string;
  alias: string;
  title: string;
  type: string;
  date: string;
};

export type Note = { path: string; content: string; hash: string; meta: Record<string, unknown> };

export type Task = {
  id: string;
  title: string;
  status: string;
  priority: string;
  project: string | null;
  due: string | null;
  path: string;
  /** position dans le backlog, null si jamais ordonnée */
  rank: number | null;
  /** semaines planifiées (2026-S43), de la plus ancienne à la plus récente */
  iterations: string[];
  /** dernière semaine planifiée */
  iteration: string | null;
};

/** Board d'une semaine : tâches qui y ont été planifiées, avec leur statut actuel. */
export type Board = {
  week: string;
  current: string;
  start: string;
  end: string;
  previous: string;
  next: string;
  /** semaines ayant des tâches, plus la semaine courante */
  weeks: string[];
  tasks: Task[];
};

export type Backlog = { current: string; weeks: { label: string; start: string }[]; tasks: Task[] };

export type Config = {
  vault: string;
  llm_enabled: boolean;
  model_fast: string;
  model_smart: string;
  statuses: string[];
  priorities: string[];
  semantic_enabled: boolean;
  /** raccourcis de navigation entre les vues (« Alt+ArrowLeft ») */
  shortcuts: { back: string; forward: string };
};

export type InboxItem = { path: string; content: string; preview: string };

export type Proposal = {
  echange: {
    source: string;
    date: string;
    subject: string;
    participants: string[];
    projects: string[];
    thread: string;
    summary: string;
  };
  new_tasks: {
    accepted: boolean;
    title: string;
    description: string;
    project: string | null;
    priority: string;
    status: string;
    due: string | null;
  }[];
  task_updates: { accepted: boolean; task_id: string; status: string | null; note: string }[];
  journal_line: string;
  duplicate_of: string | null;
};

/** catégories de compte, dans l'ordre d'affichage (CATEGORIES dans imputations.rs) */
export const ACCOUNT_CATEGORIES = ["projet", "interne", "conges"] as const;
export type AccountCategory = (typeof ACCOUNT_CATEGORIES)[number];
export const CATEGORY_LABELS: Record<AccountCategory, string> = { projet: "Projet", interne: "Interne", conges: "Congés" };

export type Account = {
  alias: string;
  title: string;
  categorie: AccountCategory;
  description: string;
  days: number;
  consumed_days: number;
  remaining_days: number;
  projects: string[];
  path: string;
};

export type Project = {
  id: string;
  title: string;
  path: string;
  compte: string | null;
  /** position de la feature dans le backlog, null si jamais ordonnée */
  rank: number | null;
};

/** heures par jour puis par alias de compte : { "2026-10-12": { "SAP-MIG": 6 } } */
export type Hours = Record<string, Record<string, number>>;

export type ImputationWeek = {
  week: string;
  start: string;
  end: string;
  days: string[];
  hours: Hours;
  totals: Record<string, number>;
  comment: string;
  path: string | null;
};

/** période entre deux dates de saisie ; open : depuis la dernière saisie jusqu'à aujourd'hui */
export type ImputationPeriod = {
  start: string;
  end: string;
  open: boolean;
  workdays: number;
  complete_days: number;
  complete: boolean;
  total_hours: number;
  expected_hours: number;
  accounts: { alias: string; title: string; hours: number; days: number; share: number }[];
  incomplete_weeks: { week: string; start: string; days: string[]; missing_hours: number; extra_hours: number }[];
};

export type ImputationTracking = {
  saisies: string[];
  last_saisie: string | null;
  periods: ImputationPeriod[];
  path: string | null;
};

/** note trouvée par la recherche sémantique (score : proximité de 0 à 1) */
export type SemanticHit = NoteSummary & { snippet: string; score: number };

export type SemanticStatus = {
  enabled: boolean;
  model: string | null;
  indexed: number;
  total: number;
  error: string | null;
};

export type SearchResult ={ path: string; title: string; type: string; snippet: string };

export class ApiError extends Error {
  status: number;
  body: any;
  constructor(status: number, body: any) {
    super(body?.detail ?? `Erreur ${status}`);
    this.status = status;
    this.body = body;
  }
}

async function request<T>(method: string, url: string, body?: unknown): Promise<T> {
  const response = await fetch(url, {
    method,
    headers: body !== undefined ? { "Content-Type": "application/json" } : undefined,
    body: body !== undefined ? JSON.stringify(body) : undefined,
  });
  const data = await response.json().catch(() => null);
  if (!response.ok) throw new ApiError(response.status, data);
  return data as T;
}

const q = (params: Record<string, string>) => "?" + new URLSearchParams(params).toString();

export const api = {
  config: () => request<Config>("GET", "/api/config"),
  state: () => request<{ revision: number; inbox: number; imputation_missing: number }>("GET", "/api/state"),
  files: () => request<NoteSummary[]>("GET", "/api/files"),
  readNote: (path: string) => request<Note>("GET", "/api/notes" + q({ path })),
  writeNote: (path: string, content: string, expected_hash: string | null) =>
    request<{ hash: string }>("PUT", "/api/notes", { path, content, expected_hash }),
  createNote: (type: string, title: string, projects: string[] = []) =>
    request<{ path: string }>("POST", "/api/notes", { type, title, projects }),
  deleteNote: (path: string) => request("DELETE", "/api/notes" + q({ path })),
  journalToday: () => request<{ path: string }>("POST", "/api/journal/today"),
  search: (query: string) => request<SearchResult[]>("GET", "/api/search" + q({ q: query })),
  resolve: (name: string) => request<{ path: string }>("GET", "/api/resolve" + q({ name })),
  links: (path: string) =>
    request<{ backlinks: NoteSummary[]; outlinks: NoteSummary[] }>("GET", "/api/links" + q({ path })),
  tasks: () => request<Task[]>("GET", "/api/tasks"),
  createTask: (task: Partial<Task> & { title: string; description?: string; origin?: string }) =>
    request<Task>("POST", "/api/tasks", task),
  updateTask: (id: string, changes: Record<string, string | null>, note?: string) =>
    request<Task>("PATCH", `/api/tasks/${encodeURIComponent(id)}`, { changes, note }),
  backlog: () => request<Backlog>("GET", "/api/tasks/backlog"),
  board: (week: string | null) => request<Board>("GET", "/api/tasks/board" + q({ week: week ?? "" })),
  /** place la tâche dans l'ordre global (backlog et board) : avant before, sinon après after, sinon à la fin */
  moveTask: (id: string, where: { before?: string | null; after?: string | null }) =>
    request<Task[]>("POST", `/api/tasks/${encodeURIComponent(id)}/move`, where),
  inbox: () => request<InboxItem[]>("GET", "/api/inbox"),
  inboxAdd: (text: string) => request<{ paths: string[] }>("POST", "/api/inbox", { text }),
  inboxAnalyze: (path: string) => request<Proposal>("POST", "/api/inbox/analyze" + q({ path })),
  inboxApply: (path: string, proposal: Proposal) =>
    request<{ echange: string; created_tasks: string[]; updated_tasks: string[]; journal: string | null }>(
      "POST", "/api/inbox/apply", { path, proposal }),
  inboxDiscard: (path: string) => request("DELETE", "/api/inbox" + q({ path })),
  weeklyReport: (date: string, extra: string) =>
    request<{ path: string }>("POST", "/api/reports/weekly", { date, extra }),
  accounts: () => request<Account[]>("GET", "/api/accounts"),
  createAccount: (title: string, alias: string, categorie: AccountCategory, days: number, description: string) =>
    request<Account>("POST", "/api/accounts", { title, alias, categorie, days, description }),
  updateAccount: (alias: string,
    changes: { title?: string; categorie?: AccountCategory; days?: number; description?: string }) =>
    request<Account>("PATCH", `/api/accounts/${encodeURIComponent(alias)}`, changes),
  projects: () => request<Project[]>("GET", "/api/projects"),
  createProject: (title: string) => request<Project>("POST", "/api/projects", { title }),
  setProjectAccount: (id: string, compte: string | null) =>
    request<Project>("PUT", `/api/projects/${encodeURIComponent(id)}/account`, { compte }),
  /** place le projet avant before dans le backlog (null : à la fin) ; renvoie les projets ordonnés */
  moveProject: (id: string, before: string | null) =>
    request<Project[]>("POST", `/api/projects/${encodeURIComponent(id)}/move`, { before }),
  imputations: (date: string) => request<ImputationWeek>("GET", "/api/imputations" + q({ date })),
  saveImputations: (date: string, hours: Hours, comment: string) =>
    request<ImputationWeek>("PUT", "/api/imputations", { date, hours, comment }),
  proposeImputations: (date: string) =>
    request<{ hours: Hours; comment: string }>("POST", "/api/imputations/propose" + q({ date })),
  imputationTracking: () => request<ImputationTracking>("GET", "/api/imputations/saisies"),
  addSaisie: (date: string) => request<ImputationTracking>("POST", "/api/imputations/saisies", { date }),
  removeSaisie: (date: string) => request<ImputationTracking>("DELETE", "/api/imputations/saisies" + q({ date })),
  taskReport: (id: string) =>
    request<{ path: string }>("POST", `/api/reports/task/${encodeURIComponent(id)}`),
  chat: (messages: ChatHistory) => request<ChatReply>("POST", "/api/chat", { messages }),
  chatStream,
  semanticSearch: (query: string) => request<SemanticHit[]>("GET", "/api/semantic/search" + q({ q: query })),
  semanticRelated: (path: string) => request<SemanticHit[]>("GET", "/api/semantic/related" + q({ path })),
  semanticStatus: () => request<SemanticStatus>("GET", "/api/semantic/status"),
  extractTasks: (path: string) =>
    request<{ tasks: TaskDraft[] }>("POST", "/api/assist/extract-tasks" + q({ path })),
  transformText: (action: TextAction, text: string) =>
    request<{ text: string }>("POST", "/api/assist/text", { action, text }),
  chatApply: (proposal: ChatProposal) =>
    request<{ path: string | null; message: string }>("POST", "/api/chat/apply", proposal),
};

export const STATUS_LABELS: Record<string, string> = {
  a_faire: "À faire",
  en_cours: "En cours",
  attente: "En attente",
  bloquee: "Bloquée",
  terminee: "Terminée",
};

export const TYPE_LABELS: Record<string, string> = {
  journal: "Journal",
  reunion: "Réunion",
  echange: "Échange",
  tache: "Tâche",
  projet: "Projet",
  sujet: "Sujet",
  personne: "Personne",
  ressource: "Ressource",
  formation: "Formation",
  rapport: "Rapport",
  compte: "Compte",
  imputation: "Imputations",
  inbox: "Inbox",
  modele: "Modèle",
  note: "Note",
};

/** Comptes regroupés par catégorie (groupes vides omis), dans l'ordre de ACCOUNT_CATEGORIES. */
export const groupAccounts = (accounts: Account[]) =>
  ACCOUNT_CATEGORIES
    .map((categorie) => ({ categorie, label: CATEGORY_LABELS[categorie], accounts: accounts.filter((a) => a.categorie === categorie) }))
    .filter((g) => g.accounts.length > 0);

export const today = () =>new Date().toLocaleDateString("sv-SE");

/** Texte d'un rapport sans frontmatter ni citations [[…]], prêt à coller dans un mail. */
export function toEmailText(markdown: string): string {
  return markdown
    .replace(/^---\n[\s\S]*?\n---\n/, "")
    .replace(/\s*\((?:\s*\[\[[^\]]+\]\]\s*,?)+\)/g, "")
    .replace(/\[\[([^\]|]+)\|([^\]]+)\]\]/g, "$2")
    .replace(/\[\[([^\]]+)\]\]/g, "$1")
    .trim();
}

/** `args` est le texte brut quand le modèle a produit des arguments illisibles */
export type ChatStep = { tool: string; args: Record<string, unknown> | string; summary: string | null };
/** Modification proposée par l'agent (une variante par opération) ; `title` désigne la note visée. */
export type ChatProposal =
  | { kind: "create_task"; title: string; description: string; project: string | null; priority: string | null;
      status: string | null; due: string | null }
  | { kind: "update_task"; task_id: string; title: string; status: string | null; priority: string | null;
      due: string | null; note: string; new_title?: string | null; project?: string | null; iteration?: string | null }
  | { kind: "journal"; line: string }
  | { kind: "create_note"; type: string; title: string; date: string | null; content: string; projects?: string[] }
  | { kind: "edit_note"; path: string; title: string; edits: { find: string; replace: string }[] }
  | { kind: "append_note"; path: string; title: string; section: string | null; text: string }
  | { kind: "update_meta"; path: string; title: string; set: Record<string, unknown>; remove: string[];
      before: Record<string, unknown> }
  | { kind: "delete_note"; path: string; title: string; backlinks: string[] }
  | { kind: "link"; path: string; title: string; link: string; target_path: string; field: string | null;
      section: string | null }
  | { kind: "unlink"; path: string; title: string; link: string; target_path: string }
  | { kind: "move_task"; task_id: string; title: string; before: string | null }
  | { kind: "create_project"; title: string }
  | { kind: "move_project"; project_id: string; title: string; before: string | null }
  | { kind: "set_project_account"; project_id: string; title: string; compte: string | null }
  | { kind: "create_account"; title: string; alias: string; categorie: string; days: number; description: string }
  | { kind: "update_account"; alias: string; title: string | null; categorie: string | null; days: number | null;
      description: string | null; before: Record<string, unknown> }
  | { kind: "save_imputations"; date: string; week: string; hours: Hours; comment: string; before: Hours }
  | { kind: "saisie"; date: string; done: boolean }
  | { kind: "inbox_add"; text: string }
  | { kind: "inbox_discard"; path: string; preview: string }
  | { kind: "inbox_apply"; path: string; proposal: Proposal }
  | { kind: "weekly_report"; date: string | null; week: string; extra: string }
  | { kind: "task_report"; task_id: string; title: string };
export type ChatReply = {
  answer: string;
  steps: ChatStep[];
  links: Record<string, string>;
  proposals: ChatProposal[];
};
export type ChatEvent =
  | ({ type: "step" } & ChatStep)
  | { type: "delta"; text: string }
  /** le texte diffusé précédait des appels d'outils : il est effacé */
  | { type: "reset" }
  | ({ type: "done" } & ChatReply)
  | { type: "error"; detail: string };

type ChatHistory = { role: "user" | "assistant"; content: string }[];

/** Réponse de l'agent diffusée en Server-Sent Events ; renvoie l'événement final. */
async function chatStream(messages: ChatHistory, onEvent: (event: ChatEvent) => void): Promise<ChatReply> {
  const response = await fetch("/api/chat/stream", {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify({ messages }),
  });
  if (!response.ok || !response.body) {
    throw new ApiError(response.status, await response.json().catch(() => null));
  }
  const reader = response.body.pipeThrough(new TextDecoderStream()).getReader();
  let buffer = "";
  for (;;) {
    const { value, done } = await reader.read();
    if (done) break;
    buffer += value;
    const parts = buffer.split("\n\n");
    buffer = parts.pop() ?? "";
    for (const part of parts) {
      if (!part.startsWith("data: ")) continue;
      const event = JSON.parse(part.slice(6)) as ChatEvent;
      if (event.type === "error") throw new Error(event.detail);
      onEvent(event);
      if (event.type === "done") return event;
    }
  }
  throw new Error("Réponse de l'agent interrompue");
}

export type TaskDraft = {
  title: string;
  description: string;
  project: string | null;
  priority: string;
  due: string | null;
  duplicate_of: string | null;
};

export type TextAction = "resumer" | "reformuler" | "actions";

/** brouillon : texte libre d'un onglet de l'espace Draft (hors notes, `.brouillons/` du vault) */
export type Draft = { id: string; title: string; content: string; created: string; updated: string };

export type DraftProposal = {
  note: { accepted: boolean; type: string; title: string; content: string; projects: string[] } | null;
  tasks: (TaskDraft & { accepted: boolean; status: string })[];
  comment: string;
};

export const drafts = {
  list: () => request<Draft[]>("GET", "/api/drafts"),
  create: (title = "") => request<Draft>("POST", "/api/drafts", { title }),
  save: (id: string, changes: { title?: string; content?: string }) =>
    request<Draft>("PUT", `/api/drafts/${encodeURIComponent(id)}`, changes),
  remove: (id: string) => request("DELETE", `/api/drafts/${encodeURIComponent(id)}`),
  analyze: (id: string) => request<DraftProposal>("POST", `/api/drafts/${encodeURIComponent(id)}/analyze`),
  apply: (id: string, proposal: DraftProposal, close: boolean) =>
    request<{ note: string | null; created_tasks: string[]; closed: boolean }>(
      "POST", `/api/drafts/${encodeURIComponent(id)}/apply`, { proposal, close }),
};

/** types de notes proposés à la création (NOTE_KINDS dans journal.rs) */
export const NOTE_KINDS = ["note", "sujet", "reunion", "projet", "personne", "ressource", "formation"];
