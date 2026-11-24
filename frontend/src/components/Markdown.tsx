import katex from "katex";
import "katex/dist/katex.min.css";
import type { SyntaxNode } from "@lezer/common";
import { Fragment, ReactNode, useEffect, useMemo, useRef, useState } from "react";
import { Citation } from "../api";
import { highlightInto, mermaidInto } from "../markdown/blocks";
import { markdownParser } from "../markdown/parser";
import { documentHref, navigate } from "../router";

/** Rendu Markdown des réponses de l'agent et des blocs de l'aperçu en direct : l'arbre de
 *  l'analyseur lezer (CommonMark + GFM) est converti en éléments React, sans HTML injecté.
 *  Titres, listes et cases à cocher, citations, tableaux, code coloré, diagrammes Mermaid,
 *  formules KaTeX et liens [[…]] cliquables. Les liens vers des documents de la bibliothèque
 *  ([[document#section]], [[document#p. 12]]) ouvrent le document au passage cité ; une
 *  citation que le serveur n'a pas pu vérifier est signalée. */

/** links : liens résolus en chemins ; null pendant la diffusion (pas encore résolus) */
type Props = {
  text: string;
  links: Record<string, string> | null;
  citations?: Citation[];
  onOpen: (path: string) => void;
};

type Ctx = Omit<Props, "citations"> & { citations: Citation[] };

/** infobulle d'une citation : document, passage, extrait, ou raison de l'échec de la vérification */
export function citationTitle(c: Citation): string {
  const where = [c.title, c.section, c.page ? `p. ${c.page}` : ""].filter(Boolean).join(" › ");
  return c.verified ? `${where}\n« ${c.excerpt} »` : `Citation non vérifiée : ${c.reason}\n${where}`;
}

export const openCitation = (c: Citation) => navigate(documentHref(c.document, { page: c.page, section: c.section }));

/** marques de syntaxe jamais affichées */
const MARKS = new Set(["EmphasisMark", "CodeMark", "HeaderMark", "QuoteMark", "ListMark", "LinkMark",
  "StrikethroughMark", "TableDelimiter", "TaskMarker", "CodeInfo", "LinkTitle", "LinkLabel"]);

const SAFE_URL = /^(https?:|mailto:|www\.)/i;

function childNodes(node: SyntaxNode, name?: string): SyntaxNode[] {
  const out: SyntaxNode[] = [];
  for (let c = node.firstChild; c; c = c.nextSibling) if (!name || c.name === name) out.push(c);
  return out;
}

const href = (url: string) => (/^www\./i.test(url) ? `http://${url}` : url);

function decodeEntity(entity: string): string {
  const area = document.createElement("textarea");
  area.innerHTML = entity;
  return area.value;
}

// ---------------------------------------------------------------------------------------------
// en ligne

/** texte brut entre deux nœuds (les « > » des lignes suivantes d'une citation retirés) */
const plain = (ctx: Ctx, from: number, to: number) =>
  ctx.text.slice(from, to).replace(/\n[ \t]*(?:>[ \t]?)*/g, "\n");

function inlines(ctx: Ctx, node: SyntaxNode, from = node.from, to = node.to): ReactNode[] {
  const out: ReactNode[] = [];
  let pos = from;
  for (let c = node.firstChild; c; c = c.nextSibling) {
    if (c.to <= from || c.from >= to) continue;
    if (c.from > pos) out.push(plain(ctx, pos, c.from));
    const el = inline(ctx, c);
    if (el !== null) out.push(<Fragment key={c.from}>{el}</Fragment>);
    pos = c.to;
  }
  if (pos < to) out.push(plain(ctx, pos, to));
  return out;
}

