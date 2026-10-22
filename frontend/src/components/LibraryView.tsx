import { DragEvent, ReactNode, useEffect, useMemo, useRef, useState } from "react";
import { Config, FORMAT_LABELS, library, LibraryHit, Project, Theme, VectorStatus } from "../api";
import DocumentView, { LibraryNav } from "./DocumentView";
import LibraryQueue from "./LibraryQueue";
import { projectTitle, useProjects } from "./ProjectSelect";
import { themeTitle, useThemes } from "./ThemeSelect";

type Notify = (message: string, error?: boolean) => void;

/** paramètres de l'adresse : document ouvert, vue (a_classer), filtres, regroupement (par=projet) */
export type LibraryParams = Record<string, string>;

/** Dépôt de fichiers un par un : progression, refus (format, taille) et bilan. */
function useUpload(config: Config, notify: Notify, onDone: () => void) {
  const [progress, setProgress] = useState<{ done: number; total: number; name: string } | null>(null);
  const running = useRef(false);
  const start = async (files: File[]) => {
    if (!files.length || running.current) return;
    running.current = true;
    const refused: string[] = [];
    const accepted = files.filter((f) => {
      const ext = f.name.includes(".") ? f.name.split(".").pop()!.toLowerCase() : "";
      if (!config.library.formats.includes(ext)) refused.push(`${f.name} : format non pris en charge`);
      else if (f.size > config.library.max_upload) refused.push(`${f.name} : fichier trop volumineux`);
      else return true;
      return false;
    });
    let added = 0;
    for (const [i, file] of accepted.entries()) {
      setProgress({ done: i, total: accepted.length, name: file.name });
      try {
        const result = await library.upload(file);
        added += 1 + result.attachments.length;
        refused.push(...result.warnings);
      } catch (e) {
        refused.push(`${file.name} : ${(e as Error).message}`);
      }
    }
    setProgress(null);
    running.current = false;
    if (added) notify(`${added} document(s) ajouté(s) à la file « À classer »`);
    refused.slice(0, 4).forEach((r) => notify(r, true));
    if (refused.length > 4) notify(`… et ${refused.length - 4} autre(s) refus`, true);
    if (added) onDone();
  };
  return { progress, start };
}

/** Extrait de recherche : les termes trouvés (entre « ») surlignés. */
function Snippet({ text }: { text: string }) {
  const parts = text.split(/«([^»]*)»/);
  return <>{parts.map((p, i) => (i % 2 ? <mark key={i}>{p}</mark> : p))}</>;
}

function DocumentRow({ hit, themes, projects, onNavigate }: {
  hit: LibraryHit;
  themes: Theme[];
  projects: Project[];
  onNavigate: LibraryNav;
}) {
  const where = [hit.section, hit.page ? `p. ${hit.page}` : ""].filter(Boolean).join(" · ");
  return (
    <li className="doc-row">
      <div className="doc-row-head">
        <button className="link strong" onClick={() => onNavigate({ doc: hit.id })}>{hit.title}</button>
        <span className="badge">{FORMAT_LABELS[hit.format] ?? hit.format}</span>
        {hit.pending && <span className="tag high">à classer</span>}
      </div>
      {hit.snippet
        ? <div className="snippet">{where && <span className="muted">{where} — </span>}<Snippet text={hit.snippet} /></div>
        : hit.summary && <div className="summary">{hit.summary}</div>}
      <div className="card-meta small">
        {hit.themes.map((t) => <span key={t} className="muted">{themeTitle(themes, t)}</span>)}
        {hit.projects.map((p) => <span key={p} className="muted">· {projectTitle(projects, p)}</span>)}
        {hit.tags.map((t) => (
          <button key={t} className="tag-chip link" onClick={() => onNavigate({ tag: t, theme: null, projet: null })}>#{t}</button>
        ))}
      </div>
    </li>
  );
}

