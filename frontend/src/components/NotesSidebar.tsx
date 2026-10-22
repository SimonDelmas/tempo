import { useEffect, useMemo, useState } from "react";
import { api, NoteSummary, SearchResult, SemanticStatus } from "../api";
import { isSpaceFolder } from "../router";
import ProjectSelect, { useProjects } from "./ProjectSelect";

type Props = {
  files: NoteSummary[];
  current: string | null;
  /** recherche sémantique disponible (modèle d'embeddings configuré) */
  semantic: boolean;
  onOpen: (path: string) => void;
  notify: (message: string, error?: boolean) => void;
};

type Tree = { folders: Map<string, Tree>; files: NoteSummary[] };

function buildTree(files: NoteSummary[]): Tree {
  const root: Tree = { folders: new Map(), files: [] };
  for (const file of files) {
    const parts = file.path.split("/");
    let node = root;
    for (const part of parts.slice(0, -1)) {
      if (!node.folders.has(part)) node.folders.set(part, { folders: new Map(), files: [] });
      node = node.folders.get(part)!;
    }
    node.files.push(file);
  }
  return root;
}

// les fichiers datés (journal, réunions…) sont affichés du plus récent au plus ancien
const sortFiles = (files: NoteSummary[]) =>
  [...files].sort((a, b) => {
    const dated = /^\d/.test(a.stem) && /^\d/.test(b.stem);
    return dated ? b.stem.localeCompare(a.stem) : a.title.localeCompare(b.title, "fr");
  });

const sortFolders = (entries: [string, Tree][]) =>
  entries.sort(([a], [b]) => (/^\d+$/.test(a) && /^\d+$/.test(b) ? b.localeCompare(a) : a.localeCompare(b)));

/** Chemins de tous les dossiers de l'arborescence (pour tout déplier). */
function folderKeys(tree: Tree, prefix = ""): string[] {
  return [...tree.folders.entries()].flatMap(([name, sub]) => {
    const key = prefix ? `${prefix}/${name}` : name;
    return [key, ...folderKeys(sub, key)];
  });
}

function TreeNode({ name, tree, depth, prefix, current, expanded, toggle, onOpen }: {
  name: string; tree: Tree; depth: number; prefix: string; current: string | null;
  expanded: Set<string>; toggle: (key: string) => void; onOpen: (path: string) => void;
}) {
  const key = prefix ? `${prefix}/${name}` : name;
  const isCollapsed = !expanded.has(key);
  return (
    <li>
      <button className="tree-folder" style={{ paddingLeft: depth * 12 }} onClick={() => toggle(key)}>
        {isCollapsed ? "▸" : "▾"} {name}
      </button>
      {!isCollapsed && (
        <ul>
          {sortFolders([...tree.folders.entries()]).map(([child, sub]) => (
            <TreeNode key={child} name={child} tree={sub} depth={depth + 1} prefix={key} current={current}
              expanded={expanded} toggle={toggle} onOpen={onOpen} />
          ))}
          {sortFiles(tree.files).map((f) => (
            <li key={f.path}>
              <button className={`tree-file ${f.path === current ? "active" : ""}`}
                style={{ paddingLeft: depth * 12 + 18 }} onClick={() => onOpen(f.path)} title={f.path}>
                {f.alias ? `${f.alias} · ${f.title}` : f.title}
              </button>
            </li>
          ))}
        </ul>
      )}
    </li>
  );
}

