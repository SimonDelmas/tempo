import { useEffect, useMemo, useState } from "react";
import { Account, api, Config, Hours, ImputationWeek, Project, today } from "../api";
import { setLeaveGuard } from "../router";
import ImputationPeriods from "./ImputationPeriods";

type Props = {
  config: Config;
  revision: number;
  /** jour de la semaine affichée, porté par l'adresse (#/imputations?date=…) ; null : cette semaine */
  date: string | null;
  onDate: (date: string | null) => void;
  onOpen: (path: string) => void;
  notify: (message: string, error?: boolean) => void;
};

const HOURS_PER_DAY = 8;
const DAY_NAMES = ["Lun", "Mar", "Mer", "Jeu", "Ven", "Sam", "Dim"];

const shiftDate = (date: string, days: number) => {
  const d = new Date(date + "T12:00:00");
  d.setDate(d.getDate() + days);
  return d.toLocaleDateString("sv-SE");
};

const dayLabel = (date: string, index: number) => `${DAY_NAMES[index]} ${date.slice(8, 10)}/${date.slice(5, 7)}`;
const fmt = (n: number) => String(Math.round(n * 100) / 100).replace(".", ",");
const sum = (values: number[]) => values.reduce((a, b) => a + b, 0);
const weekHours = (hours: Hours, alias: string) => sum(Object.values(hours).map((d) => d[alias] ?? 0));

