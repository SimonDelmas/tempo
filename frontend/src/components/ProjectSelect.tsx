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

/** valeur de l'option « Nouveau projet… » de la liste */
const NEW = "__nouveau_projet__";

type Props = {
  projects: Project[];
  /** identifiant du projet choisi, "" pour aucun */
  value: string;
  onChange: (id: string) => void;
  /** appelé après la création d'une note de projet */
  onCreated: (project: Project) => void;
  notify: (message: string, error?: boolean) => void;
  emptyLabel?: string;
  /** identifiants à ne pas proposer (projets déjà choisis d'une liste) */
  exclude?: string[];
};

/**
 * Liste déroulante des projets du vault ; sa dernière option « Nouveau projet… » ouvre la
 * saisie d'un nom et crée la note `projets/<nom>.md`.
 */
export default function ProjectSelect({ projects, value, onChange, onCreated, notify,
  emptyLabel = "Aucun projet", exclude = [] }: Props) {
  const [creating, setCreating] = useState(false);
  const [title, setTitle] = useState("");
  const unknown = value && !projects.some((p) => p.id === value);

  const cancel = () => {
    setCreating(false);
    setTitle("");
  };

  const create = async () => {
    if (!title.trim()) return;
    try {
      const project = await api.createProject(title);
      notify(`Projet « ${project.title} » créé`);
      onCreated(project);
      onChange(project.id);
      cancel();
    } catch (e) {
      notify((e as Error).message, true);
    }
  };

  if (creating) {
    return (
      <span className="project-select">
        <input autoFocus placeholder="Nom du nouveau projet" value={title} onChange={(e) => setTitle(e.target.value)}
          onKeyDown={(e) => {
            if (e.key === "Enter") { e.preventDefault(); create(); }
            if (e.key === "Escape") { e.stopPropagation(); cancel(); }
          }} />
        <button className="primary" disabled={!title.trim()} onClick={create}>Créer</button>
        <button className="ghost" onClick={cancel}>Annuler</button>
      </span>
    );
  }
  return (
    <span className="project-select">
      <select value={value} onChange={(e) => {
        if (e.target.value === NEW) setCreating(true);
        else onChange(e.target.value);
      }}>
        <option value="">{emptyLabel}</option>
        {projects.filter((p) => p.id === value || !exclude.includes(p.id))
          .map((p) => <option key={p.id} value={p.id}>{p.title}</option>)}
        {unknown && <option value={value}>{value} (sans note)</option>}
        <option value={NEW}>+ Nouveau projet…</option>
      </select>
    </span>
  );
}

/** Plusieurs projets : étiquettes retirables et une liste déroulante pour en ajouter. */
export function ProjectsSelect({ projects, value, onChange, onCreated, notify }: Omit<Props, "value" | "onChange"
  | "emptyLabel" | "exclude"> & { value: string[]; onChange: (ids: string[]) => void }) {
  return (
    <span className="projects-select">
      {value.map((id) => (
        <span key={id} className="tag">
          {projectTitle(projects, id)}
          <button className="ghost mini" title="Retirer" onClick={() => onChange(value.filter((x) => x !== id))}>×</button>
        </span>
      ))}
      <ProjectSelect projects={projects} value="" emptyLabel="Ajouter un projet…" exclude={value}
        onChange={(id) => { if (id && !value.includes(id)) onChange([...value, id]); }}
        onCreated={onCreated} notify={notify} />
    </span>
  );
}
