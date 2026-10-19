import { useEffect, useState } from "react";

/** Espaces de l'application, chacun avec sa barre latérale et son adresse (#/taches/…). */
export type Space = "notes" | "taches" | "rapports" | "imputations" | "inbox" | "chat";

export const SPACES: { id: Space; label: string }[] = [
  { id: "notes", label: "Notes" },
  { id: "taches", label: "Tâches" },
  { id: "rapports", label: "Rapports" },
  { id: "imputations", label: "Imputations" },
  { id: "inbox", label: "Inbox" },
  { id: "chat", label: "Chat" },
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

export function useRoute(): Route {
  const [route, setRoute] = useState(() => parseHash(window.location.hash));
  useEffect(() => {
    const onChange = () => setRoute(parseHash(window.location.hash));
    window.addEventListener("hashchange", onChange);
    return () => window.removeEventListener("hashchange", onChange);
  }, []);
  return route;
}
