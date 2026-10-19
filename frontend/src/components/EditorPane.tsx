import { useCallback, useEffect, useRef, useState } from "react";
import { EditorView } from "@codemirror/view";
import { api, ApiError, Config, Note, NoteSummary, STATUS_LABELS, TaskDraft, TextAction, toEmailText,
  TYPE_LABELS } from "../api";
import { ExtractTasksDialog, TEXT_ACTION_LABELS, TextResultDialog } from "./EditorAssist";
import MarkdownEditor from "./MarkdownEditor";

const PREVIEW_KEY = "tempo.livePreview";

type SaveStatus ="enregistré" | "modifié" | "enregistrement…" | "conflit" | "erreur";

type Props = {
  path: string;
  files: NoteSummary[];
  config: Config;
  revision: number;
  onOpen: (path: string | null) => void;
  notify: (message: string, error?: boolean) => void;
};

export default function EditorPane({ path, files, config, revision, onOpen, notify }: Props) {
  const [note, setNote] = useState<Note | null>(null);
  const [editorKey, setEditorKey] = useState(0);
  const [status, setStatus] = useState<SaveStatus>("enregistré");
  const [conflict, setConflict] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const content = useRef("");
  const hash = useRef<string | null>(null);
  const dirty = useRef(false);
  const timer = useRef<number | undefined>(undefined);
  const pathRef = useRef(path);
  const viewRef = useRef<EditorView | null>(null);
  const [assistMenu, setAssistMenu] = useState(false);
  const [preview, setPreview] = useState(() => {
    try {
      return localStorage.getItem(PREVIEW_KEY) !== "off";
    } catch {
      return true;
    }
  });
  const [drafts, setDrafts] = useState<TaskDraft[] | null>(null);
  const [textResult, setTextResult] = useState<
    { action: TextAction; result: string; from: number; to: number; hasSelection: boolean } | null>(null);

  const load = useCallback(async (p: string) => {
    const n = await api.readNote(p);
    content.current = n.content;
    hash.current = n.hash;
    dirty.current = false;
    setNote(n);
    setConflict(null);
    setStatus("enregistré");
    setEditorKey((k) => k + 1);
  }, []);

  const save = useCallback(async () => {
    window.clearTimeout(timer.current);
    if (!dirty.current) return;
    const p = pathRef.current;
    const sent = content.current;
    setStatus("enregistrement…");
    try {
      const result = await api.writeNote(p, sent, hash.current);
      hash.current = result.hash;
      if (content.current === sent) {
        dirty.current = false;
        setStatus("enregistré");
      }
    } catch (e) {
      if (e instanceof ApiError && e.status === 409) {
        setConflict(e.body.current_content);
        setStatus("conflit");
      } else {
        setStatus("erreur");
        notify((e as Error).message, true);
      }
    }
  }, [notify]);

  // changement de note : on enregistre la précédente puis on charge la nouvelle
  useEffect(() => {
    pathRef.current = path;
    load(path).catch((e) => notify(e.message, true));
    return () => {
      if (dirty.current) {
        window.clearTimeout(timer.current);
        api.writeNote(path, content.current, hash.current).catch(() => {});
        dirty.current = false;
      }
    };
  }, [path, load, notify]);

  // modification externe (autre éditeur) : rechargement si pas de modification locale
  useEffect(() => {
    if (dirty.current || conflict !== null) return;
    api.readNote(path).then((n) => {
      if (n.hash !== hash.current && !dirty.current) load(path);
    }).catch(() => {});
  }, [revision]); // eslint-disable-line react-hooks/exhaustive-deps

  const onChange = (text: string) => {
    content.current = text;
    dirty.current = true;
    setStatus("modifié");
    window.clearTimeout(timer.current);
    timer.current = window.setTimeout(save, 800);
  };

  const openLink = async (name: string) => {
    try {
      onOpen((await api.resolve(name)).path);
    } catch {
      notify(`Aucune note ne correspond à « ${name} »`, true);
    }
  };

  const meta = note?.meta ?? {};
  const type = String(meta.type ?? files.find((f) => f.path === path)?.type ?? "note");
  const projects = Array.isArray(meta.projects) ? (meta.projects as string[]) : [];
  const project = (meta.project as string) || projects[0] || null;

  const createTask = async (title: string) => {
    const task = await api.createTask({ title, project });
    notify(`Tâche ${task.id} créée`);
    return task.id;
  };

  const updateTask = async (field: string, value: string) => {
    await save();
    try {
      await api.updateTask(String(meta.id), { [field]: value || null });
      await load(path);
    } catch (e) {
      notify((e as Error).message, true);
    }
  };

  const run = async (action: () => Promise<void>) => {
    setBusy(true);
    try {
      await action();
    } catch (e) {
      notify((e as Error).message, true);
    } finally {
      setBusy(false);
    }
  };

  // --- actions IA (la note est enregistrée avant, le résultat est validé avant écriture) ---

  const insertText = (from: number, to: number, text: string) => {
    viewRef.current?.dispatch({ changes: { from, to, insert: text } });
  };

  const appendToNote = (text: string) => {
    const doc = viewRef.current?.state.doc;
    if (!doc) return;
    const current = doc.toString();
    const sep = current.endsWith("\n\n") ? "" : current.endsWith("\n") ? "\n" : "\n\n";
    insertText(doc.length, doc.length, `${sep}${text}\n`);
  };

  const extractTasks = () => run(async () => {
    setAssistMenu(false);
    await save();
    setDrafts((await api.extractTasks(path)).tasks);
  });

  const textAction = (action: TextAction) => run(async () => {
    setAssistMenu(false);
    const view = viewRef.current;
    if (!view) return;
    const sel = view.state.selection.main;
    const text = sel.empty
      ? view.state.doc.toString().replace(/^---\n[\s\S]*?\n---\n/, "")
      : view.state.sliceDoc(sel.from, sel.to);
    const { text: result } = await api.transformText(action, text);
    setTextResult({ action, result, from: sel.from, to: sel.to, hasSelection: !sel.empty });
  });

  const origin = (() => {
    const file = files.find((f) => f.path === path);
    return file ? file.alias || file.stem : path;
  })();

  return (
    <div className="editor-pane">
      <header className="editor-header">
        <span className="badge">{TYPE_LABELS[type] ?? type}</span>
        <span className="path" title={path}>{path}</span>
        <span className={`save-status ${status === "conflit" || status === "erreur" ? "error" : ""}`}>
          {status}
        </span>
        <span className="spacer" />
        <button className={`ghost toggle ${preview ? "on" : ""}`}
          title="Aperçu en direct : masque la syntaxe Markdown hors de la ligne en cours"
          onClick={() => {
            const next = !preview;
            setPreview(next);
            try {
              localStorage.setItem(PREVIEW_KEY, next ? "on" : "off");
            } catch {
              /* stockage indisponible */
            }
          }}>
          Aperçu
        </button>
        <div className="menu-host">
          <button disabled={busy || !config.llm_enabled} onClick={() => setAssistMenu((o) => !o)}
            title={config.llm_enabled ? "Actions IA sur la note ou la sélection" : "API LLM non configurée"}>
            {busy ? "IA…" : "IA ▾"}
          </button>
          {assistMenu && (
            <div className="menu" onMouseLeave={() => setAssistMenu(false)}>
              <button onClick={extractTasks}>Extraire des tâches de la note</button>
              {(Object.keys(TEXT_ACTION_LABELS) as TextAction[]).map((a) => (
                <button key={a} onClick={() => textAction(a)}>{TEXT_ACTION_LABELS[a]} (sélection ou note)</button>
              ))}
            </div>
          )}
        </div>
        {type === "rapport" && (
          <button onClick={() => navigator.clipboard.writeText(toEmailText(content.current))
            .then(() => notify("Rapport copié (sans les liens sources)"))}>
            Copier pour email
          </button>
        )}
        <button className="ghost" onClick={() => {
          if (window.confirm(`Mettre « ${path} » à la corbeille du vault (.trash) ?`)) {
            dirty.current = false;
            api.deleteNote(path).then(() => onOpen(null));
          }
        }}>Supprimer</button>
      </header>

      {type === "tache" && note && (
        <div className="task-bar">
          <strong>{String(meta.id)}</strong>
          <label>Statut
            <select value={String(meta.status ?? "a_faire")} onChange={(e) => updateTask("status", e.target.value)}>
              {config.statuses.map((s) => <option key={s} value={s}>{STATUS_LABELS[s]}</option>)}
            </select>
          </label>
          <label>Priorité
            <select value={String(meta.priority ?? "normale")} onChange={(e) => updateTask("priority", e.target.value)}>
              {config.priorities.map((p) => <option key={p}>{p}</option>)}
            </select>
          </label>
          <label>Échéance
            <input type="date" value={String(meta.due ?? "")} onChange={(e) => updateTask("due", e.target.value)} />
          </label>
          <label>Projet
            <input defaultValue={String(meta.project ?? "")} key={String(meta.project ?? "")}
              onBlur={(e) => e.target.value !== (meta.project ?? "") && updateTask("project", e.target.value)} />
          </label>
          <span className="spacer" />
          <button disabled={busy || !config.llm_enabled} title={config.llm_enabled ? "" : "API LLM non configurée"}
            onClick={() => run(async () => {
              notify("Génération du rapport d'avancement…");
              onOpen((await api.taskReport(String(meta.id))).path);
            })}>
            {busy ? "Génération…" : "Rapport d'avancement"}
          </button>
        </div>
      )}

      {conflict !== null && (
        <div className="conflict">
          Ce fichier a été modifié dans un autre éditeur pendant que vous l'éditiez.
          <button onClick={() => load(path)}>Recharger la version du disque</button>
          <button className="danger" onClick={async () => {
            await api.writeNote(path, content.current, null);
            await load(path);
          }}>Écraser avec ma version</button>
        </div>
      )}

      {note && (
        <MarkdownEditor key={editorKey} initial={note.content} files={files} onChange={onChange}
          onOpenLink={openLink} onCreateTask={createTask} viewRef={viewRef} preview={preview} />
      )}

      {drafts && (
        <ExtractTasksDialog drafts={drafts} config={config} origin={origin} notify={notify}
          onClose={() => setDrafts(null)}
          onCreated={(created) => {
            setDrafts(null);
            if (created.length) appendToNote(created.map((t) => `- [[${t.id}]] ${t.title}`).join("\n"));
          }} />
      )}
      {textResult && (
        <TextResultDialog action={textResult.action} result={textResult.result} notify={notify}
          hasSelection={textResult.hasSelection} onClose={() => setTextResult(null)}
          onReplace={(text) => { insertText(textResult.from, textResult.to, text); setTextResult(null); }}
          onInsert={(text) => {
            if (textResult.hasSelection) insertText(textResult.to, textResult.to, `\n\n${text}\n`);
            else appendToNote(text);
            setTextResult(null);
          }} />
      )}
    </div>
  );
}
