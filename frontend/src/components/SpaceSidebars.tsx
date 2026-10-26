import { ReactNode, useEffect, useMemo, useState } from "react";
import { Account, api, groupAccounts, NoteSummary, STATUS_LABELS, Task, today } from "../api";
import { useProjects } from "./ProjectSelect";
import type { TasksTab } from "./TasksView";

/** Barres latérales des espaces Tâches, Rapports et Imputations (Notes : NotesSidebar). */

function Item({ active, onClick, title, children, extra }: {
  active?: boolean; onClick: () => void; title?: string; children: ReactNode; extra?: ReactNode;
}) {
  return (
    <button className={`nav ${active ? "active" : ""}`} onClick={onClick} title={title}>
      <span className="nav-label">{children}</span>{extra}
    </button>
  );
}

const Section = ({ title, action, children }: { title: string; action?: ReactNode; children: ReactNode }) => (
  <>
    <h4 className="sidebar-section">{title}{action}</h4>
    {children}
  </>
);

/** tri des projets de la barre des tâches : ordre du backlog ou nombre de tâches ouvertes */
type ProjectSort = "rang" | "taches";
const PROJECT_SORT_KEY = "tempo.tasks.projectSort";

function loadProjectSort(): ProjectSort {
  try { return localStorage.getItem(PROJECT_SORT_KEY) === "taches" ? "taches" : "rang"; } catch { return "rang"; }
}

const fmt = (n: number) => String(Math.round(n * 100) / 100).replace(".", ",");

// --- Tâches ---------------------------------------------------------------

export function TasksSidebar({ revision, current, tab, project, onOpen, onNavigate }: {
  revision: number;
  current: string | null;
  tab: TasksTab;
  project: string | null;
  onOpen: (path: string) => void;
  onNavigate: (params: { vue?: TasksTab; semaine?: string | null; projet?: string | null }) => void;
}) {
  const [tasks, setTasks] = useState<Task[]>([]);
  useEffect(() => { api.tasks().then(setTasks).catch(() => {}); }, [revision]);

  const open = tasks.filter((t) => t.status !== "terminee");
  const [notes] = useProjects(revision);
  const [sort, setSort] = useState<ProjectSort>(loadProjectSort);
  const toggleSort = () => {
    const next = sort === "rang" ? "taches" : "rang";
    setSort(next);
    try { localStorage.setItem(PROJECT_SORT_KEY, next); } catch { /* préférence non conservée */ }
  };
  /** [identifiant, titre, tâches ouvertes] : projets du vault puis projets cités sans note */
  const projects = useMemo(() => {
    const counts = new Map<string, number>();
    for (const t of open) if (t.project) counts.set(t.project, (counts.get(t.project) ?? 0) + 1);
    const orphans = [...counts.keys()].filter((id) => !notes.some((p) => p.id === id))
      .sort((a, b) => a.localeCompare(b, "fr"));
    // ordre des features du backlog : rang croissant, puis titre
    const ordered = [...notes].sort((a, b) =>
      (a.rank ?? Infinity) - (b.rank ?? Infinity) || a.title.localeCompare(b.title, "fr"));
    const list = [
      ...ordered.map((p) => [p.id, p.title, counts.get(p.id) ?? 0] as const),
      ...orphans.map((id) => [id, id, counts.get(id)!] as const),
    ];
    // tri stable : à nombre égal, l'ordre du backlog est conservé
    return sort === "taches" ? list.sort((a, b) => b[2] - a[2]) : list;
  }, [open, notes, sort]);
  const watch = open.filter((t) => t.status === "bloquee" || (t.due && t.due < today()));
  const doing = open.filter((t) => t.status === "en_cours");

  const taskItem = (t: Task) => (
    <Item key={t.id} active={t.path === current} onClick={() => onOpen(t.path)}
      title={`${t.id} · ${STATUS_LABELS[t.status] ?? t.status}${t.due ? ` · échéance ${t.due}` : ""}`}>
      <span className="muted small">{t.id}</span> {t.title}
    </Item>
  );

  return (
    <aside className="sidebar">
      <div className="sidebar-title">Tâches</div>
      <Item active={!current && tab === "backlog"} onClick={() => onNavigate({ vue: "backlog" })}
        extra={<span className="count">{open.length}</span>}>Backlog</Item>
      <Item active={!current && tab === "board"} onClick={() => onNavigate({ vue: "board", semaine: null })}>
        Board de la semaine
      </Item>
      <Section title="Projets" action={
        <button className="section-sort" onClick={toggleSort}
          title={sort === "rang" ? "Trier par nombre de tâches ouvertes" : "Trier dans l'ordre du backlog"}>
          {sort === "rang" ? "ordre backlog" : "nb de tâches"} ⇅
        </button>
      }>
        {projects.length === 0 && <p className="muted small">Aucun projet</p>}
        <div className="nav-compact">
          {projects.map(([p, title, n]) => (
            <Item key={p} active={!current && project === p} onClick={() => onNavigate({ projet: project === p ? null : p })}
              title={title} extra={<span className="count">{n}</span>}>{title}</Item>
          ))}
        </div>
      </Section>
      {watch.length > 0 && <Section title="À surveiller">{watch.map(taskItem)}</Section>}
      <Section title="En cours">
        {doing.length === 0 && <p className="muted small">Aucune</p>}
        {doing.map(taskItem)}
      </Section>
    </aside>
  );
}

