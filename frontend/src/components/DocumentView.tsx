import { ReactNode, useEffect, useMemo, useState } from "react";
import { DocumentMeta, fileSize, FORMAT_LABELS, library, LibraryDetail } from "../api";
import { metaOf, ClassificationForm } from "./LibraryQueue";
import { projectTitle, useProjects } from "./ProjectSelect";
import { themeTitle, useTags, useThemes } from "./ThemeSelect";

type Notify = (message: string, error?: boolean) => void;
export type LibraryNav = (changes: Record<string, string | null>) => void;

const INLINE_RE = /\*\*([^*]+)\*\*|\*([^*\s][^*]*)\*|`([^`]+)`|\[([^\]]+)\]\((https?:[^)\s]+)\)/g;

/** Texte en ligne : gras, italique, code, liens externes ; balises HTML résiduelles retirées. */
function inline(text: string): ReactNode[] {
  const clean = text.replace(/<[^>]+>/g, " ");
  const nodes: ReactNode[] = [];
  let last = 0;
  for (const m of clean.matchAll(INLINE_RE)) {
    if (m.index > last) nodes.push(clean.slice(last, m.index));
    const key = m.index;
    if (m[1] !== undefined) nodes.push(<strong key={key}>{m[1]}</strong>);
    else if (m[2] !== undefined) nodes.push(<em key={key}>{m[2]}</em>);
    else if (m[3] !== undefined) nodes.push(<code key={key}>{m[3]}</code>);
    else nodes.push(<a key={key} href={m[5]} target="_blank" rel="noreferrer">{m[4]}</a>);
    last = m.index + m[0].length;
  }
  if (last < clean.length) nodes.push(clean.slice(last));
  return nodes;
}

const HEADING_RE = /^(#{1,6})\s+(.+?)\s*#*\s*$/;
const PAGE_RE = /^<!--\s*page\s+(\d+)\s*-->$/;
const ITEM_RE = /^\s*(?:[-*+]|(\d+)[.)])\s+(.*)$/;
const SEPARATOR_RE = /^\|?\s*:?-{3,}:?\s*(\|\s*:?-{3,}:?\s*)*\|?$/;

/** Titres du texte converti, hors blocs de code (dans l'ordre des ancres `h-N`). */
export function outlineOf(text: string) {
  const entries: { level: number; title: string }[] = [];
  let code = false;
  for (const line of text.split("\n")) {
    const trimmed = line.trim();
    if (trimmed.startsWith("```")) code = !code;
    else if (!code) {
      const m = HEADING_RE.exec(trimmed);
      if (m) entries.push({ level: m[1].length, title: m[2] });
    }
  }
  return entries;
}

const cells = (row: string) => row.trim().replace(/^\|/, "").replace(/\|$/, "").split(/(?<!\\)\|/).map((c) => c.trim().replace(/\\\|/g, "|"));

