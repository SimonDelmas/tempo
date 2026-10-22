import { useEffect, useRef, useState } from "react";
import { Config, DocumentMeta, fileSize, FORMAT_LABELS, library, LibraryDocument, LibraryQueue as Queue, Project,
  TagCount, Theme } from "../api";
import { ProjectsSelect, useProjects } from "./ProjectSelect";
import { TagsInput, ThemesSelect, useTags, useThemes } from "./ThemeSelect";

type Notify = (message: string, error?: boolean) => void;

export const metaOf = (d: LibraryDocument & { replaces?: string | null }): DocumentMeta => ({
  title: d.title, summary: d.summary, themes: d.themes, projects: d.projects, tags: d.tags, replaces: d.replaces ?? null,
});

/** Formulaire de classement d'un document : titre, résumé, thèmes, projets, tags, remplacement. */
export function ClassificationForm({ value, onChange, themes, onThemeCreated, projects, onProjectCreated, tags,
  suggestedThemes = [], replaceable, notify }: {
  value: DocumentMeta;
  onChange: (meta: DocumentMeta) => void;
  themes: Theme[];
  onThemeCreated: (theme: Theme) => void;
  projects: Project[];
  onProjectCreated: (project: Project) => void;
  tags: TagCount[];
  suggestedThemes?: string[];
  /** documents que celui-ci peut remplacer (file « À classer ») */
  replaceable?: LibraryDocument[];
  notify: Notify;
}) {
  const set = (patch: Partial<DocumentMeta>) => onChange({ ...value, ...patch });
  return (
    <div className="grid-form">
      <label className="wide">Titre<input value={value.title} onChange={(e) => set({ title: e.target.value })} /></label>
      <label className="wide">Résumé
        <textarea rows={3} value={value.summary} onChange={(e) => set({ summary: e.target.value })} />
      </label>
      <label className="wide">Thèmes
        <ThemesSelect themes={themes} value={value.themes} onChange={(ids) => set({ themes: ids })}
          onCreated={onThemeCreated} notify={notify} suggestions={suggestedThemes} />
      </label>
      <label>Projets
        <ProjectsSelect projects={projects} value={value.projects} onChange={(ids) => set({ projects: ids })}
          onCreated={onProjectCreated} notify={notify} />
      </label>
      <label>Tags<TagsInput value={value.tags} onChange={(t) => set({ tags: t })} suggestions={tags} /></label>
      {replaceable && (
        <label className="wide">Version
          <select value={value.replaces ?? ""} onChange={(e) => set({ replaces: e.target.value || null })}>
            <option value="">Nouveau document</option>
            {replaceable.map((d) => <option key={d.id} value={d.id}>Remplace « {d.title} » ({d.id})</option>)}
          </select>
        </label>
      )}
    </div>
  );
}

type CardState = {
  meta: DocumentMeta;
  selected: boolean;
  /** attente : proposition de l'IA à demander ; manuel : sans IA */
  status: "attente" | "analyse" | "pret" | "manuel" | "erreur" | "envoi";
  error?: string;
  suggestions: string[];
};

type Props = {
  config: Config;
  revision: number;
  notify: Notify;
  onOpenDocument: (id: string) => void;
  onPickFiles: () => void;
};

