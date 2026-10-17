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
};

export type Config = {
  vault: string;
  llm_enabled: boolean;
  model_fast: string;
  model_smart: string;
  statuses: string[];
  priorities: string[];
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

export type SearchResult = { path: string; title: string; type: string; snippet: string };

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
  state: () => request<{ revision: number; inbox: number }>("GET", "/api/state"),
  files: () => request<NoteSummary[]>("GET", "/api/files"),
  readNote: (path: string) => request<Note>("GET", "/api/notes" + q({ path })),
  writeNote: (path: string, content: string, expected_hash: string | null) =>
    request<{ hash: string }>("PUT", "/api/notes", { path, content, expected_hash }),
  createNote: (type: string, title: string) =>
    request<{ path: string }>("POST", "/api/notes", { type, title }),
  deleteNote: (path: string) => request("DELETE", "/api/notes" + q({ path })),
  journalToday: () => request<{ path: string }>("POST", "/api/journal/today"),
  search: (query: string) => request<SearchResult[]>("GET", "/api/search" + q({ q: query })),
  resolve: (name: string) => request<{ path: string }>("GET", "/api/resolve" + q({ name })),
  links: (path: string) =>
    request<{ backlinks: NoteSummary[]; outlinks: NoteSummary[] }>("GET", "/api/links" + q({ path })),
  tasks: () => request<Task[]>("GET", "/api/tasks"),
  createTask: (task: Partial<Task> & { title: string; description?: string }) =>
    request<Task>("POST", "/api/tasks", task),
  updateTask: (id: string, changes: Record<string, string | null>, note?: string) =>
    request<Task>("PATCH", `/api/tasks/${encodeURIComponent(id)}`, { changes, note }),
  inbox: () => request<InboxItem[]>("GET", "/api/inbox"),
  inboxAdd: (text: string) => request<{ paths: string[] }>("POST", "/api/inbox", { text }),
  inboxAnalyze: (path: string) => request<Proposal>("POST", "/api/inbox/analyze" + q({ path })),
  inboxApply: (path: string, proposal: Proposal) =>
    request<{ echange: string; created_tasks: string[]; updated_tasks: string[]; journal: string | null }>(
      "POST", "/api/inbox/apply", { path, proposal }),
  inboxDiscard: (path: string) => request("DELETE", "/api/inbox" + q({ path })),
  weeklyReport: (date: string, extra: string) =>
    request<{ path: string }>("POST", "/api/reports/weekly", { date, extra }),
  taskReport: (id: string) =>
    request<{ path: string }>("POST", `/api/reports/task/${encodeURIComponent(id)}`),
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
  rapport: "Rapport",
  inbox: "Inbox",
  modele: "Modèle",
  note: "Note",
};

export const today = () => new Date().toLocaleDateString("sv-SE");

/** Texte d'un rapport sans frontmatter ni citations [[…]], prêt à coller dans un mail. */
export function toEmailText(markdown: string): string {
  return markdown
    .replace(/^---\n[\s\S]*?\n---\n/, "")
    .replace(/\s*\((?:\s*\[\[[^\]]+\]\]\s*,?)+\)/g, "")
    .replace(/\[\[([^\]|]+)\|([^\]]+)\]\]/g, "$2")
    .replace(/\[\[([^\]]+)\]\]/g, "$1")
    .trim();
}