function inline(ctx: Ctx, node: SyntaxNode): ReactNode {
  const raw = ctx.text.slice(node.from, node.to);
  if (MARKS.has(node.name)) return null;
  switch (node.name) {
    case "Emphasis": return <em>{inlines(ctx, node)}</em>;
    case "StrongEmphasis": return <strong>{inlines(ctx, node)}</strong>;
    case "Strikethrough": return <del>{inlines(ctx, node)}</del>;
    case "InlineCode": {
      const marks = childNodes(node, "CodeMark");
      let code = marks.length >= 2 ? ctx.text.slice(marks[0].to, marks[marks.length - 1].from) : raw;
      code = code.replace(/\n/g, " ");
      if (code.length > 2 && code.startsWith(" ") && code.endsWith(" ")) code = code.slice(1, -1);
      return <code>{code}</code>;
    }
    case "Link":
    case "Image": {
      const marks = childNodes(node, "LinkMark");
      const url = node.getChild("URL");
      const target = url ? ctx.text.slice(url.from, url.to) : "";
      const start = node.name === "Image" ? 2 : 1;
      if (!url || marks.length < 2 || !SAFE_URL.test(target)) return raw;
      const label = inlines(ctx, node, node.from + start, marks[1].from);
      return <a href={href(target)} target="_blank" rel="noreferrer" title={target}>
        {label.length ? label : target}
      </a>;
    }
    case "Autolink":
    case "URL": {
      const url = node.name === "URL" ? raw : raw.slice(1, -1);
      return SAFE_URL.test(url) ? <a href={href(url)} target="_blank" rel="noreferrer">{url}</a> : raw;
    }
    case "WikiLink": return wikiLink(ctx, raw);
    case "InlineMath": return <MathFormula tex={raw.slice(1, -1)} display={false} />;
    case "DisplayMath": return <MathFormula tex={raw.slice(2, -2)} display />;
    case "Escape": return raw.slice(1);
    case "Entity": return decodeEntity(raw);
    case "HardBreak": return <br />;
    default: return raw;
  }
}

function wikiLink(ctx: Ctx, raw: string): ReactNode {
  const inner = raw.slice(2, -2);
  const [target, alias] = inner.split("|", 2);
  const hash = target.indexOf("#");
  const name = (hash >= 0 ? target.slice(0, hash) : target).trim();
  const anchor = hash >= 0 ? target.slice(hash + 1).trim() : "";
  const label = alias?.trim() || (anchor ? `${name} › ${anchor}` : name);
  const citation = ctx.citations.find((c) => c.link === (anchor ? `${name}#${anchor}` : name));
  const path = ctx.links?.[name];
  if (citation) {
    return <button className={`wikilink citation ${citation.verified ? "" : "unverified"}`}
      title={citationTitle(citation)} onClick={() => openCitation(citation)}>
      {citation.verified ? "" : "⚠ "}{label}
    </button>;
  }
  if (path) return <button className="wikilink" title={path} onClick={() => ctx.onOpen(path)}>{label}</button>;
  return <span className={`wikilink ${ctx.links ? "missing" : ""}`} title={ctx.links ? "Note introuvable" : undefined}>
    {label}
  </span>;
}

// ---------------------------------------------------------------------------------------------
// blocs

function blocks(ctx: Ctx, node: SyntaxNode): ReactNode[] {
  return childNodes(node).map((c) => {
    const el = block(ctx, c);
    return el === null ? null : <Fragment key={c.from}>{el}</Fragment>;
  });
}

function block(ctx: Ctx, node: SyntaxNode): ReactNode {
  const raw = ctx.text.slice(node.from, node.to);
  const heading = /^(?:ATX|Setext)Heading(\d)$/.exec(node.name);
  if (heading) {
    const Tag = `h${heading[1]}` as "h1";
    return <Tag className="md-heading">{inlines(ctx, node)}</Tag>;
  }
  if (MARKS.has(node.name)) return null;
  switch (node.name) {
    case "Paragraph": return <p>{inlines(ctx, node)}</p>;
    case "BulletList": return <ul>{blocks(ctx, node)}</ul>;
    case "OrderedList": {
      const mark = node.firstChild?.getChild("ListMark");
      const start = mark ? parseInt(ctx.text.slice(mark.from, mark.to), 10) : 1;
      return <ol start={start !== 1 ? start : undefined}>{blocks(ctx, node)}</ol>;
    }
    case "ListItem": {
      const task = node.getChild("Task");
      return <li className={task ? "md-task" : undefined}>{blocks(ctx, node)}</li>;
    }
    case "Task": {
      const marker = node.getChild("TaskMarker");
      const checked = marker ? ctx.text.slice(marker.from, marker.to).toLowerCase() === "[x]" : false;
      return <p className={checked ? "md-done" : undefined}>
        <input type="checkbox" checked={checked} disabled readOnly /> {inlines(ctx, node)}
      </p>;
    }
    case "Blockquote": return <blockquote>{blocks(ctx, node)}</blockquote>;
    case "FencedCode": {
      const info = node.getChild("CodeInfo");
      const lang = info ? ctx.text.slice(info.from, info.to).trim().split(/\s+/)[0].toLowerCase() : "";
      const body = node.getChild("CodeText");
      const code = body ? ctx.text.slice(body.from, body.to) : "";
      // pendant la diffusion, un diagramme n'est dessiné qu'une fois son bloc fermé
      const closed = childNodes(node, "CodeMark").length >= 2;
      if (lang === "mermaid" && closed && code.trim()) return <MermaidBlock code={code} />;
      return <CodeBlock code={code} lang={lang} />;
    }
    case "CodeBlock": {
      const code = raw.split("\n").map((l) => l.replace(/^(?: {1,4}|\t)/, "")).join("\n");
      return <CodeBlock code={code} lang="" />;
    }
    case "HorizontalRule": return <hr />;
    case "Table": return <Table ctx={ctx} node={node} />;
    case "LinkReference": return null;
    default: return <p>{raw}</p>;
  }
}