// --- Rapports -------------------------------------------------------------

export function ReportsSidebar({ files, current, onOpen, onHome }: {
  files: NoteSummary[];
  current: string | null;
  onOpen: (path: string) => void;
  onHome: () => void;
}) {
  const reports = files.filter((f) => f.type === "rapport").sort((a, b) => b.stem.localeCompare(a.stem));
  const weekly = reports.filter((r) => r.path.startsWith("rapports/hebdo/"));
  const taskReports = reports.filter((r) => !r.path.startsWith("rapports/hebdo/"));
  const list = (items: NoteSummary[]) => items.map((r) => (
    <Item key={r.path} active={r.path === current} onClick={() => onOpen(r.path)} title={r.path}>{r.title}</Item>
  ));

  return (
    <aside className="sidebar">
      <div className="sidebar-title">Rapports</div>
      <Item active={!current} onClick={onHome}>+ Nouveau rapport hebdo</Item>
      <Section title="Hebdomadaires">
        {weekly.length === 0 && <p className="muted small">Aucun</p>}
        {list(weekly)}
      </Section>
      <Section title="Avancement de tâches">
        {taskReports.length === 0 && <p className="muted small">Depuis la fiche d'une tâche</p>}
        {list(taskReports)}
      </Section>
    </aside>
  );
}

// --- Imputations ----------------------------------------------------------

/** lundi d'une semaine ISO « 2026-S42 » */
export function isoWeekStart(label: string): string | null {
  const m = /^(\d{4})-S(\d{2})$/.exec(label);
  if (!m) return null;
  const jan4 = new Date(Number(m[1]), 0, 4, 12);
  const monday = new Date(jan4);
  monday.setDate(jan4.getDate() - ((jan4.getDay() + 6) % 7) + (Number(m[2]) - 1) * 7);
  return monday.toLocaleDateString("sv-SE");
}

export function ImputationsSidebar({ files, revision, current, week, onOpen, onWeek }: {
  files: NoteSummary[];
  revision: number;
  current: string | null;
  week: string | null;
  onOpen: (path: string) => void;
  onWeek: (date: string | null) => void;
}) {
  const [accounts, setAccounts] = useState<Account[]>([]);
  useEffect(() => { api.accounts().then(setAccounts).catch(() => {}); }, [revision]);

  const weeks = files
    .filter((f) => f.path.startsWith("imputations/"))
    .map((f) => ({ label: f.stem, start: isoWeekStart(f.stem) }))
    .filter((w): w is { label: string; start: string } => !!w.start)
    .sort((a, b) => b.label.localeCompare(a.label));

  return (
    <aside className="sidebar">
      <div className="sidebar-title">Imputations</div>
      <Item active={!current && !week} onClick={() => onWeek(null)}>Semaine en cours</Item>
      {accounts.length === 0 && (
        <Section title="Comptes"><p className="muted small">Aucun compte</p></Section>
      )}
      {groupAccounts(accounts).map((g) => (
        <Section key={g.categorie} title={`Comptes · ${g.label}`}>
          {g.accounts.map((a) => (
            <Item key={a.alias} active={a.path === current} onClick={() => onOpen(a.path)} title={[a.title, a.description].filter(Boolean).join("\n")}
              extra={<span className={`count ${a.remaining_days < 0 ? "overdue" : ""}`}>{fmt(a.remaining_days)} j</span>}>
              {a.alias}
            </Item>
          ))}
        </Section>
      ))}
      <Section title="Semaines saisies">
        {weeks.length === 0 && <p className="muted small">Aucune</p>}
        {weeks.map((w) => (
          <Item key={w.label} active={!current && week === w.start} onClick={() => onWeek(w.start)}>{w.label}</Item>
        ))}
      </Section>
    </aside>
  );
}
