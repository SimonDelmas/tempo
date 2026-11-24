import { LanguageDescription } from "@codemirror/language";
import { languages } from "@codemirror/language-data";
import { classHighlighter, highlightCode } from "@lezer/highlight";

/**
 * Rendu des blocs riches, commun au chat et à l'aperçu en direct de l'éditeur : coloration
 * syntaxique des blocs de code (langages de `@codemirror/language-data`, chargés à la demande,
 * classes `tok-*` partagées avec l'éditeur) et diagrammes Mermaid (bibliothèque chargée au
 * premier diagramme).
 */

/** colore `code` dans `el` (le texte brut s'affiche aussitôt, les couleurs après chargement) */
export async function highlightInto(el: HTMLElement, code: string, lang: string): Promise<void> {
  el.textContent = code;
  const desc = lang ? LanguageDescription.matchLanguageName(languages, lang, true) : null;
  if (!desc) return;
  let support;
  try {
    support = desc.support ?? await desc.load();
  } catch {
    return;
  }
  // le bloc a pu changer pendant le chargement
  if (el.textContent !== code) return;
  const fragment = document.createDocumentFragment();
  highlightCode(code, support.language.parser.parse(code), classHighlighter,
    (text, classes) => {
      if (!classes) { fragment.append(text); return; }
      const span = document.createElement("span");
      span.className = classes;
      span.textContent = text;
      fragment.append(span);
    },
    () => fragment.append("\n"));
  el.replaceChildren(fragment);
}

type Mermaid = typeof import("mermaid").default;

let mermaidLoad: Promise<Mermaid> | null = null;
let mermaidTheme = "";
let counter = 0;
const svgCache = new Map<string, string>();

const darkMode = () => window.matchMedia?.("(prefers-color-scheme: dark)").matches ?? false;

async function loadMermaid(): Promise<Mermaid> {
  mermaidLoad ??= import("mermaid").then((m) => m.default);
  const mermaid = await mermaidLoad;
  const theme = darkMode() ? "dark" : "default";
  if (theme !== mermaidTheme) {
    // `strict` : pas de HTML ni de script dans les libellés (diagrammes écrits par le LLM)
    mermaid.initialize({ startOnLoad: false, securityLevel: "strict", theme, fontFamily: "inherit" });
    mermaidTheme = theme;
  }
  return mermaid;
}

/** SVG du diagramme, ou une erreur lisible si sa syntaxe est invalide */
export async function renderMermaid(code: string): Promise<{ svg: string } | { error: string }> {
  const key = `${darkMode() ? "d" : "l"}:${code}`;
  const cached = svgCache.get(key);
  if (cached) return { svg: cached };
  try {
    const mermaid = await loadMermaid();
    await mermaid.parse(code);
    const { svg } = await mermaid.render(`tempo-mermaid-${++counter}`, code);
    if (svgCache.size > 100) svgCache.clear();
    svgCache.set(key, svg);
    return { svg };
  } catch (e) {
    return { error: e instanceof Error ? e.message : String(e) };
  }
}

/** remplit `el` avec le diagramme (ou l'erreur et la source) */
export async function mermaidInto(el: HTMLElement, code: string): Promise<void> {
  el.classList.add("md-mermaid");
  el.dataset.code = code;
  if (!el.firstChild) el.textContent = "Diagramme…";
  const result = await renderMermaid(code);
  // un rendu plus récent a été demandé entre-temps
  if (el.dataset.code !== code) return;
  if ("svg" in result) {
    el.innerHTML = result.svg;
    el.classList.remove("md-mermaid-error");
  } else {
    const message = document.createElement("div");
    message.className = "md-mermaid-message";
    message.textContent = `Diagramme Mermaid invalide : ${result.error.split("\n")[0]}`;
    const source = document.createElement("pre");
    source.textContent = code;
    el.replaceChildren(message, source);
    el.classList.add("md-mermaid-error");
  }
}