type Align = "left" | "center" | "right" | undefined;

function alignments(delimiter: string): Align[] {
  return delimiter.trim().replace(/^\|/, "").replace(/\|$/, "").split("|").map((cell) => {
    const c = cell.trim();
    if (c.startsWith(":") && c.endsWith(":")) return "center";
    if (c.endsWith(":")) return "right";
    if (c.startsWith(":")) return "left";
    return undefined;
  });
}

/** cellules d'une ligne, y compris les cellules vides (qui n'ont pas de nœud) */
function rowCells(row: SyntaxNode): (SyntaxNode | null)[] {
  const cells: (SyntaxNode | null)[] = [];
  let pending: SyntaxNode | null = null;
  let index = 0;
  for (let c = row.firstChild; c; c = c.nextSibling, index++) {
    if (c.name === "TableDelimiter") {
      if (index > 0 || pending) cells.push(pending);
      pending = null;
    } else if (c.name === "TableCell") {
      pending = c;
    }
  }
  if (pending) cells.push(pending);
  return cells;
}

/** tableau GFM ; `data-row` / `data-col` situent chaque cellule (0 = ligne d'en-tête) */
function Table({ ctx, node }: { ctx: Ctx; node: SyntaxNode }) {
  const header = node.getChild("TableHeader");
  const rows = childNodes(node, "TableRow");
  const delimiter = childNodes(node, "TableDelimiter")[0];
  const aligns = delimiter ? alignments(ctx.text.slice(delimiter.from, delimiter.to)) : [];
  const head = header ? rowCells(header) : [];
  const width = Math.max(head.length, aligns.length);
  const columns = Array.from({ length: width }, (_, i) => i);
  const cell = (cells: (SyntaxNode | null)[], row: number, col: number, Tag: "th" | "td") => {
    const c = cells[col];
    return <Tag key={col} style={aligns[col] ? { textAlign: aligns[col] } : undefined} data-row={row} data-col={col}>
      {c ? inlines(ctx, c) : null}
    </Tag>;
  };
  return (
    <div className="md-table-wrap">
      <table className="md-table">
        <thead><tr>{columns.map((col) => cell(head, 0, col, "th"))}</tr></thead>
        <tbody>
          {rows.map((row, i) => {
            const cells = rowCells(row);
            return <tr key={row.from}>{columns.map((col) => cell(cells, i + 1, col, "td"))}</tr>;
          })}
        </tbody>
      </table>
    </div>
  );
}

function CodeBlock({ code, lang }: { code: string; lang: string }) {
  const ref = useRef<HTMLElement>(null);
  const [copied, setCopied] = useState(false);
  useEffect(() => {
    if (ref.current) void highlightInto(ref.current, code, lang);
  }, [code, lang]);
  const copy = () => {
    void navigator.clipboard.writeText(code).then(() => {
      setCopied(true);
      window.setTimeout(() => setCopied(false), 1500);
    });
  };
  return (
    <div className="md-code">
      <div className="md-code-bar">
        <span>{lang}</span>
        <button className="link" onMouseDown={(e) => e.stopPropagation()} onClick={copy}>
          {copied ? "Copié" : "Copier"}
        </button>
      </div>
      {/* contenu écrit par highlightInto (et non par React) */}
      <pre><code ref={ref} /></pre>
    </div>
  );
}

function MermaidBlock({ code }: { code: string }) {
  const ref = useRef<HTMLDivElement>(null);
  useEffect(() => {
    if (ref.current) void mermaidInto(ref.current, code);
  }, [code]);
  return <div ref={ref} className="md-mermaid" />;
}

function MathFormula({ tex, display }: { tex: string; display: boolean }) {
  const ref = useRef<HTMLSpanElement>(null);
  useEffect(() => {
    if (ref.current) katex.render(tex, ref.current, { displayMode: display, throwOnError: false });
  }, [tex, display]);
  return <span ref={ref} className={display ? "md-math md-math-block" : "md-math"} />;
}

export default function Markdown({ text, links, citations = [], onOpen }: Props) {
  const tree = useMemo(() => markdownParser.parse(text), [text]);
  const ctx: Ctx = { text, links, citations, onOpen };
  return <div className="markdown">{blocks(ctx, tree.topNode)}</div>;
}
