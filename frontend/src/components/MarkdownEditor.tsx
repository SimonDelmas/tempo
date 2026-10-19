import { MutableRefObject, useEffect, useRef } from "react";
import { EditorView, keymap, Decoration, MatchDecorator, ViewPlugin, ViewUpdate,
  DecorationSet, drawSelection, highlightActiveLine, placeholder } from "@codemirror/view";
import { EditorState } from "@codemirror/state";
import { defaultKeymap, history, historyKeymap, indentWithTab } from "@codemirror/commands";
import { markdown } from "@codemirror/lang-markdown";
import { HighlightStyle, syntaxHighlighting } from "@codemirror/language";
import { autocompletion, Completion, CompletionContext, CompletionResult,
  completionKeymap } from "@codemirror/autocomplete";
import { tags } from "@lezer/highlight";
import { NoteSummary, today } from "../api";

type Props = {
  initial: string;
  files: NoteSummary[];
  onChange: (text: string) => void;
  onOpenLink: (name: string) => void;
  onCreateTask: (title: string) => Promise<string>;
  /** vue CodeMirror, pour les actions qui agissent sur le texte (IA) */
  viewRef?: MutableRefObject<EditorView | null>;
};

const markdownStyle = HighlightStyle.define([
  { tag: tags.heading1, fontSize: "1.55em", fontWeight: "700" },
  { tag: tags.heading2, fontSize: "1.3em", fontWeight: "700" },
  { tag: tags.heading3, fontSize: "1.12em", fontWeight: "700" },
  { tag: tags.heading, fontWeight: "700" },
  { tag: tags.strong, fontWeight: "700" },
  { tag: tags.emphasis, fontStyle: "italic" },
  { tag: tags.strikethrough, textDecoration: "line-through" },
  { tag: tags.link, color: "var(--accent)" },
  { tag: tags.url, color: "var(--muted)" },
  { tag: tags.monospace, fontFamily: "var(--mono)", color: "var(--code)" },
  { tag: tags.quote, color: "var(--muted)", fontStyle: "italic" },
  { tag: tags.processingInstruction, color: "var(--muted)" },
  { tag: tags.meta, color: "var(--muted)" },
]);

const editorTheme = EditorView.theme({
  ".cm-scroller": { fontFamily: "var(--sans)", lineHeight: "1.65" },
});

const wikiLinkMatcher = new MatchDecorator({
  regexp: /\[\[([^\]\n]+)\]\]/g,
  decoration: (match) =>
    Decoration.mark({
      class: "cm-wikilink",
      attributes: { "data-link": match[1].split("|")[0].split("#")[0].trim(), title: "Ctrl+clic pour ouvrir" },
    }),
});

const wikiLinks = ViewPlugin.fromClass(
  class {
    decorations: DecorationSet;
    constructor(view: EditorView) {
      this.decorations = wikiLinkMatcher.createDeco(view);
    }
    update(update: ViewUpdate) {
      this.decorations = wikiLinkMatcher.updateDeco(update, this.decorations);
    }
  },
  { decorations: (v) => v.decorations },
);

// le frontmatter YAML est affiché en petit et en gris (sinon interprété comme un titre)
function frontmatterDeco(view: EditorView): DecorationSet {
  const doc = view.state.doc;
  if (doc.lines < 2 || doc.line(1).text.trim() !== "---") return Decoration.none;
  const line = Decoration.line({ class: "cm-frontmatter" });
  const ranges = [line.range(doc.line(1).from)];
  for (let n = 2; n <= doc.lines; n++) {
    ranges.push(line.range(doc.line(n).from));
    if (doc.line(n).text.trim() === "---") return Decoration.set(ranges);
  }
  return Decoration.none;
}

const frontmatter = ViewPlugin.fromClass(
  class {
    decorations: DecorationSet;
    constructor(view: EditorView) {
      this.decorations = frontmatterDeco(view);
    }
    update(update: ViewUpdate) {
      if (update.docChanged) this.decorations = frontmatterDeco(update.view);
    }
  },
  { decorations: (v) => v.decorations },
);

export default function MarkdownEditor({ initial, files, onChange, onOpenLink, onCreateTask, viewRef }: Props) {
  const host = useRef<HTMLDivElement>(null);
  const callbacks = useRef({ files, onChange, onOpenLink, onCreateTask });
  callbacks.current = { files, onChange, onOpenLink, onCreateTask };

  useEffect(() => {
    const linkCompletion = (ctx: CompletionContext): CompletionResult | null => {
      const match = ctx.matchBefore(/\[\[[^\]\n]*/);
      if (!match) return null;
      const query = match.text.slice(2).toLowerCase();
      const options: Completion[] = callbacks.current.files
        .filter((f) => f.type !== "modele" && f.type !== "inbox")
        .filter((f) => !query || `${f.alias} ${f.stem} ${f.title}`.toLowerCase().includes(query))
        .slice(0, 50)
        .map((f) => {
          const name = f.alias || f.stem;
          return { label: name, detail: f.title !== name ? f.title : "", type: f.type,
                   apply: name + "]]" };
        });
      return { from: match.from + 2, options, filter: false };
    };

    const slashCompletion = (ctx: CompletionContext): CompletionResult | null => {
      const match = ctx.matchBefore(/(?:^|\s)\/[\wà-ü]*$/);
      if (!match) return null;
      const from = match.text.startsWith("/") ? match.from : match.from + 1;
      const options: Completion[] = [
        {
          label: "/tache", detail: "crée une tâche et insère son lien",
          apply: (view, _c, f, t) => {
            const title = window.prompt("Titre de la tâche");
            if (!title) return;
            callbacks.current.onCreateTask(title).then((id) => {
              view.dispatch({ changes: { from: f, to: t, insert: `[[${id}]]` } });
            });
          },
        },
        { label: "/date", detail: "date du jour", apply: today() },
        { label: "/action", detail: "case à cocher", apply: "- [ ] " },
        {
          label: "/reunion", detail: "sections de compte-rendu",
          apply: "## Participants\n\n## Points abordés\n\n## Décisions\n\n## Actions\n",
        },
      ];
      return { from, options };
    };

    const view = new EditorView({
      parent: host.current!,
      state: EditorState.create({
        doc: initial,
        extensions: [
          history(),
          drawSelection(),
          highlightActiveLine(),
          EditorView.lineWrapping,
          editorTheme,
          markdown(),
          syntaxHighlighting(markdownStyle),
          wikiLinks,
          frontmatter,
          placeholder("Écrivez… ([[ pour lier une note, / pour les commandes)"),
          autocompletion({ override: [linkCompletion, slashCompletion] }),
          keymap.of([...completionKeymap, ...defaultKeymap, ...historyKeymap, indentWithTab]),
          EditorView.updateListener.of((u) => {
            if (u.docChanged) callbacks.current.onChange(u.state.doc.toString());
          }),
          EditorView.domEventHandlers({
            mousedown: (event) => {
              const target = (event.target as HTMLElement).closest(".cm-wikilink") as HTMLElement | null;
              if (target && (event.ctrlKey || event.metaKey)) {
                event.preventDefault();
                callbacks.current.onOpenLink(target.dataset.link!);
                return true;
              }
              return false;
            },
          }),
        ],
      }),
    });
    view.focus();
    if (viewRef) viewRef.current = view;
    return () => {
      if (viewRef) viewRef.current = null;
      view.destroy();
    };
    // l'éditeur est recréé via la prop `key` quand la note change
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  return <div className="cm-host" ref={host} />;
}