function LibraryList({ revision, params, onNavigate, onPickFiles }: {
  revision: number;
  params: LibraryParams;
  onNavigate: LibraryNav;
  onPickFiles: () => void;
}) {
  const [query, setQuery] = useState("");
  const [hits, setHits] = useState<LibraryHit[] | null>(null);
  const [themes] = useThemes(revision);
  const [projects] = useProjects(revision);
  const byProject = params.par === "projet";

  useEffect(() => {
    const handle = window.setTimeout(() => {
      library.list({ q: query.trim(), theme: params.theme, project: params.projet, tag: params.tag })
        .then(setHits).catch(() => setHits([]));
    }, query ? 200 : 0);
    return () => window.clearTimeout(handle);
  }, [query, params.theme, params.projet, params.tag, revision]);

  /** regroupement par thème ou par projet (un document à plusieurs thèmes apparaît dans chacun) */
  const groups = useMemo(() => {
    const map = new Map<string, LibraryHit[]>();
    for (const hit of hits ?? []) {
      const keys = byProject ? hit.projects : hit.themes;
      for (const key of keys.length ? keys : ["-"]) map.set(key, [...(map.get(key) ?? []), hit]);
    }
    const title = (key: string) => key === "-" ? (byProject ? "Sans projet" : "Sans thème")
      : byProject ? projectTitle(projects, key) : themeTitle(themes, key);
    return [...map.entries()]
      .map(([key, items]) => ({ key, title: title(key), items }))
      .sort((a, b) => (a.key === "-" ? 1 : 0) - (b.key === "-" ? 1 : 0) || a.title.localeCompare(b.title, "fr"));
  }, [hits, byProject, themes, projects]);

  const filters: [string, string, string][] = [
    ["theme", "Thème", params.theme ? (params.theme === "-" ? "sans thème" : themeTitle(themes, params.theme)) : ""],
    ["projet", "Projet", params.projet ? (params.projet === "-" ? "sans projet" : projectTitle(projects, params.projet)) : ""],
    ["tag", "Tag", params.tag ? `#${params.tag}` : ""],
  ];
  const row = (hit: LibraryHit, key: string) => (
    <DocumentRow key={key} hit={hit} themes={themes} projects={projects} onNavigate={onNavigate} />
  );

  return (
    <div className="page library-list">
      <header className="page-header">
        <h2>Bibliothèque</h2>
        {filters.filter(([, , label]) => label).map(([key, name, label]) => (
          <span key={key} className="tag filter-chip">
            {name} : {label}
            <button className="ghost mini" title="Retirer le filtre" onClick={() => onNavigate({ [key]: null })}>×</button>
          </span>
        ))}
        <span className="spacer" />
        <button onClick={onPickFiles}>Ajouter des fichiers…</button>
      </header>
      <div className="row">
        <input className="library-search" type="search" placeholder="Rechercher par titre, mot-clé ou tag…" value={query}
          onChange={(e) => setQuery(e.target.value)} autoFocus />
        {!query.trim() && (
          <span className="view-tabs">
            <button className={byProject ? "" : "active"} onClick={() => onNavigate({ par: null })}>Par thème</button>
            <button className={byProject ? "active" : ""} onClick={() => onNavigate({ par: "projet" })}>Par projet</button>
          </span>
        )}
      </div>

      {hits === null ? <p className="muted">Chargement…</p> : hits.length === 0 ? (
        <div className="drop-hint">
          {query.trim() || params.theme || params.projet || params.tag
            ? <p className="muted">Aucun document ne correspond.</p>
            : <p>La bibliothèque est vide. Glissez des fichiers depuis l'explorateur Windows (PDF, Word, PowerPoint, Excel,
              mails, Markdown…) ou utilisez « Ajouter des fichiers… ».</p>}
        </div>
      ) : query.trim() ? (
        <>
          <ul className="doc-list">{hits.filter((h) => !h.semantic).map((h) => row(h, h.id))}</ul>
          {hits.some((h) => h.semantic) && (
            <section className="doc-group">
              <h4 title="Documents qui ne contiennent pas les mots cherchés mais en sont proches par le sens">
                Proches par le sens <span className="count">{hits.filter((h) => h.semantic).length}</span>
              </h4>
              <ul className="doc-list semantic">{hits.filter((h) => h.semantic).map((h) => row(h, h.id))}</ul>
            </section>
          )}
        </>
      ) : (
        groups.map((g) => (
          <section key={g.key} className="doc-group">
            <h4>{g.title} <span className="count">{g.items.length}</span></h4>
            <ul className="doc-list">{g.items.map((h) => row(h, `${g.key}/${h.id}`))}</ul>
          </section>
        ))
      )}
    </div>
  );
}

type Props = {
  config: Config;
  revision: number;
  params: LibraryParams;
  onNavigate: LibraryNav;
  notify: Notify;
};

