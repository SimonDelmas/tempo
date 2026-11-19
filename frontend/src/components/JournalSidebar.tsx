import { useEffect, useMemo, useState } from "react";
import { NoteSummary } from "../api";

type Props = {
  files: NoteSummary[];
  current: string | null;
  /** ouvre le journal du jour (sans date) ou d'une date AAAA-MM-JJ, créé s'il n'existe pas */
  onOpenDate: (date?: string) => void;
};

const WEEKDAYS = ["L", "M", "M", "J", "V", "S", "D"];

const pad = (n: number) => String(n).padStart(2, "0");
const isoDate = (y: number, m: number, d: number) => `${y}-${pad(m + 1)}-${pad(d)}`;

function todayIso() {
  const now = new Date();
  return isoDate(now.getFullYear(), now.getMonth(), now.getDate());
}

/** date AAAA-MM-JJ d'une note du journal (`journal/2026/2026-10-14.md`), sinon null */
function journalDate(path: string | null): string | null {
  return path?.match(/^journal\/\d{4}\/(\d{4}-\d{2}-\d{2})\.md$/)?.[1] ?? null;
}

/** Barre latérale de l'entrée Journal : journal du jour et calendrier des notes quotidiennes. */
export default function JournalSidebar({ files, current, onOpenDate }: Props) {
  const today = todayIso();
  const selected = journalDate(current);
  // mois affiché : celui de la note ouverte, ou le mois courant
  const [month, setMonth] = useState(() => (selected ?? today).slice(0, 7));
  useEffect(() => {
    if (selected) setMonth(selected.slice(0, 7));
  }, [selected]);

  // jours qui ont déjà une note de journal
  const existing = useMemo(
    () => new Set(files.map((f) => journalDate(f.path)).filter((d): d is string => !!d)),
    [files],
  );

  const [year, monthIndex] = [Number(month.slice(0, 4)), Number(month.slice(5, 7)) - 1];
  const first = new Date(year, monthIndex, 1);
  const days = new Date(year, monthIndex + 1, 0).getDate();
  // décalage pour une semaine commençant le lundi
  const offset = (first.getDay() + 6) % 7;
  const cells: (number | null)[] = [
    ...Array(offset).fill(null),
    ...Array.from({ length: days }, (_, i) => i + 1),
  ];
  while (cells.length % 7) cells.push(null);

  const shift = (delta: number) => {
    const d = new Date(year, monthIndex + delta, 1);
    setMonth(`${d.getFullYear()}-${pad(d.getMonth() + 1)}`);
  };
  const label = first.toLocaleDateString("fr-FR", { month: "long", year: "numeric" });
  const count = [...existing].filter((d) => d.startsWith(month)).length;

  return (
    <aside className="sidebar">
      <div className="sidebar-title">Journal</div>
      <button className={`nav ${selected === today ? "active" : ""}`} title="Alt+J" onClick={() => onOpenDate()}>
        Journal du jour
      </button>

      <div className="calendar">
        <div className="calendar-head">
          <button className="ghost" onClick={() => shift(-1)} title="Mois précédent">‹</button>
          <button className="ghost calendar-month" onClick={() => setMonth(today.slice(0, 7))}
            title="Revenir au mois courant">
            {label}
          </button>
          <button className="ghost" onClick={() => shift(1)} title="Mois suivant">›</button>
        </div>
        <div className="calendar-grid">
          {WEEKDAYS.map((d, i) => <span key={i} className="calendar-weekday">{d}</span>)}
          {cells.map((day, i) => {
            if (day === null) return <span key={i} />;
            const date = isoDate(year, monthIndex, day);
            const classes = [
              "calendar-day",
              existing.has(date) && "has-note",
              date === today && "today",
              date === selected && "selected",
              i % 7 >= 5 && "weekend",
            ].filter(Boolean).join(" ");
            return (
              <button key={i} className={classes} onClick={() => onOpenDate(date)}
                title={existing.has(date) ? `Ouvrir le journal du ${date}` : `Créer le journal du ${date}`}>
                {day}
              </button>
            );
          })}
        </div>
        <p className="muted small">{count ? `${count} note${count > 1 ? "s" : ""} ce mois-ci` : "Aucune note ce mois-ci"}</p>
      </div>
    </aside>
  );
}