export default function ImputationsView({ config, revision, date: dateParam, onDate, onOpen, notify }: Props) {
  const date = dateParam ?? today();
  const [week, setWeek] = useState<ImputationWeek | null>(null);
  const [accounts, setAccounts] = useState<Account[]>([]);
  const [projects, setProjects] = useState<Project[]>([]);
  const [draft, setDraft] = useState<Hours>({});
  const [comment, setComment] = useState("");
  const [dirty, setDirty] = useState(false);
  const [busy, setBusy] = useState(false);
  const [newAccount, setNewAccount] = useState({ title: "", alias: "", days: "", description: "" });

  const loadRefs = () =>
    Promise.all([api.accounts(), api.projects()])
      .then(([a, p]) => { setAccounts(a); setProjects(p); })
      .catch((e) => notify(e.message, true));

  const loadWeek = () =>
    api.imputations(date).then((w) => {
      setWeek(w);
      setDraft(w.hours);
      setComment(w.comment);
      setDirty(false);
    }).catch((e) => notify(e.message, true));

  // ne pas écraser une saisie en cours quand le vault change
  useEffect(() => { loadRefs(); if (!dirty) loadWeek(); }, [revision]); // eslint-disable-line react-hooks/exhaustive-deps
  useEffect(() => { loadWeek(); }, [date]); // eslint-disable-line react-hooks/exhaustive-deps

  const days = week?.days ?? [];
  const totals = useMemo(
    () => Object.fromEntries(days.map((d) => [d, sum(Object.values(draft[d] ?? {}))])), [draft, days]);

  const setCell = (day: string, alias: string, value: string) => {
    const hours = Math.max(0, Number(value.replace(",", ".")) || 0);
    setDraft((prev) => ({ ...prev, [day]: { ...(prev[day] ?? {}), [alias]: hours } }));
    setDirty(true);
  };

  /** complète la journée jusqu'à 8 h sur le compte choisi */
  const fill = (day: string, alias: string) => {
    const missing = HOURS_PER_DAY - totals[day];
    if (missing <= 0) return;
    setCell(day, alias, String((draft[day]?.[alias] ?? 0) + missing));
  };

  // saisie non enregistrée : confirmation avant de quitter la semaine (navigation interne)
  useEffect(() => {
    setLeaveGuard(dirty ? () => window.confirm("Abandonner les modifications non enregistrées ?") : null);
    return () => setLeaveGuard(null);
  }, [dirty]);

  const changeWeek = (offset: number) => onDate(offset ? shiftDate(date, offset) : null);

  const save = async () => {
    setBusy(true);
    try {
      const w = await api.saveImputations(date, draft, comment);
      setWeek(w);
      setDraft(w.hours);
      setDirty(false);
      const incomplete = w.days.filter((d) => d <= today() && w.totals[d] !== HOURS_PER_DAY);
      notify(incomplete.length ? `Enregistré — ${incomplete.length} jour(s) sans 8 h` : "Imputations enregistrées");
      loadRefs();
    } catch (e) {
      notify((e as Error).message, true);
    } finally {
      setBusy(false);
    }
  };

  const propose = async () => {
    if (dirty && !window.confirm("La proposition remplacera la saisie en cours. Continuer ?")) return;
    setBusy(true);
    try {
      const p = await api.proposeImputations(date);
      setDraft({ ...draft, ...p.hours });
      setComment(p.comment);
      setDirty(true);
      notify("Proposition chargée : vérifiez puis enregistrez");
    } catch (e) {
      notify((e as Error).message, true);
    } finally {
      setBusy(false);
    }
  };

  const createAccount = async () => {
    try {
      await api.createAccount(newAccount.title, newAccount.alias, Number(newAccount.days.replace(",", ".")) || 0,
        newAccount.description);
      setNewAccount({ title: "", alias: "", days: "", description: "" });
      notify("Compte créé");
      loadRefs();
    } catch (e) {
      notify((e as Error).message, true);
    }
  };

  const updateDays = async (account: Account, value: string) => {
    const days = Number(value.replace(",", "."));
    if (Number.isNaN(days) || days === account.days) return;
    try {
      await api.updateAccount(account.alias, { days });
      loadRefs();
    } catch (e) {
      notify((e as Error).message, true);
    }
  };

  const updateDescription = async (account: Account, value: string) => {
    if (value.trim() === account.description) return;
    try {
      await api.updateAccount(account.alias, { description: value });
      loadRefs();
    } catch (e) {
      notify((e as Error).message, true);
    }
  };

  const linkProject = async (project: Project, compte: string) => {
    try {
      await api.setProjectAccount(project.id, compte || null);
      loadRefs();
    } catch (e) {
      notify((e as Error).message, true);
    }
  };

  // comptes du tableau : ceux du vault + ceux déjà saisis (compte supprimé depuis)
  const rows = useMemo(() => {
    const known = accounts.map((a) => a.alias);
    const extra = [...new Set(Object.values(draft).flatMap((d) => Object.keys(d)))].filter((a) => !known.includes(a));
    return [...known, ...extra];
  }, [accounts, draft]);
  const accountOf = (alias: string) => accounts.find((a) => a.alias === alias);

  return (
    <div className="page imputations-view">
      <header className="page-header">
        <h2>Imputations</h2>
        <button className="ghost" onClick={() => changeWeek(-7)} title="Semaine précédente">◀</button>
        <strong>{week ? `${week.week} · ${dayLabel(week.start, 0)} – ${dayLabel(week.end, 6)}` : "…"}</strong>
        <button className="ghost" onClick={() => changeWeek(7)} title="Semaine suivante">▶</button>
        <button className="ghost" onClick={() => changeWeek(0)}>Cette semaine</button>
      </header>

      <ImputationPeriods revision={revision} onWeek={(d) => onDate(d)} onOpen={onOpen} notify={notify} />

      <section className="panel wide-panel">
        <p className="muted small">
          Chaque jour ouvré doit totaliser {HOURS_PER_DAY} h (= 1 jour). Double-clic sur une case : compléter la
          journée sur ce compte.
        </p>
        {accounts.length === 0 ? (
          <p className="warning">Aucun compte d'imputation : créez-en un ci-dessous.</p>
        ) : (
          <table className="imputation-grid">
            <thead>
              <tr>
                <th>Compte</th>
                {days.map((d, i) => <th key={d} className={d === today() ? "today" : ""}>{dayLabel(d, i)}</th>)}
                <th>Semaine</th>
                <th title="Jours restants sur le compte après cette semaine">Reste (j)</th>
              </tr>
            </thead>
            <tbody>
              {rows.map((alias) => {
                const account = accountOf(alias);
                const draftH = weekHours(draft, alias);
                const remaining = account
                  ? account.remaining_days + (weekHours(week?.hours ?? {}, alias) - draftH) / HOURS_PER_DAY
                  : null;
                return (
                  <tr key={alias}>
                    <th title={account ? [account.title, account.description].filter(Boolean).join("\n") : "Compte inconnu"}>
                      <div>{alias}</div>
                      <div className="muted small truncate">{account?.title ?? "compte inconnu"}</div>
                    </th>
                    {days.map((d) => (
                      <td key={d} className={d === today() ? "today" : ""}>
                        <input type="number" min={0} max={24} step={0.5} value={draft[d]?.[alias] || ""}
                          onChange={(e) => setCell(d, alias, e.target.value)} onDoubleClick={() => fill(d, alias)} />
                      </td>
                    ))}
                    <td className="num">{draftH ? fmt(draftH) : ""}</td>
                    <td className={`num ${remaining !== null && remaining < 0 ? "overdue" : ""}`}>
                      {remaining === null ? "–" : fmt(remaining)}
                    </td>
                  </tr>
                );
              })}
            </tbody>
            <tfoot>
              <tr>
                <th>Total</th>
                {days.map((d) => {
                  const total = totals[d] ?? 0;
                  const state = total === HOURS_PER_DAY ? "ok" : d <= today() || total > HOURS_PER_DAY ? "ko" : "";
                  return <td key={d} className={`num total ${state}`}>{fmt(total)} / {HOURS_PER_DAY}</td>;
                })}
                <td className="num">{fmt(sum(days.map((d) => totals[d] ?? 0)))}</td>
                <td />
              </tr>
            </tfoot>
          </table>
        )}
        <label className="block-label">Justification
          <textarea rows={3} value={comment} placeholder="Commentaire (rempli par la proposition IA)"
            onChange={(e) => { setComment(e.target.value); setDirty(true); }} />
        </label>
        <div className="row">
          <button className="primary" disabled={busy || !dirty} onClick={save}>Enregistrer</button>
          <button disabled={busy || !config.llm_enabled || accounts.length === 0} onClick={propose}
            title={config.llm_enabled ? "D'après le rapport hebdomadaire et les jours provisionnés" : "API LLM non configurée (.env)"}>
            {busy ? "…" : "Proposer une répartition (IA)"}
          </button>
          {dirty && <button className="ghost" onClick={loadWeek}>Annuler</button>}
          <span className="spacer" />
          {week?.path && <button className="link" onClick={() => onOpen(week.path!)}>Ouvrir le fichier</button>}
        </div>
        <p className="muted small">
          La proposition s'appuie sur le rapport hebdomadaire de la semaine s'il existe (sinon sur les notes et
          tâches de la semaine) et sur les jours restants de chaque compte. Rien n'est enregistré sans validation.
        </p>
      </section>

      <section className="panel wide-panel">
        <h3>Comptes d'imputation</h3>
        <table className="data-table">
          <thead>
            <tr><th>Alias</th><th>Nom et description</th><th>Attribués (j)</th><th>Consommés (j)</th><th>Restants (j)</th><th>Projets</th></tr>
          </thead>
          <tbody>
            {accounts.map((a) => (
              <tr key={a.alias}>
                <td><button className="link" onClick={() => onOpen(a.path)}>{a.alias}</button></td>
                <td>
                  <div>{a.title}</div>
                  <input key={`${a.alias}-${a.description}`} className="description-input" defaultValue={a.description}
                    placeholder="Ajouter une description…" onBlur={(e) => updateDescription(a, e.target.value)}
                    onKeyDown={(e) => e.key === "Enter" && (e.target as HTMLInputElement).blur()} />
                </td>
                <td>
                  <input key={`${a.alias}-${a.days}`} className="days-input" type="number" min={0} step={0.5}
                    defaultValue={a.days} onBlur={(e) => updateDays(a, e.target.value)}
                    onKeyDown={(e) => e.key === "Enter" && (e.target as HTMLInputElement).blur()} />
                </td>
                <td className="num">{fmt(a.consumed_days)}</td>
                <td className={`num ${a.remaining_days < 0 ? "overdue" : ""}`}>{fmt(a.remaining_days)}</td>
                <td>{a.projects.map((p) => <span key={p} className="tag">{p}</span>)}</td>
              </tr>
            ))}
          </tbody>
        </table>
        <div className="task-create account-create">
          <input placeholder="Nom du compte" value={newAccount.title}
            onChange={(e) => setNewAccount({ ...newAccount, title: e.target.value })} />
          <input placeholder="Alias (ex. SAP-MIG)" value={newAccount.alias}
            onChange={(e) => setNewAccount({ ...newAccount, alias: e.target.value })} />
          <input placeholder="Jours attribués" type="number" min={0} step={0.5} value={newAccount.days}
            onChange={(e) => setNewAccount({ ...newAccount, days: e.target.value })}
            onKeyDown={(e) => e.key === "Enter" && createAccount()} />
          <input className="account-description" placeholder="Description (facultative)" value={newAccount.description}
            onChange={(e) => setNewAccount({ ...newAccount, description: e.target.value })}
            onKeyDown={(e) => e.key === "Enter" && createAccount()} />
          <button className="primary" onClick={createAccount}>Ajouter</button>
        </div>

        <h3>Projets</h3>
        {projects.length === 0 && <p className="muted small">Aucun projet (dossier projets/).</p>}
        <table className="data-table">
          <tbody>
            {projects.map((p) => (
              <tr key={p.id}>
                <td><button className="link" onClick={() => onOpen(p.path)}>{p.title}</button></td>
                <td className="muted small">{p.id}</td>
                <td>
                  <select value={p.compte ?? ""} onChange={(e) => linkProject(p, e.target.value)}>
                    <option value="">— aucun compte —</option>
                    {accounts.map((a) => <option key={a.alias} value={a.alias}>{a.alias} · {a.title}</option>)}
                  </select>
                </td>
              </tr>
            ))}
          </tbody>
        </table>
      </section>
    </div>
  );
}