/** File « À classer » : documents déposés dont le classement proposé par l'IA attend la validation. */
export default function LibraryQueue({ config, revision, notify, onOpenDocument, onPickFiles }: Props) {
  const [queue, setQueue] = useState<Queue>({ pending: [], untracked: [] });
  const [documents, setDocuments] = useState<LibraryDocument[]>([]);
  const [cards, setCards] = useState<Record<string, CardState>>({});
  const [busy, setBusy] = useState(false);
  const [themes, addTheme] = useThemes(revision);
  const [projects, addProject] = useProjects(revision);
  const tags = useTags(revision);
  const classifying = useRef<string | null>(null);

  useEffect(() => {
    library.queue().then((q) => {
      setQueue(q);
      // une carte déjà affichée garde sa saisie ; les nouvelles partent du frontmatter
      setCards((prev) => Object.fromEntries(q.pending.map((d) => [d.id, prev[d.id] ?? {
        meta: metaOf(d), selected: false, status: config.llm_enabled ? "attente" : "manuel", suggestions: [],
      }])));
    }).catch((e) => notify(e.message, true));
    library.list().then((all) => setDocuments(all.filter((d) => !d.pending))).catch(() => {});
  }, [revision]); // eslint-disable-line react-hooks/exhaustive-deps

  const update = (id: string, patch: Partial<CardState>) =>
    setCards((prev) => (prev[id] ? { ...prev, [id]: { ...prev[id], ...patch } } : prev));

  // propositions demandées une à une, dans l'ordre de la file
  useEffect(() => {
    if (classifying.current) return;
    const next = queue.pending.find((d) => cards[d.id]?.status === "attente");
    if (!next) return;
    classifying.current = next.id;
    update(next.id, { status: "analyse", error: undefined });
    library.classify(next.id)
      .then((c) => setCards((prev) => prev[next.id] ? {
        ...prev,
        [next.id]: {
          ...prev[next.id], status: "pret", selected: true, suggestions: c.new_themes,
          meta: { title: c.title, summary: c.summary, themes: c.themes, projects: c.projects, tags: c.tags,
                  replaces: c.replaces ?? prev[next.id].meta.replaces ?? null },
        },
      } : prev))
      .catch((e) => update(next.id, { status: "erreur", error: (e as Error).message }))
      .finally(() => { classifying.current = null; setCards((prev) => ({ ...prev })); });
  }, [queue, cards]);

  const validate = async (ids: string[]) => {
    setBusy(true);
    let done = 0;
    for (const id of ids) {
      const card = cards[id];
      if (!card) continue;
      update(id, { status: "envoi" });
      try {
        const saved = await library.update(id, card.meta);
        done += 1;
        setQueue((q) => ({ ...q, pending: q.pending.filter((d) => d.id !== id) }));
        setCards((prev) => Object.fromEntries(Object.entries(prev).filter(([k]) => k !== id)));
        if (ids.length === 1) notify(`« ${saved.title} » classé`);
      } catch (e) {
        update(id, { status: "erreur", error: (e as Error).message });
      }
    }
    if (ids.length > 1) notify(`${done} document(s) classé(s)${done < ids.length ? `, ${ids.length - done} en erreur` : ""}`);
    setBusy(false);
  };

  const remove = async (d: LibraryDocument) => {
    if (!window.confirm(`Retirer « ${d.title} » de la bibliothèque ? (fichiers déplacés dans .trash)`)) return;
    try {
      await library.remove(d.id);
      setQueue((q) => ({ ...q, pending: q.pending.filter((x) => x.id !== d.id) }));
    } catch (e) {
      notify((e as Error).message, true);
    }
  };

  const ingest = async (paths: string[]) => {
    setBusy(true);
    let done = 0;
    for (const path of paths) {
      try {
        const result = await library.ingest(path);
        done += 1;
        result.warnings.forEach((w) => notify(w, true));
      } catch (e) {
        notify(`${path} : ${(e as Error).message}`, true);
      }
    }
    if (done) notify(`${done} fichier(s) ajouté(s) à la file`);
    setQueue(await library.queue());
    setBusy(false);
  };

  const selected = queue.pending.filter((d) => cards[d.id]?.selected && ["pret", "manuel"].includes(cards[d.id].status));
  const supported = queue.untracked.filter((f) => f.format);

  return (
    <div className="page library-queue">
      <header className="page-header">
        <h2>À classer</h2>
        <span className="count">{queue.pending.length} document(s)</span>
        <span className="spacer" />
        <button onClick={onPickFiles}>Ajouter des fichiers…</button>
        <button className="primary" disabled={busy || !selected.length} onClick={() => validate(selected.map((d) => d.id))}>
          Valider la sélection ({selected.length})
        </button>
      </header>
      {!config.llm_enabled && (
        <p className="warning">API LLM non configurée : pas de proposition de classement, renseignez-le à la main.</p>
      )}

      {queue.untracked.length > 0 && (
        <section className="panel untracked">
          <div className="row">
            <strong>Fichiers copiés dans le dossier</strong>
            <span className="muted small">{config.library.dir}</span>
            <span className="spacer" />
            <button disabled={busy || !supported.length} onClick={() => ingest(supported.map((f) => f.path))}>
              Tout ajouter ({supported.length})
            </button>
          </div>
          <ul className="untracked-list">
            {queue.untracked.map((f) => (
              <li key={f.path} className={f.format ? "" : "muted"}>
                <span className="grow">{f.path}</span>
                <span className="small muted">{f.format ? FORMAT_LABELS[f.format] ?? f.format : "format non pris en charge"} · {fileSize(f.size)}</span>
                {f.format && <button className="mini" disabled={busy} onClick={() => ingest([f.path])}>Ajouter</button>}
              </li>
            ))}
          </ul>
        </section>
      )}

      {queue.pending.length === 0 && queue.untracked.length === 0 && (
        <p className="muted">Rien à classer. Déposez des fichiers depuis l'explorateur pour les ajouter à la bibliothèque.</p>
      )}

      {queue.pending.map((d) => {
        const card = cards[d.id];
        if (!card) return null;
        const working = card.status === "analyse" || card.status === "envoi";
        return (
          <section key={d.id} className={`panel queue-card ${card.selected ? "selected" : ""}`}>
            <div className="row queue-card-head">
              <input type="checkbox" checked={card.selected} disabled={working}
                onChange={(e) => update(d.id, { selected: e.target.checked })} title="Inclure dans la validation groupée" />
              <button className="link strong" onClick={() => onOpenDocument(d.id)} title="Voir le texte converti">{d.id}</button>
              <span className="badge">{FORMAT_LABELS[d.format] ?? d.format}</span>
              <span className="small muted">
                {fileSize(d.size)}{d.pages ? ` · ${d.pages} ${d.format.startsWith("ppt") ? "diapositives" : "pages"}` : ""}
                {d.attached_to ? ` · pièce jointe de ${d.attached_to.replace(/^\[\[|\]\]$/g, "")}` : ""}
              </span>
              <span className="spacer" />
              {card.status === "analyse" && <span className="small muted">Proposition de l'IA…</span>}
              {(card.status === "pret" || card.status === "erreur") && config.llm_enabled && (
                <button className="ghost mini" onClick={() => update(d.id, { status: "attente" })}>Relancer l'IA</button>
              )}
              <button className="ghost mini danger" disabled={working} onClick={() => remove(d)}>Retirer</button>
              <button className="primary" disabled={working || busy} onClick={() => validate([d.id])}>Valider</button>
            </div>
            {card.error && <p className="warning small">{card.error}</p>}
            <fieldset disabled={working} className="plain">
              <ClassificationForm value={card.meta} onChange={(meta) => update(d.id, { meta })} themes={themes}
                onThemeCreated={addTheme} projects={projects} onProjectCreated={addProject} tags={tags}
                suggestedThemes={card.suggestions} replaceable={documents} notify={notify} />
            </fieldset>
          </section>
        );
      })}
    </div>
  );
}
