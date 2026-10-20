import { useEffect, useState } from "react";

/** Espaces de l'application, chacun avec sa barre latérale et son adresse (#/taches/…). */
export type Space = "notes" | "taches" | "rapports" | "imputations" | "inbox" | "brouillons";

export const SPACES: { id: Space; label: string }[] = [
  { id: "notes", label: "Notes" },
  { id: "taches", label: "Tâches" },
  { id: "rapports", label: "Rapports" },
  { id: "imputations", label: "Imputations" },
  { id: "inbox", label: "Inbox" },
  { id: "brouillons", label: "Draft" },
];

export const DEFAULT_SPACE: Space = "taches";

/** #/<espace>/<chemin de note>?<paramètres> ; sans chemin : page d'accueil de l'espace */
export type Route = { space: Space; path: string | null; params: Record<string, string> };

/** dossiers du vault rattachés à un espace autre que Notes */
const FOLDER_SPACES: [string, Space][] = [
  ["taches/", "taches"],
  ["rapports/", "rapports"],
  ["comptes/", "imputations"],
  ["imputations/", "imputations"],
];

export const spaceOf = (path: string): Space =>
  FOLDER_SPACES.find(([folder]) => path.startsWith(folder))?.[1] ?? "notes";

export const isSpaceFolder = (folder: string) => FOLDER_SPACES.some(([f]) => f === `${folder}/`);

export function parseHash(hash: string): Route {
  const [rawPath, query = ""] = hash.replace(/^#\/?/, "").split("?");
  const [head, ...rest] = rawPath.split("/");
  const space = SPACES.some((s) => s.id === head) ? (head as Space) : DEFAULT_SPACE;
  const path = rest.length ? rest.map(decodeURIComponent).join("/") : null;
  return { space, path: path || null, params: Object.fromEntries(new URLSearchParams(query)) };
}

export function href(space: Space, path?: string | null, params?: Record<string, string | null | undefined>): string {
  const segments = path ? "/" + path.split("/").map(encodeURIComponent).join("/") : "";
  const clean = Object.entries(params ?? {}).filter(([, v]) => v) as [string, string][];
  const query = clean.length ? "?" + new URLSearchParams(clean).toString() : "";
  return `#/${space}${segments}${query}`;
}

/** adresse d'une note : elle s'ouvre dans l'espace auquel appartient son dossier */
export const noteHref = (path: string) => href(spaceOf(path), path);

// Une vue avec une saisie non enregistrée peut bloquer la navigation interne.
let leaveGuard: (() => boolean) | null = null;
export const setLeaveGuard = (guard: (() => boolean) | null) => { leaveGuard = guard; };

export function navigate(to: string) {
  if (to === window.location.hash) return;
  if (leaveGuard && !leaveGuard()) return;
  leaveGuard = null;
  window.location.hash = to;
}

// Historique des vues propre à l'application : les boutons précédent / suivant y circulent
// en posant l'adresse cible, sans dépendre de l'historique du navigateur (indisponible pour
// savoir si l'on peut reculer ou avancer).
const MAX_HISTORY = 100;
let views: string[] = [window.location.hash];
let position = 0;
let moving: number | null = null;
const historyListeners = new Set<() => void>();

window.addEventListener("hashchange", () => {
  const hash = window.location.hash;
  if (moving !== null && views[moving] === hash) {
    position = moving;
  } else if (hash !== views[position]) {
    views = [...views.slice(0, position + 1), hash].slice(-MAX_HISTORY);
    position = views.length - 1;
  }
  moving = null;
  historyListeners.forEach((listener) => listener());
});

function move(offset: number) {
  const target = position + offset;
  if (target < 0 || target >= views.length) return;
  if (leaveGuard && !leaveGuard()) return;
  leaveGuard = null;
  moving = target;
  window.location.hash = views[target];
}

export const goBack = () => move(-1);
export const goForward = () => move(1);

export function useNavHistory() {
  const [, setTick] = useState(0);
  useEffect(() => {
    const listener = () => setTick((t) => t + 1);
    historyListeners.add(listener);
    return () => { historyListeners.delete(listener); };
  }, []);
  return { canBack: position > 0, canForward: position < views.length - 1 };
}

/** Le raccourci (« Alt+ArrowLeft », « Ctrl+Shift+[ »…) correspond-il à l'événement ? */
export function matchShortcut(e: KeyboardEvent, shortcut: string): boolean {
  const parts = shortcut.split("+").map((p) => p.trim()).filter(Boolean);
  // « Ctrl++ » : la touche est elle-même « + »
  if (shortcut.endsWith("++")) parts.push("+");
  const key = parts.pop();
  if (!key) return false;
  const mods = new Set(parts.map((p) => p.toLowerCase()));
  return e.key.toLowerCase() === key.toLowerCase()
    && e.ctrlKey === mods.has("ctrl")
    && e.altKey === mods.has("alt")
    && e.shiftKey === mods.has("shift")
    && e.metaKey === (mods.has("meta") || mods.has("cmd"));
}

/** libellé lisible d'un raccourci pour les infobulles (« Alt+← ») */
export const shortcutLabel = (shortcut: string) =>
  shortcut.replace(/ArrowLeft/i, "←").replace(/ArrowRight/i, "→").replace(/ArrowUp/i, "↑").replace(/ArrowDown/i, "↓");

export function useRoute(): Route {
  const [route, setRoute] = useState(() => parseHash(window.location.hash));
  useEffect(() => {
    const onChange = () => setRoute(parseHash(window.location.hash));
    window.addEventListener("hashchange", onChange);
    return () => window.removeEventListener("hashchange", onChange);
  }, []);
  return route;
}
