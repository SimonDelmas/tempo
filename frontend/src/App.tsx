import { useCallback, useEffect, useRef, useState } from "react";
import { api, Config, NoteSummary } from "./api";
import { ChatEntry, useChats } from "./chats";
import ChatPanel, { useChatPanel } from "./components/ChatPanel";
import DraftsView from "./components/DraftsView";
import EditorPane, { EditorActions } from "./components/EditorPane";
import ImputationsView from "./components/ImputationsView";
import InboxView from "./components/InboxView";
import LibraryView, { LibrarySidebar } from "./components/LibraryView";
import NotesHome from "./components/NotesHome";
import NotesSidebar from "./components/NotesSidebar";
import QuickOpen from "./components/QuickOpen";
import ReportsView from "./components/ReportsView";
import RightPanel from "./components/RightPanel";
import SpaceBar from "./components/SpaceBar";
import { ImputationsSidebar, ReportsSidebar, TasksSidebar } from "./components/SpaceSidebars";
import TasksView, { TasksTab } from "./components/TasksView";
import { goBack, goForward, href, matchShortcut, navigate, noteHref, Space, SPACES, useRoute } from "./router";

/** `action` : la notification est cliquable (ouvre par exemple la conversation concernée) */
type Toast = { id: number; message: string; error: boolean; action?: () => void };

const LAST_ROUTES_KEY = "tempo.lastRoutes";

function loadLastRoutes(): Partial<Record<Space, string>> {
  try {
    return JSON.parse(localStorage.getItem(LAST_ROUTES_KEY) ?? "{}");
  } catch {
    return {};
  }
}

