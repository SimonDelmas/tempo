import { ReactNode } from "react";

/** Rendu Markdown minimal des réponses de l'agent : titres, listes, paragraphes, **gras**,
 *  `code` et liens [[…]] cliquables (sans HTML injecté). */

type Props = { text: string; links: Record<string, string>; onOpen: (path: string) => void };

const INLINE_RE = /\*\*([^*]+)\*\*|`([^`]+)`|\[\[([^\]|#\n]+)(?:#[^\]|\n]*)?(?:\|([^\]\n]+))?\]\]/g;

function inline(text: string, links: Props["links"], onOpen: Props["onOpen"]): ReactNode[] {
  const nodes: ReactNode[] = [];
  let last = 0;
  for (const m of text.matchAll(INLINE_RE)) {
    if (m.index > last) nodes.push(text.slice(last, m.index));
    const key = `${m.index}`;
    if (m[1] !== undefined) nodes.push(<strong key={key}>{m[1]}</strong>);
    else if (m[2] !== undefined) nodes.push(<code key={key}>{m[2]}</code>);
    else {
      const name = m[3].trim();
      const path = links[name];
      nodes.push(path
        ? <button key={key} className="wikilink" title={path} onClick={() => onOpen(path)}>{m[4] ?? name}</button>
        : <span key={key} className="wikilink missing" title="Note introuvable">{m[4] ?? name}</span>);
    }
    last = m.index + m[0].length;
  }
  if (last < text.length) nodes.push(text.slice(last));
  return nodes;
}

export default function Markdown({ text, links, onOpen }: Props) {
  const blocks: ReactNode[] = [];
  let list: { ordered: boolean; items: string[] } | null = null;
  let paragraph: string[] = [];

  const flush = () => {
    const key = blocks.length;
    if (paragraph.length) {
      blocks.push(<p key={key}>{inline(paragraph.join(" "), links, onOpen)}</p>);
      paragraph = [];
    }
    if (list) {
      const items = list.items.map((item, i) => <li key={i}>{inline(item, links, onOpen)}</li>);
      blocks.push(list.ordered ? <ol key={`l${key}`}>{items}</ol> : <ul key={`l${key}`}>{items}</ul>);
      list = null;
    }
  };

  for (const line of text.split("\n")) {
    const heading = /^(#{1,4})\s+(.*)$/.exec(line);
    const item = /^\s*(?:([-*])|(\d+)[.)])\s+(.*)$/.exec(line);
    if (!line.trim()) {
      flush();
    } else if (heading) {
      flush();
      blocks.push(<h4 key={blocks.length} className="md-heading">{inline(heading[2], links, onOpen)}</h4>);
    } else if (item) {
      const ordered = item[2] !== undefined;
      if (paragraph.length || (list && list.ordered !== ordered)) flush();
      list ??= { ordered, items: [] };
      list.items.push(item[3]);
    } else if (list && /^\s+/.test(line)) {
      list.items[list.items.length - 1] += " " + line.trim();
    } else {
      if (list) flush();
      paragraph.push(line.trim());
    }
  }
  flush();
  return <div className="markdown">{blocks}</div>;
}
