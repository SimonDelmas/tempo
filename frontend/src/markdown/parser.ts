import { GFM, InlineContext, MarkdownConfig, parser as baseParser } from "@lezer/markdown";

/**
 * Analyseur Markdown du rendu (chat, widgets de l'éditeur) : le même analyseur lezer que
 * l'éditeur (CommonMark + GFM : tableaux, cases à cocher, barré, liens nus), complété par les
 * liens `[[cible#ancre|alias]]` et les formules `$…$` / `$$…$$` de Tempo.
 */

const BRACKET = 91; // [
const DOLLAR = 36; // $
const BACKSLASH = 92;

const isSpace = (c: number) => c === 32 || c === 9 || c === 10 || c === 13;
const isDigit = (c: number) => c >= 48 && c <= 57;

/** `[[cible]]`, avant les liens Markdown (sinon `[cible]` serait lu comme un lien) */
const wikiLinks: MarkdownConfig = {
  defineNodes: ["WikiLink"],
  parseInline: [{
    name: "WikiLink",
    before: "Link",
    parse(cx: InlineContext, next: number, pos: number) {
      if (next !== BRACKET || cx.char(pos + 1) !== BRACKET) return -1;
      const rest = cx.slice(pos + 2, cx.end);
      const close = rest.indexOf("]]");
      if (close <= 0) return -1;
      const inner = rest.slice(0, close);
      if (inner.includes("\n") || inner.includes("[")) return -1;
      return cx.addElement(cx.elt("WikiLink", pos, pos + close + 4));
    },
  }],
};

/** `$$…$$`, ou `$…$` sans espace intérieur aux bords ni chiffre après (« 5 $ et 10 $ ») */
const math: MarkdownConfig = {
  defineNodes: ["InlineMath", "DisplayMath"],
  parseInline: [{
    name: "Math",
    before: "Emphasis",
    parse(cx: InlineContext, next: number, pos: number) {
      if (next !== DOLLAR || cx.char(pos - 1) === DOLLAR || cx.char(pos - 1) === BACKSLASH) return -1;
      if (cx.char(pos + 1) === DOLLAR) {
        const close = cx.slice(pos + 2, cx.end).indexOf("$$");
        if (close <= 0 || !cx.slice(pos + 2, pos + 2 + close).trim()) return -1;
        return cx.addElement(cx.elt("DisplayMath", pos, pos + close + 4));
      }
      if (isSpace(cx.char(pos + 1))) return -1;
      for (let i = pos + 1; i < cx.end; i++) {
        const c = cx.char(i);
        if (c === 10) return -1;
        if (c === BACKSLASH) { i++; continue; }
        if (c === DOLLAR) {
          if (i === pos + 1 || isSpace(cx.char(i - 1)) || isDigit(cx.char(i + 1)) || cx.char(i + 1) === DOLLAR) return -1;
          return cx.addElement(cx.elt("InlineMath", pos, i + 1));
        }
      }
      return -1;
    },
  }],
};

export const markdownParser = baseParser.configure([GFM, wikiLinks, math]);
