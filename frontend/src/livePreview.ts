import { syntaxTree } from "@codemirror/language";
import { EditorState, Range } from "@codemirror/state";
import { Decoration, DecorationSet, EditorView, ViewPlugin, ViewUpdate, WidgetType } from "@codemirror/view";
import { frontmatterEnd, inMath, mathPreview } from "./mathPreview";

/**
 * Aperçu en direct façon Obsidian : hors des lignes où se trouve le curseur, la syntaxe
 * Markdown est masquée (#, **, `, [[ ]], [texte](url)), les puces deviennent des points et
 * les cases « - [ ] » des cases à cocher cliquables et les formules `$…$` / `$$…$$` sont rendues
 * par KaTeX (`mathPreview.ts`). Le texte du fichier n'est jamais modifié,
 * sauf en cochant une case.
 */

class BulletWidget extends WidgetType {
  eq() { return true; }
  toDOM() {
    const span = document.createElement("span");
    span.className = "cm-lp-bullet";
    span.textContent = "•";
    return span;
  }
}

class CheckboxWidget extends WidgetType {
  constructor(readonly checked: boolean, readonly pos: number) { super(); }
  eq(other: CheckboxWidget) { return other.checked === this.checked && other.pos === this.pos; }
  toDOM(view: EditorView) {
    const box = document.createElement("input");
    box.type = "checkbox";
    box.className = "cm-lp-checkbox";
    box.checked = this.checked;
    box.addEventListener("mousedown", (e) => {
      e.preventDefault();
      // « [ ] » ↔ « [x] » : le caractère central du marqueur
      view.dispatch({ changes: { from: this.pos + 1, to: this.pos + 2, insert: this.checked ? " " : "x" } });
    });
    return box;
  }
  ignoreEvent() { return false; }
}

const hidden = Decoration.replace({});
const bullet = Decoration.replace({ widget: new BulletWidget() });
const linkText = Decoration.mark({ class: "cm-lp-link" });
const done = Decoration.mark({ class: "cm-lp-done" });

/** lignes touchées par la sélection : leur syntaxe reste visible pour l'édition */
function activeLines(state: EditorState): Set<number> {
  const lines = new Set<number>();
  for (const range of state.selection.ranges) {
    const first = state.doc.lineAt(range.from).number;
    const last = state.doc.lineAt(range.to).number;
    for (let n = first; n <= last; n++) lines.add(n);
  }
  return lines;
}

const WIKILINK_RE = /\[\[([^\]|\n]+\|)?[^\]\n]+\]\]/g;

function buildDecorations(view: EditorView, hasFocus: boolean): DecorationSet {
  const { state } = view;
  const active = hasFocus ? activeLines(state) : new Set<number>();
  const skipUntil = frontmatterEnd(state);
  const ranges: Range<Decoration>[] = [];
  const isActive = (pos: number) => active.has(state.doc.lineAt(pos).number);

  for (const { from, to } of view.visibleRanges) {
    syntaxTree(state).iterate({
      from, to,
      enter: (node) => {
        if (node.to <= skipUntil) return node.name === "Document" ? undefined : false;
        const name = node.name;
        if (isActive(node.from)) return;
        if (inMath(state, node.from)) return false;

        if (name === "TaskMarker") {
          const checked = state.sliceDoc(node.from, node.to).toLowerCase() === "[x]";
          ranges.push(Decoration.replace({ widget: new CheckboxWidget(checked, node.from) })
            .range(node.from, node.to));
          if (checked) {
            const line = state.doc.lineAt(node.from);
            if (node.to + 1 < line.to) ranges.push(done.range(node.to + 1, line.to));
          }
          return;
        }
        if (name === "HeaderMark") {
          // « ## » et l'espace qui suit (pas le soulignement des titres « setext »)
          const next = state.sliceDoc(node.to, node.to + 1);
          if (state.sliceDoc(node.from, node.to).startsWith("#")) {
            ranges.push(hidden.range(node.from, next === " " ? node.to + 1 : node.to));
          }
        } else if (name === "EmphasisMark" || name === "StrikethroughMark") {
          ranges.push(hidden.range(node.from, node.to));
        } else if (name === "CodeMark" && node.node.parent?.name === "InlineCode") {
          ranges.push(hidden.range(node.from, node.to));
        } else if (name === "ListMark") {
          const parent = node.node.parent;
          const isTask = parent?.getChild("Task") !== null;
          const mark = state.sliceDoc(node.from, node.to);
          if (isTask) ranges.push(hidden.range(node.from, Math.min(node.to + 1, state.doc.lineAt(node.from).to)));
          else if (mark === "-" || mark === "*" || mark === "+") ranges.push(bullet.range(node.from, node.to));
        } else if (name === "Link") {
          const marks = node.node.getChildren("LinkMark");
          const url = node.node.getChild("URL");
          if (marks.length >= 2) {
            ranges.push(hidden.range(marks[0].from, marks[0].to));
            ranges.push(linkText.range(marks[0].to, marks[1].from));
            ranges.push(hidden.range(marks[1].from, node.to));
          } else if (url) {
            ranges.push(linkText.range(url.from, url.to));
          }
          return false;
        }
        return undefined;
      },
    });

    // liens [[cible|alias]] : seul le texte affiché reste visible
    const text = state.sliceDoc(from, to);
    for (const match of text.matchAll(WIKILINK_RE)) {
      const start = from + match.index;
      const end = start + match[0].length;
      if (end <= skipUntil || isActive(start) || inMath(state, start)) continue;
      ranges.push(hidden.range(start, start + 2 + (match[1]?.length ?? 0)));
      ranges.push(hidden.range(end - 2, end));
    }
  }
  return Decoration.set(ranges, true);
}

const syntaxPreview = ViewPlugin.fromClass(
  class {
    decorations: DecorationSet;
    /** éléments remplacés uniquement : le curseur les saute d'un bloc */
    atomic: DecorationSet;
    constructor(view: EditorView) {
      this.decorations = buildDecorations(view, view.hasFocus);
      this.atomic = this.replaced();
    }
    update(update: ViewUpdate) {
      if (update.docChanged || update.viewportChanged || update.selectionSet || update.focusChanged
          || syntaxTree(update.state) !== syntaxTree(update.startState)) {
        this.decorations = buildDecorations(update.view, update.view.hasFocus);
        this.atomic = this.replaced();
      }
    }
    replaced(): DecorationSet {
      const ranges: Range<Decoration>[] = [];
      this.decorations.between(0, Number.MAX_SAFE_INTEGER, (from, to, deco) => {
        if (from < to && deco.spec.class === undefined) ranges.push(deco.range(from, to));
      });
      return Decoration.set(ranges, true);
    }
  },
  {
    decorations: (v) => v.decorations,
    provide: (plugin) => EditorView.atomicRanges.of((view) => view.plugin(plugin)?.atomic ?? Decoration.none),
  },
);

export const livePreview = [mathPreview, syntaxPreview];
