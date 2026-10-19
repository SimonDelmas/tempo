import { useEffect, useMemo, useState } from "react";
import { api, NoteSummary, SearchResult } from "../api";
import { isSpaceFolder } from "../router";

type Props = {
  files: NoteSummary[];
  current: string | null;
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

function TreeNode({ name, tree, depth, prefix, current, collapsed, toggle, onOpen }: {
  name: string; tree: Tree; depth: number; prefix: string; current: string | null;
  collapsed: Set<string>; toggle: (key: string) => void; onOpen: (path: string) => void;
}) {
  const key = prefix ? `${prefix}/${name}` : name;
  const isCollapsed = collapsed.has(key);
  return (
    <li>
      <button className="tree-folder" style={{ paddingLeft: depth * 12 }} onClick={() => toggle(key)}>
        {isCollapsed ? "▸" : "▾"} {name}
      </button>
      {!isCollapsed && (
        <ul>
          {sortFolders([...tree.folders.entries()]).map(([child, sub]) => (
            <TreeNode key={child} name={child} tree={sub} depth={depth + 1} prefix={key} current={current}
              collapsed={collapsed} toggle={toggle} onOpen={onOpen} />
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

const DEFAULT_COLLAPSED = ["_templates", "echanges", "00-inbox"];

export default function NotesSidebar({ files, current, onOpen, notify }: Props) {
  const [query, setQuery] = useState("");
  const [results, setResults] = useState<SearchResult[]>([]);
  const [collapsed, setCollapsed] = useState(() => new Set(DEFAULT_COLLAPSED));
  const [creating, setCreating] = useState(false);
  const [newType, setNewType] = useState("reunion");
  const [newTitle, setNewTitle] = useState("");
  // tâches, rapports et imputations ont leur propre espace
  const tree = useMemo(() => buildTree(files.filter((f) => !isSpaceFolder(f.path.split("/")[0]))), [files]);

  useEffect(() => {
    if (!query.trim()) return setResults([]);
    const handle = window.setTimeout(() => api.search(query).then(setResults).catch(() => {}), 200);
    return () => window.clearTimeout(handle);
  }, [query]);

  const toggle = (key: string) =>
    setCollapsed((prev) => {
      const next = new Set(prev);
      next.has(key) ? next.delete(key) : next.add(key);
      return next;
    });

  const create = async () => {
    try {
      const { path } = await api.createNote(newType, newTitle);
      setCreating(false);
      setNewTitle("");
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
          <div className="row">
            <button className="primary" onClick={create}>Créer</button>
            <button className="ghost" onClick={() => setCreating(false)}>Annuler</button>
          </div>
        </div>
      ) : (
        <button className="nav" onClick={() => setCreating(true)}>+ Nouvelle note</button>
      )}

      <input className="search" placeholder="Rechercher…  (Ctrl+P : ouvrir)" value={query}
        onChange={(e) => setQuery(e.target.value)} />

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
              collapsed={collapsed} toggle={toggle} onOpen={onOpen} />
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
