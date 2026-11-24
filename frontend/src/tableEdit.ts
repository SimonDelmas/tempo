import { completionStatus } from "@codemirror/autocomplete";
import { syntaxTree } from "@codemirror/language";
import { EditorState, Prec } from "@codemirror/state";
import { EditorView, KeyBinding, keymap } from "@codemirror/view";
import { cellOffset } from "./blockPreview";

/**
 * Édition des tableaux Markdown (GFM) depuis la source : le curseur dans un tableau,
 * Tab / Maj+Tab passent à la cellule suivante / précédente (une ligne est ajoutée après la
 * dernière cellule), Entrée ajoute une ligne (sur une dernière ligne vide, sort du tableau),
 * Alt+Maj+flèches insèrent une ligne ou une colonne, Alt+Maj+Retour arrière supprime la ligne
 * et Alt+Maj+Suppr la colonne. Chaque commande réaligne les colonnes du tableau.
 */

type Align = "left" | "center" | "right" | "";

/** tableau : lignes de cellules (la ligne 1, séparateur, est vide) et alignements */
type Model = { rows: string[][]; aligns: Align[] };

type Located = { from: number; to: number; model: Model; row: number; col: number };

/** cellules d'une ligne : `|` non échappés, bords retirés, contenu rogné */
export function splitRow(line: string): string[] {
  let text = line.trim();
  if (text.startsWith("|")) text = text.slice(1);
  if (text.endsWith("|") && !text.endsWith("\\|")) text = text.slice(0, -1);
  const cells: string[] = [];
  let current = "";
  for (let i = 0; i < text.length; i++) {
    if (text[i] === "\\" && i + 1 < text.length) { current += text[i] + text[i + 1]; i++; continue; }
    if (text[i] === "|") { cells.push(current.trim()); current = ""; continue; }
    current += text[i];
  }
  cells.push(current.trim());
  return cells;
}

function parseAlign(cell: string): Align {
  const left = cell.startsWith(":");
  const right = cell.endsWith(":");
  return left && right ? "center" : right ? "right" : left ? "left" : "";
}

/** texte du tableau, colonnes alignées */
export function formatTable({ rows, aligns }: Model): string {
  const cols = Math.max(aligns.length, ...rows.map((r) => r.length));
  const widths = Array.from({ length: cols }, (_, c) =>
    Math.max(3, ...rows.filter((_, i) => i !== 1).map((r) => (r[c] ?? "").length)));
  const pad = (text: string, c: number) => {
    const space = widths[c] - text.length;
    if (aligns[c] === "right") return " ".repeat(space) + text;
    if (aligns[c] === "center") return " ".repeat(Math.floor(space / 2)) + text + " ".repeat(Math.ceil(space / 2));
    return text + " ".repeat(space);
  };
  const delimiter = (c: number) => {
    const w = widths[c];
    switch (aligns[c]) {
      case "left": return ":" + "-".repeat(w - 1);
      case "right": return "-".repeat(w - 1) + ":";
      case "center": return ":" + "-".repeat(w - 2) + ":";
      default: return "-".repeat(w);
    }
  };
  return rows.map((row, i) => {
    const cells = Array.from({ length: cols }, (_, c) => (i === 1 ? delimiter(c) : pad(row[c] ?? "", c)));
    return `| ${cells.join(" | ")} |`;
  }).join("\n");
}

/** tableau sous le curseur, et la cellule où il se trouve */
function locate(state: EditorState): Located | null {
  const pos = state.selection.main.head;
  let table = null;
  for (const side of [-1, 1] as const) {
    for (let node: ReturnType<typeof syntaxTree>["topNode"] | null = syntaxTree(state).resolveInner(pos, side);
      node && !table; node = node.parent) {
      if (node.name === "Table") table = node;
    }
  }
  if (!table) return null;
  const first = state.doc.lineAt(table.from);
  const last = state.doc.lineAt(table.to);
  const lines: string[] = [];
  for (let n = first.number; n <= last.number; n++) lines.push(state.doc.line(n).text);
  if (lines.length < 2) return null;
  const rows = lines.map((l, i) => (i === 1 ? [] : splitRow(l)));
  const aligns = splitRow(lines[1]).map(parseAlign);
  const cols = Math.max(aligns.length, ...rows.map((r) => r.length));
  while (aligns.length < cols) aligns.push("");

  const line = state.doc.lineAt(pos);
  const row = line.number - first.number;
  const before = line.text.slice(0, pos - line.from);
  let pipes = 0;
  for (let i = 0; i < before.length; i++) {
    if (before[i] === "\\") i++;
    else if (before[i] === "|") pipes++;
  }
  const leading = line.text.trimStart().startsWith("|") ? 1 : 0;
  const col = Math.min(Math.max(pipes - leading, 0), cols - 1);
  return { from: first.from, to: last.to, model: { rows, aligns }, row, col };
}

type Edit = { model: Model; row: number; col: number } | null;