/** Espace Bibliothèque : liste, file « À classer » ou fiche d'un document, et dépôt de fichiers partout. */
export default function LibraryView({ config, revision, params, onNavigate, notify }: Props) {
  const input = useRef<HTMLInputElement>(null);
  const [dragging, setDragging] = useState(false);
  const depth = useRef(0);
  const upload = useUpload(config, notify, () => onNavigate({ doc: null, vue: "a_classer" }));

  const hasFiles = (e: DragEvent) => Array.from(e.dataTransfer.types).includes("Files");
  const pick = () => input.current?.click();

  let content: ReactNode;
  if (params.doc) {
    content = <DocumentView id={params.doc} page={params.page} section={params.section} revision={revision}
      notify={notify} onNavigate={onNavigate} />;
  }
  else if (params.vue === "a_classer") {
    content = <LibraryQueue config={config} revision={revision} notify={notify} onPickFiles={pick}
      onOpenDocument={(doc) => onNavigate({ doc })} />;
  } else content = <LibraryList revision={revision} params={params} onNavigate={onNavigate} onPickFiles={pick} />;

  return (
    <div className="library-view"
      onDragEnter={(e) => { if (hasFiles(e)) { depth.current += 1; setDragging(true); } }}
      onDragOver={(e) => { if (hasFiles(e)) { e.preventDefault(); e.dataTransfer.dropEffect = "copy"; } }}
      onDragLeave={(e) => { if (hasFiles(e) && --depth.current <= 0) { depth.current = 0; setDragging(false); } }}
      onDrop={(e) => {
        if (!hasFiles(e)) return;
        e.preventDefault();
        depth.current = 0;
        setDragging(false);
        upload.start(Array.from(e.dataTransfer.files));
      }}>
      {upload.progress && (
        <div className="upload-progress">
          Conversion {upload.progress.done + 1}/{upload.progress.total} : {upload.progress.name}…
          <span className="share-bar"><span style={{ width: `${(upload.progress.done / upload.progress.total) * 100}%` }} /></span>
        </div>
      )}
      {content}
      {dragging && (
        <div className="drop-overlay">
          <div>Déposez vos fichiers pour les ajouter à la bibliothèque
            <div className="small">{config.library.formats.filter((f) => f !== "htm" && f !== "markdown").join(", ")}</div>
          </div>
        </div>
      )}
      <input ref={input} type="file" multiple hidden accept={config.library.formats.map((f) => `.${f}`).join(",")}
        onChange={(e) => {
          upload.start(Array.from(e.target.files ?? []));
          e.target.value = "";
        }} />
    </div>
  );
}

/** Barre latérale : file « À classer », thèmes et projets avec leur nombre de documents. */
export function LibrarySidebar({ revision, params, pending, onNavigate }: {
  revision: number;
  params: LibraryParams;
  /** documents à classer et fichiers à ingérer */
  pending: number;
  onNavigate: LibraryNav;
}) {
  const [docs, setDocs] = useState<LibraryHit[]>([]);
  const [vectors, setVectors] = useState<VectorStatus | null>(null);
  const [themes] = useThemes(revision);
  const [projects] = useProjects(revision);
  useEffect(() => { library.list().then(setDocs).catch(() => {}); }, [revision]);
  // avancement de la vectorisation, relu tant qu'elle n'est pas terminée
  useEffect(() => {
    let handle = 0;
    const load = () => library.status().then((s) => {
      setVectors(s);
      if (s.enabled && s.embedded < s.total && !s.error) handle = window.setTimeout(load, 5000);
    }).catch(() => {});
    load();
    return () => window.clearTimeout(handle);
  }, [revision]);

  const count = (key: "themes" | "projects", id: string) =>
    docs.filter((d) => (id === "-" ? d[key].length === 0 : d[key].includes(id))).length;
  const home = !params.doc && !params.vue;
  const select = (changes: Record<string, string | null>) =>
    onNavigate({ doc: null, vue: null, theme: null, projet: null, tag: null, ...changes });
  const item = (label: string, active: boolean, n: number, onClick: () => void, key?: string) => (
    <button key={key ?? label} className={`nav ${active ? "active" : ""}`} onClick={onClick}>
      <span className="nav-label">{label}</span><span className="count">{n}</span>
    </button>
  );
  const usedProjects = projects.filter((p) => count("projects", p.id) > 0);

  return (
    <aside className="sidebar">
      <div className="sidebar-title">Bibliothèque</div>
      {item("À classer", params.vue === "a_classer", pending, () => select({ vue: "a_classer" }))}
      {item("Tous les documents", home && !params.theme && !params.projet && !params.tag, docs.length, () => select({}))}
      <h4 className="sidebar-section">Thèmes</h4>
      {themes.length === 0 && <p className="muted small">Créés au classement des documents</p>}
      {themes.map((t) => item(t.title, home && params.theme === t.id, count("themes", t.id),
        () => select({ theme: params.theme === t.id ? null : t.id, par: params.par }), t.id))}
      {count("themes", "-") > 0 && item("Sans thème", home && params.theme === "-", count("themes", "-"),
        () => select({ theme: "-", par: params.par }), "-")}
      <h4 className="sidebar-section">Projets</h4>
      {usedProjects.length === 0 && <p className="muted small">Aucun document rattaché</p>}
      {usedProjects.map((p) => item(p.title, home && params.projet === p.id, count("projects", p.id),
        () => select({ projet: params.projet === p.id ? null : p.id, par: params.par }), p.id))}
      {vectors?.enabled && (
        <p className="muted small sidebar-note" title={vectors.model ?? ""}>
          {vectors.error ? `Recherche par le sens indisponible : ${vectors.error}`
            : vectors.embedded < vectors.total
              ? `Recherche par le sens : ${vectors.embedded} / ${vectors.total} passages analysés…`
              : `Recherche par le sens active (${vectors.total} passages)`}
        </p>
      )}
    </aside>
  );
}
