import { useState } from "react";
import { api, Config, today } from "../api";

type Props = {
  config: Config;
  onOpen: (path: string) => void;
  notify: (message: string, error?: boolean) => void;
};

export default function ReportsView({ config, onOpen, notify }: Props) {
  const [date, setDate] = useState(today());
  const [extra, setExtra] = useState("");
  const [busy, setBusy] = useState(false);

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

      <p className="muted small">
        Les rapports enregistrés sont listés dans la barre latérale. Les rapports d'avancement se génèrent depuis
        la fiche d'une tâche (bouton « Rapport d'avancement »).
      </p>
    </div>
  );
}
