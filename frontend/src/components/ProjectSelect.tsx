import { useEffect, useState } from "react";
import { api, Project } from "../api";

/** Projets du vault (notes `projets/*.md`), rechargés à chaque révision. */
export function useProjects(revision: number) {
  const [projects, setProjects] = useState<Project[]>([]);
  useEffect(() => { api.projects().then(setProjects).catch(() => {}); }, [revision]);
  const add = (p: Project) => setProjects((prev) =>
    [...prev.filter((x) => x.id !== p.id), p].sort((a, b) => a.title.localeCompare(b.title, "fr")));
  return [projects, add] as const;
}

/** Titre d'un projet à partir de son identifiant (nom du fichier), l'identifiant à défaut. */
export function projectTitle(projects: Project[], id: string) {
  return projects.find((p) => p.id === id)?.title ?? id;
}

type Props = {
  projects: Project[];
  /** identifiant du projet choisi, "" pour aucun */
  value: string;
  onChange: (id: string) => void;
  /** appelé après la création d'une note de projet */
  onCreated: (project: Project) => void;
  notify: (message: string, error?: boolean) => void;
  emptyLabel?: string;
};

/** Liste déroulante des projets avec un bouton pour en créer un nouveau. */
export default function ProjectSelect({ projects, value, onChange, onCreated, notify, emptyLabel = "Aucun projet" }: Props) {
  const [creating, setCreating] = useState(false);
  const [title, setTitle] = useState("");
  const unknown = value && !projects.some((p) => p.id === value);

  const create = async () => {
    if (!title.trim()) return;
    try {
      const project = await api.createProject(title);
      notify(`Projet « ${project.title} » créé`);
      onCreated(project);
      onChange(project.id);
      setCreating(false);
      setTitle("");
    } catch (e) {
      notify((e as Error).message, true);
    }
  };

  if (creating) {
    return (
      <span className="project-select">
        <input autoFocus placeholder="Nom du projet" value={title} onChange={(e) => setTitle(e.target.value)}
          onKeyDown={(e) => {
            if (e.key === "Enter") create();
            if (e.key === "Escape") setCreating(false);
          }} />
        <button className="primary" onClick={create}>Créer</button>
        <button className="ghost" onClick={() => setCreating(false)}>Annuler</button>
      </span>
    );
  }
  return (
    <span className="project-select">
      <select value={value} onChange={(e) => onChange(e.target.value)}>
        <option value="">{emptyLabel}</option>
        {projects.map((p) => <option key={p.id} value={p.id}>{p.title}</option>)}
        {unknown && <option value={value}>{value} (sans note)</option>}
      </select>
      <button className="mini" title="Nouveau projet" onClick={() => setCreating(true)}>+</button>
    </span>
  );
}
