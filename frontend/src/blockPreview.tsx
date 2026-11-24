import { syntaxTree } from "@codemirror/language";
import { EditorState, Range, StateField } from "@codemirror/state";
import { Decoration, DecorationSet, EditorView, ViewPlugin, ViewUpdate, WidgetType } from "@codemirror/view";
import { createRoot, Root } from "react-dom/client";
import Markdown from "./components/Markdown";
import { mermaidInto } from "./markdown/blocks";
import { frontmatterEnd, setFocus } from "./mathPreview";

/**
 * Blocs de l'aperçu en direct : hors des lignes où se trouve le curseur, les tableaux sont
 * affichés mis en forme et les blocs ```mermaid dessinés, avec le même rendu que le chat
 * (`components/Markdown.tsx`). Un clic dans le bloc place le curseur à l'endroit
 * correspondant de la source (la cellule cliquée pour un tableau), ce qui la réaffiche.
 * Les lignes des blocs de code reçoivent un fond, que l'aperçu soit actif ou non.
 */

type Block = { from: number; to: number; kind: "table" | "mermaid"; source: string };

/** début du contenu de la cellule `col` d'une ligne de tableau (après les `|` non échappés) */
export function cellOffset(line: string, col: number): number {
  let i = 0;
  while (i < line.length && /\s/.test(line[i])) i++;
  if (line[i] === "|") i++;
  for (let c = 0; c < col && i < line.length; i++) {
    if (line[i] === "\\") i++;
    else if (line[i] === "|") c++;
  }
  let j = i;
  while (j < line.length && line[j] === " ") j++;
  // cellule vide : juste après l'espace qui suit le « | »
  if (j >= line.length || line[j] === "|") return Math.min(i + (line[i] === " " ? 1 : 0), line.length);
  return j;
}

const roots = new WeakMap<HTMLElement, Root>();

class BlockWidget extends WidgetType {
  constructor(readonly kind: Block["kind"], readonly source: string) { super(); }
  eq(other: BlockWidget) { return other.kind === this.kind && other.source === this.source; }

  toDOM(view: EditorView) {
    const el = document.createElement("div");
    el.className = `cm-lp-block cm-lp-${this.kind}`;
    if (this.kind === "mermaid") {
      const code = this.source.split("\n").slice(1, -1).join("\n");
      void mermaidInto(el, code);
    } else {
      const root = createRoot(el);
      root.render(<Markdown text={this.source} links={null} onOpen={() => {}} />);
      roots.set(el, root);
    }
    el.addEventListener("mousedown", (e) => {
      const target = e.target as HTMLElement;
      if (target.closest("a, button")) return;
      e.preventDefault();
      const start = view.posAtDOM(el);
      let anchor = start;
      const cell = target.closest<HTMLElement>("[data-row]");
      if (cell) {
        const row = Number(cell.dataset.row);
        const line = view.state.doc.line(view.state.doc.lineAt(start).number + (row === 0 ? 0 : row + 1));
        anchor = line.from + cellOffset(line.text, Number(cell.dataset.col));
      }
      view.dispatch({ selection: { anchor } });
      view.focus();
    });
    return el;
  }

  destroy(dom: HTMLElement) {
    const root = roots.get(dom);
    // démonté hors du cycle de rendu en cours
    if (root) queueMicrotask(() => root.unmount());
  }

  ignoreEvent() { return true; }
}

function findBlocks(state: EditorState): Block[] {
  const skipUntil = frontmatterEnd(state);
  const blocks: Block[] = [];
  // blocs de premier niveau seulement (pas dans une liste ou une citation)
  for (let node = syntaxTree(state).topNode.firstChild; node; node = node.nextSibling) {
    if (node.from < skipUntil) continue;
    let kind: Block["kind"] | null = null;
    if (node.name === "Table") {
      kind = "table";
    } else if (node.name === "FencedCode") {
      const info = node.getChild("CodeInfo");
      const closed = node.getChildren("CodeMark").length >= 2;
      if (closed && info && state.sliceDoc(info.from, info.to).trim().toLowerCase() === "mermaid") kind = "mermaid";
    }
    if (!kind) continue;
    const from = state.doc.lineAt(node.from).from;
    const to = state.doc.lineAt(node.to).to;
    blocks.push({ from, to, kind, source: state.sliceDoc(from, to) });
  }
  return blocks;
}

function decorate(state: EditorState, blocks: Block[], focused: boolean): DecorationSet {
  const decorations: Range<Decoration>[] = [];
  for (const block of blocks) {
    const touched = focused && state.selection.ranges.some((r) => r.from <= block.to && r.to >= block.from);
    if (touched) continue;
    decorations.push(Decoration.replace({ widget: new BlockWidget(block.kind, block.source), block: true })
      .range(block.from, block.to));
  }
  return Decoration.set(decorations, true);
}

type BlockState = { blocks: Block[]; focused: boolean; decorations: DecorationSet };

const blockField = StateField.define<BlockState>({
  create(state) {
    const blocks = findBlocks(state);
    return { blocks, focused: false, decorations: decorate(state, blocks, false) };
  },
  update(value, tr) {
    let { blocks, focused } = value;
    for (const effect of tr.effects) if (effect.is(setFocus)) focused = effect.value;
    if (tr.docChanged || syntaxTree(tr.state) !== syntaxTree(tr.startState)) blocks = findBlocks(tr.state);
    if (blocks === value.blocks && focused === value.focused && !tr.selection) return value;
    return { blocks, focused, decorations: decorate(tr.state, blocks, focused) };
  },
  provide: (field) => EditorView.decorations.from(field, (value) => value.decorations),
});

/** aperçu des tableaux et diagrammes (le focus est suivi par `mathPreview`) */
export const blockPreview = [blockField];

// ---------------------------------------------------------------------------------------------
// fond des blocs de code

const codeLine = Decoration.line({ class: "cm-codeblock" });
const codeFirst = Decoration.line({ class: "cm-codeblock cm-codeblock-first" });
const codeLast = Decoration.line({ class: "cm-codeblock cm-codeblock-last" });

function codeDecorations(view: EditorView): DecorationSet {
  const { state } = view;
  const ranges: Range<Decoration>[] = [];
  let lastLine = 0;
  for (const { from, to } of view.visibleRanges) {
    syntaxTree(state).iterate({
      from, to,
      enter: (node) => {
        if (node.name !== "FencedCode" && node.name !== "CodeBlock") return undefined;
        const first = state.doc.lineAt(node.from).number;
        const last = state.doc.lineAt(node.to).number;
        for (let n = Math.max(first, lastLine + 1); n <= last; n++) {
          const deco = n === first ? codeFirst : n === last ? codeLast : codeLine;
          ranges.push(deco.range(state.doc.line(n).from));
        }
        lastLine = Math.max(lastLine, last);
        return false;
      },
    });
  }
  return Decoration.set(ranges, true);
}

export const codeBlockBackground = ViewPlugin.fromClass(
  class {
    decorations: DecorationSet;
    constructor(view: EditorView) { this.decorations = codeDecorations(view); }
    update(update: ViewUpdate) {
      if (update.docChanged || update.viewportChanged || syntaxTree(update.state) !== syntaxTree(update.startState)) {
        this.decorations = codeDecorations(update.view);
      }
    }
  },
  { decorations: (v) => v.decorations },
);