/** Rendu du texte converti : titres (ancres), marqueurs de page, tableaux, listes, citations, code. */
function DocumentText({ text }: { text: string }) {
  const blocks: ReactNode[] = [];
  const lines = text.split("\n");
  let heading = 0;
  let i = 0;
  while (i < lines.length) {
    const trimmed = lines[i].trim();
    const key = blocks.length;
    if (!trimmed) { i++; continue; }
    const page = PAGE_RE.exec(trimmed);
    const title = HEADING_RE.exec(trimmed);
    if (page) {
      blocks.push(<div key={key} className="page-marker" id={`page-${page[1]}`}>Page {page[1]}</div>);
      i++;
    } else if (title) {
      const Tag = `h${Math.min(title[1].length + 1, 6)}` as "h2";
      blocks.push(<Tag key={key} id={`h-${heading++}`} className="doc-heading">{inline(title[2])}</Tag>);
      i++;
    } else if (trimmed.startsWith("```")) {
      const code: string[] = [];
      i++;
      while (i < lines.length && !lines[i].trim().startsWith("```")) code.push(lines[i++]);
      i++;
      blocks.push(<pre key={key} className="raw">{code.join("\n")}</pre>);
    } else if (trimmed.startsWith("|") && SEPARATOR_RE.test(lines[i + 1]?.trim() ?? "")) {
      const head = cells(trimmed);
      const rows: string[][] = [];
      i += 2;
      while (i < lines.length && lines[i].trim().startsWith("|")) rows.push(cells(lines[i++]));
      blocks.push(
        <div key={key} className="table-wrap">
          <table className="data-table">
            <thead><tr>{head.map((c, j) => <th key={j}>{inline(c)}</th>)}</tr></thead>
            <tbody>{rows.map((r, j) => <tr key={j}>{r.map((c, k) => <td key={k}>{inline(c)}</td>)}</tr>)}</tbody>
          </table>
        </div>,
      );
    } else if (ITEM_RE.test(lines[i])) {
      const ordered = ITEM_RE.exec(lines[i])![1] !== undefined;
      const items: string[] = [];
      while (i < lines.length && lines[i].trim()) {
        const m = ITEM_RE.exec(lines[i]);
        if (m) items.push(m[2]);
        else items[items.length - 1] += " " + lines[i].trim();
        i++;
      }
      const list = items.map((item, j) => <li key={j}>{inline(item)}</li>);
      blocks.push(ordered ? <ol key={key}>{list}</ol> : <ul key={key}>{list}</ul>);
    } else if (trimmed.startsWith(">")) {
      const quote: string[] = [];
      while (i < lines.length && lines[i].trim().startsWith(">")) quote.push(lines[i++].trim().replace(/^>\s?/, ""));
      blocks.push(<blockquote key={key}>{inline(quote.join(" "))}</blockquote>);
    } else {
      const paragraph: string[] = [];
      while (i < lines.length && lines[i].trim() && !HEADING_RE.test(lines[i].trim()) && !PAGE_RE.test(lines[i].trim())
        && !lines[i].trim().startsWith("```") && !(lines[i].trim().startsWith("|") && paragraph.length === 0)) {
        paragraph.push(lines[i++].trim());
      }
      if (!paragraph.length) paragraph.push(lines[i++].trim());
      blocks.push(<p key={key}>{inline(paragraph.join(" "))}</p>);
    }
  }
  return <div className="document-text">{blocks}</div>;
}

type Props = {
  id: string;
  revision: number;
  notify: Notify;
  onNavigate: LibraryNav;
};

