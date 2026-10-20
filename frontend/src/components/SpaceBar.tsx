import { goBack, goForward, shortcutLabel, Space, SPACES, useNavHistory } from "../router";

type Props = {
  current: Space;
  badges: Partial<Record<Space, number>>;
  /** espaces où un traitement tourne en arrière-plan (réponse de l'agent en cours) */
  busy?: Partial<Record<Space, boolean>>;
  shortcuts: { back: string; forward: string };
  onSelect: (space: Space) => void;
};

const ICONS: Record<Space, string> = {
  notes: "M6 3h9l4 4v14H6z M15 3v4h4 M9 12h7 M9 16h7",
  taches: "M4 5h4v4H4z M4 15h4v4H4z M11 7h9 M11 17h9 M5 7l1 1 2-2",
  rapports: "M5 20V10 M10 20V4 M15 20v-7 M20 20v-11 M3 20h18",
  imputations: "M12 3a9 9 0 1 0 0 18a9 9 0 1 0 0-18 M12 7v5l3 2",
  inbox: "M3 13l3-8h12l3 8v6H3z M3 13h5l1 3h6l1-3h5",
  brouillons: "M4 20h4l11-11-4-4L4 16z M13 7l4 4 M4 20h16",
};

export default function SpaceBar({ current, badges, busy = {}, shortcuts, onSelect }: Props) {
  const { canBack, canForward } = useNavHistory();
  return (
    <nav className="space-bar">
      <div className="brand-mark" title="Tempo">T</div>
      <div className="history-nav">
        <button onClick={goBack} disabled={!canBack} title={`Vue précédente (${shortcutLabel(shortcuts.back)})`}
          aria-label="Vue précédente">
          <svg viewBox="0 0 24 24" aria-hidden="true"><path d="M15 5l-7 7 7 7" /></svg>
        </button>
        <button onClick={goForward} disabled={!canForward} title={`Vue suivante (${shortcutLabel(shortcuts.forward)})`}
          aria-label="Vue suivante">
          <svg viewBox="0 0 24 24" aria-hidden="true"><path d="M9 5l7 7-7 7" /></svg>
        </button>
      </div>
      {SPACES.map((s, i) => (
        <button key={s.id} className={`space ${current === s.id ? "active" : ""}`}
          onClick={() => onSelect(s.id)} title={`${s.label} (Alt+${i + 1})`}>
          <svg viewBox="0 0 24 24" aria-hidden="true"><path d={ICONS[s.id]} /></svg>
          <span>{s.label}</span>
          {!!badges[s.id] && <span className="space-badge">{badges[s.id]}</span>}
          {busy[s.id] && !badges[s.id] && <span className="space-busy" title="Réponse de l'agent en cours" />}
        </button>
      ))}
    </nav>
  );
}
