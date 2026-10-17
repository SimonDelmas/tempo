import { useState } from "react";
import { api, Config, NoteSummary, today } from "../api";

type Props = {
  config: Config;
  files: NoteSummary[];
  onOpen: (path: string) => void;
  notify: (message: string, error?: boolean) => void;
};

export default function ReportsView({ config, files, onOpen, notify }: Props) {
  const [date, setDate] = useState(today());
  const [extra, setExtra] = useState("");
  const [busy, setBusy] = useState(false);

  const reports = files
    .filter((f) => f.type === "rapport")
    .sort((a, b) => b.stem.localeCompare(a.stem));
  const weekly = reports.filter((r) => r.path.startsWith("rapports/hebdo/"));
  const taskReports = reports.filter((r) => !r.path.startsWith("rapports/hebdo/"));

  const generate = async () => {
    setBusy(true);
    try {
      const { path } = await api.weeklyReport(date, extra);
      notify("Rapport hebdomadaire généré");
      onOpen(path);
    } catch (e) {
      notify((e as Error).message, true);
    } finally {
      setBusy(false);
    }
  };

  return (
    <div className="page reports-view">
      <header className="page-header"><h2>Rapports</h2></header>

      <section className="panel">
        <h3>Rapport hebdomadaire</h3>
        <p className="muted small">
          Réunions et échanges · Actions réalisées · Non terminé · Plan pour la semaine suivante.
          Sources : journal, réunions, échanges, historique et échéances des tâches.
        </p>
        <div className="grid-form">
          <label>Semaine du<input type="date" value={date} onChange={(e) => setDate(e.target.value)} /></label>
          <label className="wide">Éléments à ajouter (plan, points d'attention…)
            <textarea rows={4} value={extra} onChange={(e) => setExtra(e.target.value)}
              placeholder="Ex. : semaine prochaine, démarrage de la recette le 28/10" />
          </label>
        </div>
        <button className="primary" disabled={busy || !config.llm_enabled} onClick={generate}
          title={config.llm_enabled ? "" : "API LLM non configurée (.env)"}>
          {busy ? "Génération en cours…" : "Générer le rapport"}
        </button>
        {!config.llm_enabled && <p className="warning">API LLM non configurée : renseignez VIBE_BASE_URL et VIBE_API_KEY dans .env.</p>}
      </section>

      <section className="panel">
        <h3>Rapports hebdomadaires</h3>
        {weekly.length === 0 && <p className="muted small">Aucun rapport pour l'instant.</p>}
        <ul className="link-list">
          {weekly.map((r) => <li key={r.path}><button className="link" onClick={() => onOpen(r.path)}>{r.title}</button></li>)}
        </ul>
        <h3>Rapports d'avancement de tâches</h3>
        <p className="muted small">À générer depuis la fiche d'une tâche (bouton « Rapport d'avancement »).</p>
        <ul className="link-list">
          {taskReports.map((r) => <li key={r.path}><button className="link" onClick={() => onOpen(r.path)}>{r.title}</button></li>)}
        </ul>
      </section>
    </div>
  );
}
