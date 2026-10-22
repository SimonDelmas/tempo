import { KeyboardEvent, useEffect, useId, useState } from "react";
import { library, TagCount, Theme } from "../api";

/** Thèmes de la bibliothèque (notes `themes/*.md`), rechargés à chaque révision. */
export function useThemes(revision: number) {
  const [themes, setThemes] = useState<Theme[]>([]);
  useEffect(() => { library.themes().then(setThemes).catch(() => {}); }, [revision]);
  const add = (t: Theme) => setThemes((prev) =>
    [...prev.filter((x) => x.id !== t.id), t].sort((a, b) => a.title.localeCompare(b.title, "fr")));
  return [themes, add] as const;
}

/** Tags des notes et des documents, des plus fréquents aux plus rares. */
export function useTags(revision: number) {
  const [tags, setTags] = useState<TagCount[]>([]);
  useEffect(() => { library.tags().then(setTags).catch(() => {}); }, [revision]);
  return tags;
}

export const themeTitle = (themes: Theme[], id: string) => themes.find((t) => t.id === id)?.title ?? id;

const NEW = "__nouveau_theme__";

type ThemesProps = {
  themes: Theme[];
  value: string[];
  onChange: (ids: string[]) => void;
  /** appelé après la création d'une note de thème */
  onCreated: (theme: Theme) => void;
  notify: (message: string, error?: boolean) => void;
  /** noms de thèmes à créer proposés par l'IA, ajoutés d'un clic */
  suggestions?: string[];
};

/**
 * Plusieurs thèmes : étiquettes retirables, liste déroulante pour en ajouter (dernière option
 * « Nouveau thème… ») et suggestions de thèmes à créer.
 */
export function ThemesSelect({ themes, value, onChange, onCreated, notify, suggestions = [] }: ThemesProps) {
  const [creating, setCreating] = useState(false);
  const [title, setTitle] = useState("");

  const create = async (name: string) => {
    if (!name.trim()) return;
    try {
      const theme = await library.createTheme(name.trim());
      notify(`Thème « ${theme.title} » créé`);
      onCreated(theme);
      if (!value.includes(theme.id)) onChange([...value, theme.id]);
      setCreating(false);
      setTitle("");
    } catch (e) {
      notify((e as Error).message, true);
    }
  };
  const pending = suggestions.filter((s) => !themes.some((t) => t.title.toLowerCase() === s.toLowerCase()));

  return (
    <span className="projects-select">
      {value.map((id) => (
        <span key={id} className="tag">
          {themeTitle(themes, id)}
          <button className="ghost mini" title="Retirer" onClick={() => onChange(value.filter((x) => x !== id))}>×</button>
        </span>
      ))}
      {creating ? (
        <span className="project-select">
          <input autoFocus placeholder="Nom du nouveau thème" value={title} onChange={(e) => setTitle(e.target.value)}
            onKeyDown={(e) => {
              if (e.key === "Enter") { e.preventDefault(); create(title); }
              if (e.key === "Escape") { e.stopPropagation(); setCreating(false); }
            }} />
          <button className="primary" disabled={!title.trim()} onClick={() => create(title)}>Créer</button>
          <button className="ghost" onClick={() => setCreating(false)}>Annuler</button>
        </span>
      ) : (
        <select value="" onChange={(e) => {
          if (e.target.value === NEW) setCreating(true);
          else if (e.target.value && !value.includes(e.target.value)) onChange([...value, e.target.value]);
        }}>
          <option value="">Ajouter un thème…</option>
          {themes.filter((t) => !value.includes(t.id)).map((t) => <option key={t.id} value={t.id}>{t.title}</option>)}
          <option value={NEW}>+ Nouveau thème…</option>
        </select>
      )}
      {pending.map((s) => (
        <button key={s} className="ghost mini suggestion" title="Créer ce thème proposé par l'IA" onClick={() => create(s)}>
          + {s}
        </button>
      ))}
    </span>
  );
}

/** Tags : étiquettes retirables et saisie avec les tags existants en suggestion (Entrée ou virgule pour ajouter). */
export function TagsInput({ value, onChange, suggestions }: {
  value: string[];
  onChange: (tags: string[]) => void;
  suggestions: TagCount[];
}) {
  const [text, setText] = useState("");
  const listId = useId();
  const add = (raw: string) => {
    const tags = raw.split(",").map((t) => t.trim().replace(/^#/, "").toLowerCase().replace(/\s+/g, "-")).filter(Boolean);
    const next = [...value];
    for (const tag of tags) if (!next.includes(tag)) next.push(tag);
    onChange(next);
    setText("");
  };
  const onKey = (e: KeyboardEvent<HTMLInputElement>) => {
    if ((e.key === "Enter" || e.key === ",") && text.trim()) {
      e.preventDefault();
      add(text);
    } else if (e.key === "Backspace" && !text && value.length) {
      onChange(value.slice(0, -1));
    }
  };
  return (
    <span className="projects-select tags-input">
      {value.map((tag) => (
        <span key={tag} className="tag">
          {tag}
          <button className="ghost mini" title="Retirer" onClick={() => onChange(value.filter((t) => t !== tag))}>×</button>
        </span>
      ))}
      <input list={listId} placeholder={value.length ? "" : "Ajouter un tag…"} value={text}
        onChange={(e) => {
          // choix dans la liste des suggestions : ajouté directement
          if (suggestions.some((s) => s.tag === e.target.value)) add(e.target.value);
          else setText(e.target.value);
        }}
        onKeyDown={onKey} onBlur={() => text.trim() && add(text)} />
      <datalist id={listId}>
        {suggestions.filter((s) => !value.includes(s.tag)).slice(0, 200).map((s) => <option key={s.tag} value={s.tag} />)}
      </datalist>
    </span>
  );
}
