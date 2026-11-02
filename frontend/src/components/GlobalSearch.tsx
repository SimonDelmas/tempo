import { useEffect, useMemo, useRef, useState } from "react";
import { api, GlobalHit, NoteSummary, TYPE_LABELS } from "../api";
import { documentHref, href, navigate } from "../router";
import { Snippet } from "./LibraryView";

type Props = {
  files: NoteSummary[];
  /** recherche par le sens disponible (modèle d'embeddings configuré) */
  semantic: boolean;
  onOpen: (path: string) => void;
  onClose: () => void;
  notify: (message: string, error?: boolean) => void;
};

type Section = { title: string; hits: GlobalHit[]; byTitle?: boolean };

const GROUPS: { id: GlobalHit["group"]; title: string }[] = [
  { id: "tache", title: "Tâches" },
  { id: "note", title: "Notes" },
  { id: "document", title: "Documents" },
  { id: "brouillon", title: "Brouillons" },
];

const RECENT_COUNT = 10;
const TITLE_LIMIT = 8;
const MEANING_MIN_CHARS = 3;

/** texte comparable : minuscules, sans accents */
const fold = (text: string) => text.normalize("NFD").replace(/[̀-ͯ]/g, "").toLowerCase();

const byDate = (a: NoteSummary, b: NoteSummary) => (b.date || "").localeCompare(a.date || "");

const noteHit = (f: NoteSummary): GlobalHit => ({
  group: f.type === "tache" || f.path.startsWith("taches/") ? "tache" : "note",
  target: f.path, title: f.alias ? `${f.alias} · ${f.title}` : f.title, type: f.type, date: f.date ?? "", snippet: "", section: "", page: null,
});

const hitLabel = (hit: GlobalHit) => {
  if (hit.group === "document") return `Document · ${hit.type}`;
  if (hit.group === "brouillon") return "Brouillon";
  return TYPE_LABELS[hit.type] ?? hit.type;
};

