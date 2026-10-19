import katex from "katex";
import "katex/dist/katex.min.css";
import { syntaxTree } from "@codemirror/language";
import { EditorState, Range, StateEffect, StateField } from "@codemirror/state";
import { Decoration, DecorationSet, EditorView, WidgetType } from "@codemirror/view";

/**
 * Formules mathématiques de l'aperçu en direct : `$…$` (en ligne) et `$$…$$` (bloc, sur une
 * ou plusieurs lignes) sont rendues par KaTeX hors des lignes où se trouve le curseur.
 * Un champ d'état (et non un plugin de vue) parce que les blocs remplacent des sauts de ligne.
 */

export type MathRange = { from: number; to: number; tex: string; display: boolean; block: boolean };

/** `$$…$$`, ou `$…$` sans espace intérieur aux bords ni chiffre après (« 5 $ et 10 $ ») */
const MATH_RE = /\$\$([\s\S]+?)\$\$|(?<![\\$])\$(?=[^\s$])((?:[^$\n\\]|\\.)*?[^\s\\$])\$(?![\d$])/g;

/** nœuds Markdown dans lesquels un `$` reste du texte */
const LITERAL_NODES = new Set(["InlineCode", "FencedCode", "CodeBlock", "HTMLBlock", "CommentBlock", "URL"]);

function inLiteral(state: EditorState, pos: number): boolean {
  for (let node: ReturnType<typeof syntaxTree>["topNode"] | null = syntaxTree(state).resolveInner(pos, 1);
    node; node = node.parent) {
    if (LITERAL_NODES.has(node.name)) return true;
  }
  return false;
}

/** fin du frontmatter YAML (jamais décoré), ou 0 */
export function frontmatterEnd(state: EditorState): number {
  const doc = state.doc;
  if (doc.lines < 2 || doc.line(1).text.trim() !== "---") return 0;
  for (let n = 2; n <= doc.lines; n++) {
    if (doc.line(n).text.trim() === "---") return doc.line(n).to;
  }
  return 0;
}

function findMath(state: EditorState): MathRange[] {
  const text = state.doc.toString();
  const skipUntil = frontmatterEnd(state);
  const found: MathRange[] = [];
  for (const match of text.matchAll(MATH_RE)) {
    const from = match.index;
    const to = from + match[0].length;
    if (from < skipUntil || inLiteral(state, from)) continue;
    const display = match[1] !== undefined;
    const tex = (display ? match[1] : match[2]).trim();
    if (!tex) continue;
    // bloc : le `$$…$$` occupe ses lignes entières
    const first = state.doc.lineAt(from);
    const last = state.doc.lineAt(to);
    const block = display && !first.text.slice(0, from - first.from).trim()
      && !last.text.slice(to - last.from).trim();
    found.push({ from: block ? first.from : from, to: block ? last.to : to, tex, display, block });
  }
  return found;
}

class MathWidget extends WidgetType {
  constructor(readonly tex: string, readonly display: boolean) { super(); }
  eq(other: MathWidget) { return other.tex === this.tex && other.display === this.display; }
  toDOM(view: EditorView) {
    const el = document.createElement(this.display ? "div" : "span");
    el.className = this.display ? "cm-lp-math cm-lp-math-block" : "cm-lp-math";
    katex.render(this.tex, el, { displayMode: this.display, throwOnError: false });
    // un clic place le curseur sur la formule, ce qui réaffiche sa source
    el.addEventListener("mousedown", (e) => {
      e.preventDefault();
      view.dispatch({ selection: { anchor: view.posAtDOM(el) } });
      view.focus();
    });
    return el;
  }
  ignoreEvent() { return true; }
}

const setFocus = StateEffect.define<boolean>();

type MathState = { ranges: MathRange[]; focused: boolean; decorations: DecorationSet };

function decorate(state: EditorState, ranges: MathRange[], focused: boolean): DecorationSet {
  const active = new Set<number>();
  if (focused) {
    for (const range of state.selection.ranges) {
      const first = state.doc.lineAt(range.from).number;
      const last = state.doc.lineAt(range.to).number;
      for (let n = first; n <= last; n++) active.add(n);
    }
  }
  const decorations: Range<Decoration>[] = [];
  for (const m of ranges) {
    const first = state.doc.lineAt(m.from).number;
    const last = state.doc.lineAt(m.to).number;
    let touched = false;
    for (let n = first; n <= last && !touched; n++) touched = active.has(n);
    if (touched) continue;
    decorations.push(Decoration.replace({ widget: new MathWidget(m.tex, m.display), block: m.block })
      .range(m.from, m.to));
  }
  return Decoration.set(decorations, true);
}

export const mathField = StateField.define<MathState>({
  create(state) {
    const ranges = findMath(state);
    return { ranges, focused: false, decorations: decorate(state, ranges, false) };
  },
  update(value, tr) {
    let { ranges, focused } = value;
    for (const effect of tr.effects) if (effect.is(setFocus)) focused = effect.value;
    const reparsed = syntaxTree(tr.state) !== syntaxTree(tr.startState);
    if (tr.docChanged || reparsed) ranges = findMath(tr.state);
    if (ranges === value.ranges && focused === value.focused && !tr.selection) return value;
    return { ranges, focused, decorations: decorate(tr.state, ranges, focused) };
  },
  provide: (field) => [
    EditorView.decorations.from(field, (value) => value.decorations),
    EditorView.atomicRanges.of((view) => view.state.field(field).decorations),
  ],
});

export const mathPreview = [
  mathField,
  EditorView.focusChangeEffect.of((_state, focusing) => setFocus.of(focusing)),
];

/** position comprise dans une formule (sa syntaxe Markdown ne doit pas être décorée) */
export function inMath(state: EditorState, pos: number): boolean {
  const ranges = state.field(mathField, false)?.ranges ?? [];
  return ranges.some((m) => pos >= m.from && pos < m.to);
}
