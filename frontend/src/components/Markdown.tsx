import { ReactNode } from "react";
import { Citation } from "../api";
import { documentHref, navigate } from "../router";

/** Rendu Markdown minimal des réponses de l'agent : titres, listes, paragraphes, **gras**,
 *  `code` et liens [[…]] cliquables (sans HTML injecté). Les liens vers des documents de la
 *  bibliothèque ([[document#section]], [[document#p. 12]]) ouvrent le document au passage cité ;
 *  une citation que le serveur n'a pas pu vérifier est signalée. */

/** links : liens résolus en chemins ; null pendant la diffusion (pas encore résolus) */
type Props = {
  text: string;
  links: Record<string, string> | null;
  citations?: Citation[];
  onOpen: (path: string) => void;
};

const INLINE_RE = /\*\*([^*]+)\*\*|`([^`]+)`|\[\[([^\]|#\n]+)(?:#([^\]|\n]*))?(?:\|([^\]\n]+))?\]\]/g;

/** infobulle d'une citation : document, passage, extrait, ou raison de l'échec de la vérification */
export function citationTitle(c: Citation): string {
  const where = [c.title, c.section, c.page ? `p. ${c.page}` : ""].filter(Boolean).join(" › ");
  return c.verified ? `${where}\n« ${c.excerpt} »` : `Citation non vérifiée : ${c.reason}\n${where}`;
}

export const openCitation = (c: Citation) => navigate(documentHref(c.document, { page: c.page, section: c.section }));

function inline(text: string, links: Props["links"], citations: Citation[], onOpen: Props["onOpen"]): ReactNode[] {
  const nodes: ReactNode[] = [];
  let last = 0;
  for (const m of text.matchAll(INLINE_RE)) {
    if (m.index > last) nodes.push(text.slice(last, m.index));
    const key = `${m.index}`;
    if (m[1] !== undefined) nodes.push(<strong key={key}>{m[1]}</strong>);
    else if (m[2] !== undefined) nodes.push(<code key={key}>{m[2]}</code>);
    else {
      const name = m[3].trim();
      const anchor = m[4]?.trim();
      const label = m[5] ?? (anchor ? `${name} › ${anchor}` : name);
      const citation = citations.find((c) => c.link === (anchor ? `${name}#${anchor}` : name));
      const path = links?.[name];
      if (citation) {
        nodes.push(
          <button key={key} className={`wikilink citation ${citation.verified ? "" : "unverified"}`}
            title={citationTitle(citation)} onClick={() => openCitation(citation)}>
            {citation.verified ? "" : "⚠ "}{label}
          </button>,
        );
      } else if (path) {
        nodes.push(<button key={key} className="wikilink" title={path} onClick={() => onOpen(path)}>{label}</button>);
      } else {
        nodes.push(
          <span key={key} className={`wikilink ${links ? "missing" : ""}`} title={links ? "Note introuvable" : undefined}>
            {label}
          </span>,
        );
      }
    }
    last = m.index + m[0].length;
  }
  if (last < text.length) nodes.push(text.slice(last));
  return nodes;
}

export default function Markdown({ text, links, citations = [], onOpen }: Props) {
  const blocks: ReactNode[] = [];
  let list: { ordered: boolean; items: string[] } | null = null;
  let paragraph: string[] = [];
  const render = (t: string) => inline(t, links, citations, onOpen);

  const flush = () => {
    const key = blocks.length;
    if (paragraph.length) {
      blocks.push(<p key={key}>{render(paragraph.join(" "))}</p>);
      paragraph = [];
    }
    if (list) {
      const items = list.items.map((item, i) => <li key={i}>{render(item)}</li>);
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
      blocks.push(<h4 key={blocks.length} className="md-heading">{render(heading[2])}</h4>);
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
