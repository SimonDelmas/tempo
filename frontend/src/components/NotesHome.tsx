import { api, NoteSummary, TYPE_LABELS } from "../api";
import { spaceOf } from "../router";

type Props = { files: NoteSummary[]; onOpen: (path: string) => void };

/** Accueil de l'espace Notes : journal du jour et notes récentes. */
export default function NotesHome({ files, onOpen }: Props) {
  const recent = files
    .filter((f) => spaceOf(f.path) === "notes" && f.type !== "modele" && f.type !== "inbox" && f.date)
    .sort((a, b) => b.date.localeCompare(a.date) || b.stem.localeCompare(a.stem))
    .slice(0, 15);

  return (
    <div className="page">
      <header className="page-header">
        <h2>Notes</h2>
        <button className="primary" onClick={() => api.journalToday().then((r) => onOpen(r.path))}>
          Journal du jour
        </button>
      </header>
      <section className="panel">
        <h3>Récentes</h3>
        {recent.length === 0 && <p className="muted small">Aucune note datée.</p>}
        <ul className="link-list">
          {recent.map((n) => (
            <li key={n.path}>
              <button className="link" onClick={() => onOpen(n.path)} title={n.path}>{n.title}</button>
              <span className="muted small">{TYPE_LABELS[n.type] ?? n.type} · {n.date}</span>
            </li>
          ))}
        </ul>
        <p className="muted small">Ctrl+P : ouvrir une note · Alt+1…5 : changer d'espace</p>
      </section>
    </div>
  );
}
