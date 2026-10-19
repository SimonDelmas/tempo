import { useEffect, useState } from "react";
import { api, ImputationPeriod, ImputationTracking, today } from "../api";

type Props = {
  revision: number;
  /** ouvre la semaine contenant ce jour dans la grille de saisie */
  onWeek: (date: string) => void;
  onOpen: (path: string) => void;
  notify: (message: string, error?: boolean) => void;
};

const fmt = (n: number) => String(Math.round(n * 100) / 100).replace(".", ",");
const short = (date: string) => `${date.slice(8, 10)}/${date.slice(5, 7)}`;
const long = (date: string) => `${short(date)}/${date.slice(0, 4)}`;
const DAY_NAMES = ["Dim", "Lun", "Mar", "Mer", "Jeu", "Ven", "Sam"];
const dayName = (date: string) => DAY_NAMES[new Date(date + "T12:00:00").getDay()];

const periodLabel = (p: ImputationPeriod) =>
  p.open ? `En cours · depuis le ${short(p.start)}` : `${short(p.start)} → ${long(p.end)}`;

/** Dates de dernière saisie, périodes qu'elles définissent, bilan et complétude de chacune. */
export default function ImputationPeriods({ revision, onWeek, onOpen, notify }: Props) {
  const [tracking, setTracking] = useState<ImputationTracking | null>(null);
  const [selected, setSelected] = useState(0);
  const [date, setDate] = useState(today());

  const apply = (t: ImputationTracking) => {
    setTracking(t);
    setSelected((s) => Math.min(s, Math.max(t.periods.length - 1, 0)));
  };

  useEffect(() => { api.imputationTracking().then(apply).catch((e) => notify(e.message, true)); }, [revision]); // eslint-disable-line react-hooks/exhaustive-deps

  const add = async () => {
    try {
      apply(await api.addSaisie(date));
      setSelected(0);
      notify(`Saisie du ${long(date)} enregistrée`);
    } catch (e) {
      notify((e as Error).message, true);
    }
  };

  const remove = async (saisie: string) => {
    if (!window.confirm(`Retirer la saisie du ${long(saisie)} ?`)) return;
    try {
      apply(await api.removeSaisie(saisie));
    } catch (e) {
      notify((e as Error).message, true);
    }
  };

  if (!tracking) return null;
  const period = tracking.periods[selected];

  return (
    <section className="panel wide-panel periods">
      <div className="row">
        <h3>Suivi des saisies</h3>
        <span className="spacer" />
        <label className="check">J'ai saisi mes imputations jusqu'au
          <input type="date" value={date} onChange={(e) => setDate(e.target.value)} />
        </label>
        <button className="primary" disabled={!date} onClick={add}>Enregistrer</button>
      </div>

      {tracking.saisies.length === 0 ? (
        <p className="muted small">
          Indiquez la date jusqu'à laquelle vos imputations sont saisies dans l'outil de gestion des temps. La
          première date marque le début du suivi ; chaque date suivante clôt une période.
        </p>
      ) : (
        <p className="muted small saisies">
          Dernière saisie : <strong>{long(tracking.last_saisie!)}</strong> · historique :
          {tracking.saisies.map((s) => (
            <span key={s} className="tag">
              {long(s)}
              <span className="tag-remove" title="Retirer" onClick={() => remove(s)}>×</span>
            </span>
          ))}
          {tracking.path && <button className="link" onClick={() => onOpen(tracking.path!)}>Ouvrir le fichier</button>}
        </p>
      )}

      {tracking.saisies.length > 0 && tracking.periods.length === 0 && (
        <p className="muted small">Saisie à jour : pas de période en cours.</p>
      )}

      {tracking.periods.length > 0 && (
        <div className="period-tabs">
          {tracking.periods.map((p, i) => (
            <button key={p.start} className={`period-tab ${i === selected ? "active" : ""} ${p.complete ? "ok" : "ko"}`}
              onClick={() => setSelected(i)} title={p.complete ? "Complète" : "Incomplète"}>
              <span className="dot" />{periodLabel(p)}
            </button>
          ))}
        </div>
      )}

      {period && (
        <>
          <div className="stat-tiles">
            <div className={`stat-tile ${period.complete ? "ok" : "ko"}`}>
              <div className="stat-label">Complétude</div>
              <div className="stat-value">{period.complete_days} / {period.workdays} j</div>
              <div className="muted small">{period.complete ? "Tous les jours ouvrés font 8 h" : `${period.workdays - period.complete_days} jour(s) à compléter`}</div>
            </div>
            <div className="stat-tile">
              <div className="stat-label">Heures saisies</div>
              <div className="stat-value">{fmt(period.total_hours)} / {fmt(period.expected_hours)} h</div>
              <div className="muted small">{fmt(period.total_hours / 8)} j imputés</div>
            </div>
            <div className="stat-tile">
              <div className="stat-label">Période</div>
              <div className="stat-value">{short(period.start)} → {short(period.end)}</div>
              <div className="muted small">{period.open ? "à saisir dans l'outil de gestion des temps" : "saisie close"}</div>
            </div>
          </div>

          {period.incomplete_weeks.length > 0 && (
            <>
              <h4>Semaines à compléter</h4>
              <ul className="gap-list">
                {period.incomplete_weeks.map((w) => (
                  <li key={w.week}>
                    <button className="link" onClick={() => onWeek(w.days[0])}>{w.week}</button>
                    <span className="muted small">
                      {w.days.map((d) => `${dayName(d)} ${short(d)}`).join(", ")}
                    </span>
                    <span className="spacer" />
                    {w.missing_hours > 0 && <span className="overdue">{fmt(w.missing_hours)} h manquantes</span>}
                    {w.extra_hours > 0 && <span className="overdue">{fmt(w.extra_hours)} h en trop</span>}
                  </li>
                ))}
              </ul>
            </>
          )}

          <h4>Bilan par compte</h4>
          {period.accounts.length === 0 ? (
            <p className="muted small">Aucune heure saisie sur la période.</p>
          ) : (
            <table className="data-table">
              <thead>
                <tr><th>Compte</th><th>Nom</th><th className="num">Heures</th><th className="num">Jours</th><th>Part</th></tr>
              </thead>
              <tbody>
                {period.accounts.map((a) => (
                  <tr key={a.alias}>
                    <td><strong>{a.alias}</strong></td>
                    <td>{a.title || <span className="muted">compte inconnu</span>}</td>
                    <td className="num">{fmt(a.hours)}</td>
                    <td className="num">{fmt(a.days)}</td>
                    <td className="share">
                      <span className="share-bar"><span style={{ width: `${a.share * 100}%` }} /></span>
                      <span className="num small">{Math.round(a.share * 100)} %</span>
                    </td>
                  </tr>
                ))}
              </tbody>
            </table>
          )}
        </>
      )}
    </section>
  );
}