export default function NotesSidebar({ files, current, semantic, onOpen, notify }: Props) {
  const [query, setQuery] = useState("");
  const [results, setResults] = useState<SearchResult[]>([]);
  const [byMeaning, setByMeaning] = useState(false);
  const [status, setStatus] = useState<SemanticStatus | null>(null);
  // dossiers dépliés : tout est plié par défaut
  const [expanded, setExpanded] = useState(() => new Set<string>());
  const [creating, setCreating] = useState(false);
  const [newType, setNewType] = useState("reunion");
  const [newTitle, setNewTitle] = useState("");
  const [newProject, setNewProject] = useState("");
  const [projects, addProject] = useProjects(0);
  // tâches, rapports et imputations ont leur propre espace
  const tree = useMemo(() => buildTree(files.filter((f) => !isSpaceFolder(f.path.split("/")[0]))), [files]);

  useEffect(() => {
    if (!query.trim()) return setResults([]);
    const search = byMeaning
      ? () => api.semanticSearch(query).then(setResults).catch((e) => notify(e.message, true))
      : () => api.search(query).then(setResults).catch(() => {});
    const handle = window.setTimeout(search, byMeaning ? 450 : 200);
    return () => window.clearTimeout(handle);
  }, [query, byMeaning, notify]);

  useEffect(() => {
    if (byMeaning) api.semanticStatus().then(setStatus).catch(() => setStatus(null));
  }, [byMeaning]);

  // déplie les dossiers de la note ouverte pour qu'elle reste visible dans l'arborescence
  useEffect(() => {
    if (!current) return;
    const parts = current.split("/").slice(0, -1);
    const ancestors = parts.map((_, i) => parts.slice(0, i + 1).join("/"));
    setExpanded((prev) => (ancestors.every((a) => prev.has(a)) ? prev : new Set([...prev, ...ancestors])));
  }, [current]);

  const toggleAll = () => setExpanded(expanded.size > 0 ? new Set() : new Set(folderKeys(tree)));

  const toggle = (key: string) =>
    setExpanded((prev) => {
      const next = new Set(prev);
      next.has(key) ? next.delete(key) : next.add(key);
      return next;
    });

  const create = async () => {
    try {
      const { path } = await api.createNote(newType, newTitle,
        newType !== "projet" && newProject ? [newProject] : []);
      setCreating(false);
      setNewTitle("");
      setNewProject("");
      onOpen(path);
    } catch (e) {
      notify((e as Error).message, true);
    }
  };

  return (
    <aside className="sidebar">
      <div className="sidebar-title">Notes</div>
      <button className="nav" onClick={() => api.journalToday().then((r) => onOpen(r.path))}>
        Journal du jour
      </button>

      {creating ? (
        <div className="new-note">
          <select value={newType} onChange={(e) => setNewType(e.target.value)}>
            <option value="reunion">Réunion</option>
            <option value="sujet">Sujet</option>
            <option value="projet">Projet</option>
            <option value="personne">Personne</option>
            <option value="ressource">Ressource</option>
            <option value="formation">Formation</option>
          </select>
          <input autoFocus placeholder="Titre" value={newTitle} onChange={(e) => setNewTitle(e.target.value)}
            onKeyDown={(e) => { if (e.key === "Enter") create(); if (e.key === "Escape") setCreating(false); }} />
          {newType !== "projet" && (
            <ProjectSelect projects={projects} value={newProject} onChange={setNewProject} onCreated={addProject}
              notify={notify} />
          )}
          <div className="row">
            <button className="primary" onClick={create}>Créer</button>
            <button className="ghost" onClick={() => setCreating(false)}>Annuler</button>
          </div>
        </div>
      ) : (
        <button className="nav" onClick={() => setCreating(true)}>+ Nouvelle note</button>
      )}

      <div className="search-row">
        <input className="search" value={query} onChange={(e) => setQuery(e.target.value)}
          placeholder={byMeaning ? "Rechercher par le sens…" : "Rechercher…  (Ctrl+P : ouvrir)"} />
        {semantic && (
          <button className={`ghost toggle ${byMeaning ? "on" : ""}`} onClick={() => setByMeaning((b) => !b)}
            title="Recherche par le sens (notes proches même sans les mêmes mots)">≈</button>
        )}
        {!query.trim() && (
          <button className="ghost toggle" onClick={toggleAll}
            title={expanded.size > 0 ? "Plier tous les dossiers" : "Déplier tous les dossiers"}>
            {expanded.size > 0 ? "⊟" : "⊞"}
          </button>
        )}
      </div>
      {byMeaning && status && (
        <p className={`small ${status.error ? "warning" : "muted"}`}>
          {status.error ?? `Index sémantique : ${status.indexed} / ${status.total} notes`}
        </p>
      )}

      {query.trim() ? (
        <ul className="search-results">
          {results.length === 0 && <li className="muted small">Aucun résultat</li>}
          {results.map((r) => (
            <li key={r.path}>
              <button className="link" onClick={() => onOpen(r.path)}>{r.title}</button>
              <div className="muted small">{r.snippet}</div>
            </li>
          ))}
        </ul>
      ) : (
        <ul className="tree">
          {sortFolders([...tree.folders.entries()]).map(([name, sub]) => (
            <TreeNode key={name} name={name} tree={sub} depth={0} prefix="" current={current}
              expanded={expanded} toggle={toggle} onOpen={onOpen} />
          ))}
          {sortFiles(tree.files).map((f) => (
            <li key={f.path}>
              <button className={`tree-file ${f.path === current ? "active" : ""}`} onClick={() => onOpen(f.path)}>
                {f.title}
              </button>
            </li>
          ))}
        </ul>
      )}
    </aside>
  );
}