export default function App() {
  const [config, setConfig] = useState<Config | null>(null);
  const [files, setFiles] = useState<NoteSummary[]>([]);
  const [revision, setRevision] = useState(0);
  const [inboxCount, setInboxCount] = useState(0);
  const [imputationMissing, setImputationMissing] = useState(0);
  const [libraryPending, setLibraryPending] = useState(0);
  const [quickOpen, setQuickOpen] = useState(false);
  const [toasts, setToasts] = useState<Toast[]>([]);
  const lastRevision = useRef(-1);
  const lastRoutes = useRef(loadLastRoutes());
  const editorActions = useRef<EditorActions>({});
  const route = useRoute();
  const { space, path, params } = route;

  const notify = useCallback((message: string, error = false, action?: () => void) => {
    const id = Date.now() + Math.random();
    setToasts((t) => [...t, { id, message, error, action }]);
    window.setTimeout(() => setToasts((t) => t.filter((x) => x.id !== id)), error || action ? 7000 : 3500);
  }, []);

  const [chatPanel, setChatPanel] = useChatPanel();
  const chat = useChats({
    onCreated: (current) => setChatPanel({ current }),
    onFinished: (conversation, seen, error) => {
      if (seen) return;
      const open = () => setChatPanel({ open: true, current: conversation.id });
      notify(`${error ? "Échec de l'agent" : "Réponse de l'agent prête"} : « ${conversation.title} »`, error, open);
    },
  });
  // la conversation affichée dans le panneau déplié est marquée comme lue
  const viewedChat = chatPanel.open ? chatPanel.current : null;
  useEffect(() => chat.view(viewedChat), [chat.view, viewedChat]);

  // l'index change (édition ici ou dans un autre éditeur) : on rafraîchit les listes
  const poll = useCallback(async () => {
    try {
      const state = await api.state();
      setInboxCount(state.inbox);
      setImputationMissing(state.imputation_missing);
      setLibraryPending(state.library);
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

  // chaque espace retrouve la dernière page consultée
  useEffect(() => {
    lastRoutes.current[space] = window.location.hash;
    try {
      localStorage.setItem(LAST_ROUTES_KEY, JSON.stringify(lastRoutes.current));
    } catch {
      /* stockage indisponible */
    }
  }, [space, path, params]);

  const selectSpace = useCallback((target: Space) => {
    // un second clic sur l'espace courant ramène à sa page d'accueil
    navigate(target === space ? href(target) : lastRoutes.current[target] || href(target));
  }, [space]);

  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      // Alt+1…6 : changer d'espace (Ctrl+chiffre est réservé aux onglets du navigateur)
      const index = Number(e.key) - 1;
      if (e.altKey && !e.ctrlKey && index >= 0 && index < SPACES.length) {
        e.preventDefault();
        selectSpace(SPACES[index].id);
      }
      if ((e.ctrlKey || e.metaKey) && e.key.toLowerCase() === "p") {
        e.preventDefault();
        setQuickOpen(true);
      }
      // Alt+C : déplie le chat et passe à la saisie ; depuis la saisie, le réduit
      if (e.altKey && !e.ctrlKey && !e.shiftKey && e.key.toLowerCase() === "c") {
        e.preventDefault();
        const input = () => document.querySelector<HTMLTextAreaElement>(".chat-panel textarea");
        if (chatPanel.open && document.activeElement === input()) {
          setChatPanel({ open: false });
        } else {
          setChatPanel({ open: true });
          window.setTimeout(() => input()?.focus(), 0);
        }
      }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [selectSpace, chatPanel.open, setChatPanel]);

  // vue précédente / suivante : raccourcis configurables (capturés avant l'éditeur, dont
  // Alt+flèche déplace sinon le curseur) et boutons latéraux de la souris
  const shortcuts = config?.shortcuts;
  useEffect(() => {
    if (!shortcuts) return;
    const onKey = (e: KeyboardEvent) => {
      const action = matchShortcut(e, shortcuts.back) ? goBack : matchShortcut(e, shortcuts.forward) ? goForward : null;
      if (!action) return;
      e.preventDefault();
      e.stopPropagation();
      action();
    };
    const onMouse = (e: MouseEvent) => {
      if (e.button !== 3 && e.button !== 4) return;
      e.preventDefault();
      (e.button === 3 ? goBack : goForward)();
    };
    window.addEventListener("keydown", onKey, true);
    window.addEventListener("mouseup", onMouse);
    return () => {
      window.removeEventListener("keydown", onKey, true);
      window.removeEventListener("mouseup", onMouse);
    };
  }, [shortcuts]);

  const open = useCallback((p: string | null) => {
    navigate(p ? noteHref(p) : href(space));
    window.setTimeout(poll, 300);
  }, [poll, space]);

  const replaceChatEntry = useCallback((id: string, index: number, entry: ChatEntry) => {
    chat.replace(id, index, entry);
    window.setTimeout(poll, 300);
  }, [chat.replace, poll]);

  if (!config) return <div className="loading">Chargement…</div>;

  /** espace Tâches : onglet, semaine du board et filtre projet portés par l'adresse */
  const tasksTab: TasksTab = params.vue === "backlog" ? "backlog" : "board";
  const tasksNav = (changes: { vue?: TasksTab; semaine?: string | null; projet?: string | null }) =>
    navigate(href("taches", null, { vue: params.vue, semaine: params.semaine, projet: params.projet, ...changes }));

  /** espace Bibliothèque : document ouvert, vue, filtres et regroupement portés par l'adresse */
  const libraryNav = (changes: Record<string, string | null>) => navigate(href("bibliotheque", null, {
    doc: params.doc, vue: params.vue, theme: params.theme, projet: params.projet, tag: params.tag, par: params.par, ...changes,
  }));

  const sidebar = {
    notes: <NotesSidebar files={files} current={path} semantic={config.semantic_enabled} onOpen={open}
      notify={notify} />,
    taches: <TasksSidebar revision={revision} current={path} tab={tasksTab} project={params.projet ?? null}
      onOpen={open} onNavigate={tasksNav} />,
    rapports: <ReportsSidebar files={files} current={path} onOpen={open} onHome={() => navigate(href("rapports"))} />,
    imputations: <ImputationsSidebar files={files} revision={revision} current={path} week={params.date ?? null}
      onOpen={open} onWeek={(date) => navigate(href("imputations", null, { date }))} />,
    inbox: null,
    brouillons: null,
    bibliotheque: <LibrarySidebar revision={revision} params={params} pending={libraryPending} onNavigate={libraryNav} />,
  }[space];

  const home = {
    notes: <NotesHome files={files} onOpen={open} />,
    taches: <TasksView config={config} revision={revision} tab={tasksTab} week={params.semaine ?? ""}
      project={params.projet ?? ""} onNavigate={tasksNav} onOpen={open} notify={notify} />,
    rapports: <ReportsView config={config} onOpen={open} notify={notify} />,
    imputations: <ImputationsView config={config} revision={revision} date={params.date ?? null}
      onDate={(date) => navigate(href("imputations", null, { date }))} onOpen={open} notify={notify} />,
    inbox: <InboxView config={config} revision={revision} onOpen={open} notify={notify} />,
    brouillons: <DraftsView config={config} files={files} current={params.d ?? null}
      onSelect={(d) => navigate(href("brouillons", null, { d }))} onOpen={open} notify={notify} />,
    bibliotheque: <LibraryView config={config} revision={revision} params={params} onNavigate={libraryNav} notify={notify}
      onAsk={(question) => { setChatPanel({ open: true }); chat.send(null, question); }} />,
  }[space];

  return (
    <div className="app">
      <SpaceBar current={space} onSelect={selectSpace} shortcuts={config.shortcuts}
        badges={{ inbox: inboxCount, imputations: imputationMissing, bibliotheque: libraryPending }} />
      {sidebar}

      <main>
        {path
          ? <EditorPane path={path} files={files} config={config} revision={revision} onOpen={open} notify={notify}
            actions={editorActions} />
          : home}
      </main>

      {path && <RightPanel path={path} revision={revision} semantic={config.semantic_enabled} onOpen={open}
        onLink={(name) => editorActions.current.appendLink?.(name)} />}

      <ChatPanel config={config} panel={chatPanel} onPanel={setChatPanel} chats={chat.chats} live={chat.live}
        onSend={(question) => chat.send(chatPanel.current, question)}
        onReplace={replaceChatEntry} onOpen={open} notify={notify}
        onDelete={(id) => {
          chat.remove(id);
          if (chatPanel.current === id) setChatPanel({ current: null });
        }} />

      {quickOpen && <QuickOpen files={files} onOpen={open} onClose={() => setQuickOpen(false)} />}

      <div className="toasts">
        {toasts.map((t) => (
          <div key={t.id} className={`toast ${t.error ? "error" : ""} ${t.action ? "clickable" : ""}`}
            onClick={t.action && (() => {
              t.action!();
              setToasts((all) => all.filter((x) => x.id !== t.id));
            })}>
            {t.message}
          </div>
        ))}
      </div>
    </div>
  );
}