/** applique une transformation du tableau, puis place le curseur à la fin de la cellule visée */
function tableCommand(change: (t: Located) => Edit) {
  return (view: EditorView): boolean => {
    if (completionStatus(view.state) === "active") return false;
    const located = locate(view.state);
    if (!located) return false;
    const result = change(located);
    if (!result) return false;
    const text = formatTable(result.model);
    const lines = text.split("\n");
    const row = Math.min(result.row, lines.length - 1);
    let offset = 0;
    for (let i = 0; i < row; i++) offset += lines[i].length + 1;
    const cell = result.model.rows[row]?.[result.col] ?? "";
    const anchor = located.from + offset + cellOffset(lines[row], result.col) + cell.length;
    view.dispatch({
      changes: { from: located.from, to: located.to, insert: text },
      selection: { anchor },
      scrollIntoView: true,
      userEvent: "input.table",
    });
    return true;
  };
}

const emptyRow = (m: Model) => m.aligns.map(() => "");
const lastRow = (m: Model) => m.rows.length - 1;
/** rang d'insertion d'une ligne de données (jamais avant le séparateur) */
const dataRow = (row: number) => Math.max(row, 2);

export const nextCell = tableCommand(({ model, row, col }) => {
  const cols = model.aligns.length;
  if (row === 1) return { model, row: 2 <= lastRow(model) ? 2 : 0, col: 0 };
  if (col + 1 < cols) return { model, row, col: col + 1 };
  const next = row === 0 ? 2 : row + 1;
  if (next > lastRow(model)) model.rows.push(emptyRow(model));
  return { model, row: next, col: 0 };
});

export const previousCell = tableCommand(({ model, row, col }) => {
  if (col > 0) return { model, row, col: col - 1 };
  if (row <= 1) return { model, row: 0, col: 0 };
  return { model, row: row === 2 ? 0 : row - 1, col: model.aligns.length - 1 };
});

/** Entrée : nouvelle ligne sous la ligne courante ; sur une dernière ligne vide, sort du tableau */
function enter(view: EditorView): boolean {
  if (completionStatus(view.state) === "active") return false;
  const located = locate(view.state);
  if (!located) return false;
  const { model, row } = located;
  if (row >= 2 && row === lastRow(model) && model.rows[row].every((c) => !c)) {
    const line = view.state.doc.lineAt(view.state.selection.main.head);
    // une ligne vide termine le tableau (sinon le texte tapé dessous en deviendrait une ligne)
    view.dispatch({ changes: { from: line.from, to: line.to, insert: "\n" }, selection: { anchor: line.from + 1 } });
    return true;
  }
  return tableCommand(() => {
    const at = dataRow(row + 1);
    model.rows.splice(at, 0, emptyRow(model));
    return { model, row: at, col: 0 };
  })(view);
}

export const insertRowBelow = tableCommand(({ model, row, col }) => {
  const at = dataRow(row + 1);
  model.rows.splice(at, 0, emptyRow(model));
  return { model, row: at, col };
});

export const insertRowAbove = tableCommand(({ model, row, col }) => {
  if (row <= 1) return null; // pas de ligne au-dessus de l'en-tête
  model.rows.splice(row, 0, emptyRow(model));
  return { model, row, col };
});

function insertColumn(offset: 0 | 1) {
  return tableCommand(({ model, row, col }) => {
    const at = col + offset;
    model.rows.forEach((r, i) => { if (i !== 1) r.splice(at, 0, ""); });
    model.aligns.splice(at, 0, "");
    return { model, row, col: at };
  });
}

export const insertColumnRight = insertColumn(1);
export const insertColumnLeft = insertColumn(0);

export const deleteRow = tableCommand(({ model, row, col }) => {
  if (row <= 1) return null;
  model.rows.splice(row, 1);
  return { model, row: Math.min(row, lastRow(model)) === 1 ? 0 : Math.min(row, lastRow(model)), col };
});

export const deleteColumn = tableCommand(({ model, row, col }) => {
  if (model.aligns.length <= 1) return null;
  model.rows.forEach((r, i) => { if (i !== 1) r.splice(col, 1); });
  model.aligns.splice(col, 1);
  return { model, row, col: Math.min(col, model.aligns.length - 1) };
});

/** squelette inséré par la commande « /tableau » */
export const TABLE_TEMPLATE = "| Colonne 1 | Colonne 2 | Colonne 3 |\n| --------- | --------- | --------- |\n|           |           |           |\n";

const bindings: KeyBinding[] = [
  { key: "Tab", run: nextCell, shift: previousCell },
  { key: "Enter", run: enter },
  { key: "Alt-Shift-ArrowDown", run: insertRowBelow },
  { key: "Alt-Shift-ArrowUp", run: insertRowAbove },
  { key: "Alt-Shift-ArrowRight", run: insertColumnRight },
  { key: "Alt-Shift-ArrowLeft", run: insertColumnLeft },
  { key: "Alt-Shift-Backspace", run: deleteRow },
  { key: "Alt-Shift-Delete", run: deleteColumn },
];

/** raccourcis actifs seulement quand le curseur est dans un tableau */
export const tableKeymap = Prec.highest(keymap.of(bindings));
