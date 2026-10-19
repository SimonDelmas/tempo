import { Space, SPACES } from "../router";

type Props = {
  current: Space;
  badges: Partial<Record<Space, number>>;
  onSelect: (space: Space) => void;
};

const ICONS: Record<Space, string> = {
  notes: "M6 3h9l4 4v14H6z M15 3v4h4 M9 12h7 M9 16h7",
  taches: "M4 5h4v4H4z M4 15h4v4H4z M11 7h9 M11 17h9 M5 7l1 1 2-2",
  rapports: "M5 20V10 M10 20V4 M15 20v-7 M20 20v-11 M3 20h18",
  imputations: "M12 3a9 9 0 1 0 0 18a9 9 0 1 0 0-18 M12 7v5l3 2",
  inbox: "M3 13l3-8h12l3 8v6H3z M3 13h5l1 3h6l1-3h5",
};

export default function SpaceBar({ current, badges, onSelect }: Props) {
  return (
    <nav className="space-bar">
      <div className="brand-mark" title="Tempo">T</div>
      {SPACES.map((s, i) => (
        <button key={s.id} className={`space ${current === s.id ? "active" : ""}`}
          onClick={() => onSelect(s.id)} title={`${s.label} (Alt+${i + 1})`}>
          <svg viewBox="0 0 24 24" aria-hidden="true"><path d={ICONS[s.id]} /></svg>
          <span>{s.label}</span>
          {!!badges[s.id] && <span className="space-badge">{badges[s.id]}</span>}
        </button>
      ))}
    </nav>
  );
}
