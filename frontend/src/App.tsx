import { useCallback, useEffect, useRef, useState } from "react";
import { api, Config, NoteSummary } from "./api";
import EditorPane from "./components/EditorPane";
import ImputationsView from "./components/ImputationsView";
import InboxView from "./components/InboxView";
import QuickOpen from "./components/QuickOpen";
import ReportsView from "./components/ReportsView";
import RightPanel from "./components/RightPanel";
import Sidebar, { View } from "./components/Sidebar";
import TasksView from "./components/TasksView";

type Toast = { id: number; message: string; error: boolean };

export default function App() {
  const [config, setConfig] = useState<Config | null>(null);
  const [files, setFiles] = useState<NoteSummary[]>([]);
  const [revision, setRevision] = useState(0);
  const [inboxCount, setInboxCount] = useState(0);
  const [imputationMissing, setImputationMissing] = useState(0);
  const [view, setView] = useState<View>("tasks");
  const [path, setPath] = useState<string | null>(null);
  const [quickOpen, setQuickOpen] = useState(false);
  const [toasts, setToasts] = useState<Toast[]>([]);
  const lastRevision = useRef(-1);

  const notify = useCallback((message: string, error = false) => {
    const id = Date.now() + Math.random();
    setToasts((t) => [...t, { id, message, error }]);
    window.setTimeout(() => setToasts((t) => t.filter((x) => x.id !== id)), error ? 7000 : 3500);
  }, []);

  // l'index change (édition ici ou dans un autre éditeur) : on rafraîchit les listes
  const poll = useCallback(async () => {
    try {
      const state = await api.state();
      setInboxCount(state.inbox);
      setImputationMissing(state.imputation_missing);
      if (state.revision !== lastRevision.current) {
        lastRevision.current = state.revision;
        setFiles(await api.files());
        setRevision(state.revision);
      }
    } catch {
      /* serveur momentanément indisponible */
    }
  }, []);

  useEffect(() => {
    api.config().then(setConfig).catch((e) => notify(e.message, true));
    poll();
    const handle = window.setInterval(poll, 2500);
    return () => window.clearInterval(handle);
  }, [poll, notify]);

  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if ((e.ctrlKey || e.metaKey) && e.key.toLowerCase() === "p") {
        e.preventDefault();
        setQuickOpen(true);
      }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, []);

  const open = useCallback((p: string | null) => {
    setPath(p);
    setView(p ? "editor" : "tasks");
    window.setTimeout(poll, 300);
  }, [poll]);

  if (!config) return <div className="loading">Chargement…</div>;

  return (
    <div className={`app ${view === "editor" && path ? "with-right" : ""}`}>
      <Sidebar files={files} current={view === "editor" ? path : null} view={view} inboxCount={inboxCount}
        imputationMissing={imputationMissing}
        setView={setView} onOpen={open} notify={notify} />

      <main>
        {view === "editor" && path && (
          <EditorPane path={path} files={files} config={config} revision={revision} onOpen={open} notify={notify} />
        )}
        {view === "editor" && !path && <div className="empty">Aucune note ouverte (Ctrl+P).</div>}
        {view === "tasks" && <TasksView config={config} revision={revision} onOpen={open} notify={notify} />}
        {view === "inbox" && <InboxView config={config} revision={revision} onOpen={open} notify={notify} />}
        {view === "reports" && <ReportsView config={config} files={files} onOpen={open} notify={notify} />}
        {view === "imputations" && <ImputationsView config={config} revision={revision} onOpen={open} notify={notify} />}
      </main>

      {view === "editor" && path && <RightPanel path={path} revision={revision} onOpen={open} />}

      {quickOpen && <QuickOpen files={files} onOpen={open} onClose={() => setQuickOpen(false)} />}

      <div className="toasts">
        {toasts.map((t) => <div key={t.id} className={`toast ${t.error ? "error" : ""}`}>{t.message}</div>)}
      </div>
    </div>
  );
}
