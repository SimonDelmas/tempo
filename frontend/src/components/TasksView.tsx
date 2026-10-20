import { useEffect, useMemo, useState } from "react";
import { api, Config, Task } from "../api";
import BacklogView from "./BacklogView";
import BoardView from "./BoardView";
import { useProjects } from "./ProjectSelect";

export type TasksTab = "backlog" | "board";

type Props = {
  config: Config;
  revision: number;
  /** onglet, porté par l'adresse (#/taches?vue=backlog) */
  tab: TasksTab;
  /** semaine du board (2026-S43), "" pour la semaine courante */
  week: string;
  /** filtre projet, porté par l'adresse (#/taches?projet=…), "-" pour les tâches sans projet */
  project: string;
  onNavigate: (params: { vue?: TasksTab; semaine?: string | null; projet?: string | null }) => void;
  onOpen: (path: string) => void;
  notify: (message: string, error?: boolean) => void;
};

/** Espace Tâches : backlog (un seul ordre pour toutes les tâches) et board par semaine dans le même ordre. */
export default function TasksView({ config, revision, tab, week, project, onNavigate, onOpen, notify }: Props) {
  const [projects, addProject] = useProjects(revision);
  const [tasks, setTasks] = useState<Task[]>([]);
  useEffect(() => { api.tasks().then(setTasks).catch(() => {}); }, [revision]);
  /** projets cités par des tâches sans note dans projets/ */
  const orphans = useMemo(() => [...new Set(tasks.map((t) => t.project).filter(Boolean) as string[])]
    .filter((id) => !projects.some((p) => p.id === id)).sort(), [tasks, projects]);

  const shared = { config, revision, project, projects, orphans, addProject, onOpen, notify };

  return (
    <div className="page tasks-view">
      <header className="page-header">
        <h2>Tâches</h2>
        <div className="view-tabs">
          <button className={tab === "backlog" ? "active" : ""} onClick={() => onNavigate({ vue: "backlog" })}>
            Backlog
          </button>
          <button className={tab === "board" ? "active" : ""} onClick={() => onNavigate({ vue: "board" })}>
            Board
          </button>
        </div>
        <select value={project} onChange={(e) => onNavigate({ projet: e.target.value || null })}>
          <option value="">Tous les projets</option>
          {projects.map((p) => <option key={p.id} value={p.id}>{p.title}</option>)}
          {orphans.map((p) => <option key={p} value={p}>{p} (sans note)</option>)}
          <option value="-">Sans projet</option>
        </select>
      </header>
      {tab === "backlog"
        ? <BacklogView {...shared} onProject={(projet) => onNavigate({ projet: projet || null })} onPlan={(semaine) => onNavigate({ vue: "board", semaine })} />
        : <BoardView {...shared} week={week} onWeek={(semaine) => onNavigate({ semaine })} />}
    </div>
  );
}