/** Fiche d'un document : classement, texte converti, original (PDF) et actions. */
export default function DocumentView({ id, revision, notify, onNavigate }: Props) {
  const [detail, setDetail] = useState<LibraryDetail | null>(null);
  const [missing, setMissing] = useState(false);
  const [tab, setTab] = useState<"texte" | "original">("texte");
  const [editing, setEditing] = useState<DocumentMeta | null>(null);
  const [busy, setBusy] = useState(false);
  const [themes, addTheme] = useThemes(revision);
  const [projects, addProject] = useProjects(revision);
  const tags = useTags(revision);

  useEffect(() => {
    library.get(id).then((d) => { setDetail(d); setMissing(false); }).catch(() => setMissing(true));
  }, [id, revision]);
  // un PDF s'ouvre sur l'original, les autres formats sur le texte converti
  useEffect(() => { setTab("texte"); setEditing(null); }, [id]);
  useEffect(() => { if (detail?.id === id && detail.format === "pdf") setTab("original"); }, [detail?.id]); // eslint-disable-line react-hooks/exhaustive-deps

  const outline = useMemo(() => outlineOf(detail?.content ?? ""), [detail?.content]);

  if (missing) return <div className="page"><p className="muted">Document introuvable : {id}</p></div>;
  if (!detail) return <div className="loading">Chargement…</div>;

  const run = async (action: () => Promise<void>) => {
    setBusy(true);
    try {
      await action();
    } catch (e) {
      notify((e as Error).message, true);
    } finally {
      setBusy(false);
    }
  };
  const save = () => run(async () => {
    if (!editing) return;
    const saved = await library.update(id, { ...editing, replaces: null });
    setDetail({ ...detail, ...saved });
    setEditing(null);
    notify("Classement enregistré");
  });
  const reconvert = () => run(async () => {
    const saved = await library.reconvert(id);
    setDetail(await library.get(saved.id));
    notify("Texte régénéré à partir du fichier d'origine");
  });
  const remove = () => run(async () => {
    if (!window.confirm(`Supprimer « ${detail.title} » ? (fichiers déplacés dans .trash)`)) return;
    await library.remove(id);
    notify("Document supprimé");
    onNavigate({ doc: null });
  });
  const scrollTo = (anchor: string) => {
    setTab("texte");
    window.setTimeout(() => document.getElementById(anchor)?.scrollIntoView({ behavior: "smooth", block: "start" }), 0);
  };
  const unit = detail.format.startsWith("ppt") ? "diapositives" : "pages";
  const sourceName = detail.source.split("/").pop();

  return (
    <div className="page document-view">
      <header className="page-header">
        <h2>{detail.title}</h2>
        <span className="badge">{FORMAT_LABELS[detail.format] ?? detail.format}</span>
        {detail.pending && <button className="tag high link" onClick={() => onNavigate({ doc: null, vue: "a_classer" })}>à classer</button>}
      </header>
      <div className="row document-actions">
        <button onClick={() => run(() => library.open(id).then(() => undefined))} disabled={busy}>Ouvrir le fichier</button>
        <button className="ghost" onClick={() => run(() => library.open(id, true).then(() => undefined))} disabled={busy}>
          Afficher dans l'explorateur
        </button>
        <button className="ghost" onClick={() => setEditing(editing ? null : metaOf(detail))} disabled={busy}>
          {editing ? "Annuler la modification" : "Modifier le classement"}
        </button>
        <button className="ghost" onClick={reconvert} disabled={busy} title="Régénère le texte converti sans toucher au classement">
          Reconvertir
        </button>
        <span className="spacer" />
        <button className="ghost danger" onClick={remove} disabled={busy}>Supprimer</button>
      </div>

      {editing ? (
        <section className="panel">
          <ClassificationForm value={editing} onChange={setEditing} themes={themes} onThemeCreated={addTheme}
            projects={projects} onProjectCreated={addProject} tags={tags} notify={notify} />
          <div className="row">
            <button className="primary" onClick={save} disabled={busy || !editing.title.trim()}>Enregistrer</button>
          </div>
        </section>
      ) : (
        <section className="document-meta">
          {detail.summary && <p className="document-summary">{detail.summary}</p>}
          <div className="card-meta">
            {detail.themes.map((t) => (
              <button key={t} className="tag theme link" onClick={() => onNavigate({ doc: null, theme: t, projet: null, tag: null })}>
                {themeTitle(themes, t)}
              </button>
            ))}
            {detail.projects.map((p) => (
              <button key={p} className="tag project link" onClick={() => onNavigate({ doc: null, projet: p, theme: null, tag: null })}>
                {projectTitle(projects, p)}
              </button>
            ))}
            {detail.tags.map((t) => (
              <button key={t} className="tag-chip link" onClick={() => onNavigate({ doc: null, tag: t, theme: null, projet: null })}>#{t}</button>
            ))}
          </div>
          <div className="small muted">
            {sourceName} · {fileSize(detail.size)}{detail.pages ? ` · ${detail.pages} ${unit}` : ""} · ajouté le {detail.added}
            {detail.updated ? ` · nouvelle version le ${detail.updated}` : ""}
            {detail.attached_to ? ` · pièce jointe de ${detail.attached_to.replace(/^\[\[|\]\]$/g, "")}` : ""}
            {" · "}lien : <code>[[{detail.id}]]</code>
          </div>
        </section>
      )}

      <div className="view-tabs document-tabs">
        <button className={tab === "texte" ? "active" : ""} onClick={() => setTab("texte")}>Texte converti</button>
        <button className={tab === "original" ? "active" : ""} onClick={() => setTab("original")}>Original</button>
      </div>
      {tab === "original" ? (
        detail.format === "pdf" || detail.format === "txt" || detail.format === "md"
          ? <iframe className="source-frame" src={library.sourceUrl(id)} title={detail.title} />
          : (
            <p className="muted">
              Aperçu non disponible dans l'application pour ce format.{" "}
              <button className="link" onClick={() => run(() => library.open(id).then(() => undefined))}>Ouvrir le fichier</button>
            </p>
          )
      ) : (
        <div className="document-body">
          {outline.length > 1 && (
            <nav className="document-outline">
              <h4>Sommaire</h4>
              {outline.map((entry, i) => (
                <button key={i} className="link" style={{ paddingLeft: (entry.level - 1) * 12 }} onClick={() => scrollTo(`h-${i}`)}>
                  {entry.title}
                </button>
              ))}
            </nav>
          )}
          <DocumentText text={detail.content} />
        </div>
      )}
    </div>
  );
}
