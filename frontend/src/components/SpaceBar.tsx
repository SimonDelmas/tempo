import { goBack, goForward, MENU, MenuId, shortcutLabel, useNavHistory } from "../router";
import type { TasksTab } from "./TasksView";

type Props = {
  current: MenuId;
  badges: Partial<Record<MenuId, number>>;
  /** entrées où un traitement tourne en arrière-plan (réponse de l'agent en cours) */
  busy?: Partial<Record<MenuId, boolean>>;
  shortcuts: { back: string; forward: string };
  /** vue affichée dans l'espace Tâches, null ailleurs ou sur une fiche */
  tasksTab: TasksTab | null;
  onSelect: (entry: MenuId) => void;
  onTasksTab: (tab: TasksTab) => void;
};

const ICONS: Record<MenuId, string> = {
  journal: "M5 3h14v18H5z M9 3v18 M12 8h4 M12 12h4",
  taches: "M4 5h4v4H4z M4 15h4v4H4z M11 7h9 M11 17h9 M5 7l1 1 2-2",
  imputations: "M12 3a9 9 0 1 0 0 18a9 9 0 1 0 0-18 M12 7v5l3 2",
  brouillons: "M4 20h4l11-11-4-4L4 16z M13 7l4 4 M4 20h16",
  bibliotheque: "M4 4h4v16H4z M10 4h4v16h-4z M15.5 5.5l3.8-1.2 3.4 14.4-3.8 1.2z M4 8h4 M10 8h4",
};

const TASKS_MENU: { id: TasksTab; label: string }[] = [
  { id: "backlog", label: "Backlog" },
  { id: "board", label: "Board" },
];

export default function SpaceBar({ current, badges, busy = {}, shortcuts, tasksTab, onSelect, onTasksTab }: Props) {
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
      {MENU.map((s, i) => (
        <div key={s.id} className="space-entry">
          <button className={`space ${current === s.id ? "active" : ""}`}
            onClick={() => onSelect(s.id)} title={`${s.label} (Alt+${i + 1})`}>
            <svg viewBox="0 0 24 24" aria-hidden="true"><path d={ICONS[s.id]} /></svg>
            <span>{s.label}</span>
            {!!badges[s.id] && <span className="space-badge">{badges[s.id]}</span>}
            {busy[s.id] && !badges[s.id] && <span className="space-busy" title="Réponse de l'agent en cours" />}
          </button>
          {s.id === "taches" && (
            <div className="space-menu">
              {TASKS_MENU.map((t) => (
                <button key={t.id} className={tasksTab === t.id ? "active" : ""} onClick={() => onTasksTab(t.id)}>
                  {t.label}
                </button>
              ))}
            </div>
          )}
        </div>
      ))}
    </nav>
  );
}