export default function GlobalSearch({ files, semantic, onOpen, onClose, notify }: Props) {
  const [query, setQuery] = useState("");
  const [selected, setSelected] = useState(0);
  const [wordHits, setWordHits] = useState<GlobalHit[]>([]);
  const [meaningHits, setMeaningHits] = useState<GlobalHit[]>([]);
  const [searching, setSearching] = useState(false);
  const selectedRow = useRef<HTMLLIElement>(null);
  const trimmed = query.trim();

  const notes = useMemo(() => files.filter((f) =>
    !["modele", "inbox"].includes(f.type) && !f.path.startsWith("_templates/") && !f.path.startsWith("00-inbox/")), [files]);

  // les titres s'affichent aussitôt, le serveur complète ensuite
  useEffect(() => {
    setMeaningHits([]);
    if (!trimmed) {
      setWordHits([]);
      setSearching(false);
      return;
    }
    setSearching(true);
    let cancelled = false;
    const words = window.setTimeout(() => {
      api.globalSearch(trimmed)
        .then((hits) => { if (!cancelled) setWordHits(hits); })
        .catch((e) => { if (!cancelled) notify(e.message, true); })
        .finally(() => { if (!cancelled) setSearching(false); });
    }, 200);
    // le sens est plus lent (modèle à charger) : silencieux en cas d'échec, les mots restent
    const meaning = semantic && trimmed.length >= MEANING_MIN_CHARS ? window.setTimeout(() => {
      api.globalSearchMeaning(trimmed)
        .then((hits) => { if (!cancelled) setMeaningHits(hits); })
        .catch(() => {});
    }, 450) : undefined;
    return () => {
      cancelled = true;
      window.clearTimeout(words);
      window.clearTimeout(meaning);
    };
  }, [trimmed, semantic, notify]);

  const sections = useMemo<Section[]>(() => {
    if (!trimmed) {
      return [{ title: "Récents", hits: [...notes].sort(byDate).slice(0, RECENT_COUNT).map(noteHit) }];
    }
    const words = fold(trimmed).split(/\s+/).filter(Boolean);
    const byTitle = notes
      .filter((f) => words.every((w) => fold(`${f.alias} ${f.title} ${f.path}`).includes(w)))
      .sort(byDate)
      .map(noteHit);
    const snippets = new Map(wordHits.map((h) => [`${h.group}:${h.target}`, h.snippet]));
    const seen = new Set<string>();
    const unseen = (h: GlobalHit) => {
      const key = `${h.group}:${h.target}`;
      if (seen.has(key)) return false;
      seen.add(key);
      return true;
    };
    const groups = GROUPS.map((g): Section => {
      const titled = byTitle.filter((h) => h.group === g.id).slice(0, TITLE_LIMIT).filter(unseen)
        .map((h) => ({ ...h, snippet: snippets.get(`${h.group}:${h.target}`) ?? "" }));
      const others = wordHits.filter((h) => h.group === g.id).filter(unseen);
      return { title: g.title, hits: [...titled, ...others], byTitle: titled.length > 0 };
    });
    // un groupe dont un titre correspond passe devant : ses lignes ne bougent plus quand le
    // serveur répond, et Entrée ouvre ce qu'on vient de lire
    groups.sort((a, b) => Number(!!b.byTitle) - Number(!!a.byTitle));
    groups.push({ title: "Proches par le sens", hits: meaningHits.filter(unseen) });
    return groups.filter((s) => s.hits.length);
  }, [trimmed, notes, wordHits, meaningHits]);

  const rows = useMemo(() => sections.flatMap((s) => s.hits), [sections]);
  const active = Math.min(selected, Math.max(rows.length - 1, 0));

  useEffect(() => {
    selectedRow.current?.scrollIntoView({ block: "nearest" });
  }, [active, rows]);

  const choose = (hit?: GlobalHit) => {
    if (!hit) return;
    if (hit.group === "document") navigate(documentHref(hit.target, { page: hit.page, section: hit.section }));
    else if (hit.group === "brouillon") navigate(href("brouillons", null, { d: hit.target }));
    else onOpen(hit.target);
    onClose();
  };

  let index = -1;
  return (
    <div className="modal-backdrop" onMouseDown={onClose}>
      <div className="modal quick-open global-search" onMouseDown={(e) => e.stopPropagation()}>
        <input autoFocus placeholder="Rechercher partout : notes, tâches, documents, brouillons…" value={query}
          onChange={(e) => { setQuery(e.target.value); setSelected(0); }}
          onKeyDown={(e) => {
            if (e.key === "Escape") onClose();
            if (e.key === "ArrowDown") { e.preventDefault(); setSelected(Math.min(active + 1, rows.length - 1)); }
            if (e.key === "ArrowUp") { e.preventDefault(); setSelected(Math.max(active - 1, 0)); }
            if (e.key === "Enter") choose(rows[active]);
          }} />
        <ul>
          {sections.map((section) => [
            <li key={section.title} className="group-title">{section.title}</li>,
            ...section.hits.map((hit) => {
              const i = ++index;
              const where = [hit.section, hit.page ? `p. ${hit.page}` : ""].filter(Boolean).join(" · ");
              return (
                <li key={`${section.title}:${hit.group}:${hit.target}`} ref={i === active ? selectedRow : undefined}
                  className={`hit ${i === active ? "selected" : ""}`} onMouseMove={() => setSelected(i)} onClick={() => choose(hit)}>
                  <div className="hit-head">
                    <span className="hit-title">{hit.title}</span>
                    <span className="muted small">{hitLabel(hit)}{hit.date ? ` · ${hit.date.slice(0, 10)}` : ""}</span>
                  </div>
                  {hit.snippet && (
                    <div className="snippet small">{where && <span className="muted">{where} — </span>}<Snippet text={hit.snippet} /></div>
                  )}
                </li>
              );
            }),
          ])}
          {!rows.length && <li className="muted group-title">{searching ? "Recherche…" : trimmed ? "Aucun résultat" : "Aucune note récente"}</li>}
        </ul>
      </div>
    </div>
  );
}
