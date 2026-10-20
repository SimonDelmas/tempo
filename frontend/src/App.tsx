import { useCallback, useEffect, useRef, useState } from "react";
import { api, Config, NoteSummary } from "./api";
import { ChatEntry, useChats } from "./chats";
import ChatView from "./components/ChatView";
import DraftsView from "./components/DraftsView";
import EditorPane, { EditorActions } from "./components/EditorPane";
import ImputationsView from "./components/ImputationsView";
import InboxView from "./components/InboxView";
import NotesHome from "./components/NotesHome";
import NotesSidebar from "./components/NotesSidebar";
import QuickOpen from "./components/QuickOpen";
import ReportsView from "./components/ReportsView";
import RightPanel from "./components/RightPanel";
import SpaceBar from "./components/SpaceBar";
import { ChatSidebar, ImputationsSidebar, ReportsSidebar, TasksSidebar } from "./components/SpaceSidebars";
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

  const chat = useChats({
    onCreated: (c) => navigate(href("chat", null, { c })),
    onFinished: (conversation, seen, error) => {
      if (seen) return;
      const open = () => navigate(href("chat", null, { c: conversation.id }));
      notify(`${error ? "Échec de l'agent" : "Réponse de l'agent prête"} : « ${conversation.title} »`, error, open);
    },
  });
  // la conversation affichée est marquée comme lue
  const viewedChat = space === "chat" && !path ? params.c ?? null : null;
  useEffect(() => chat.view(viewedChat), [chat.view, viewedChat]);

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
      // Alt+1…7 : changer d'espace (Ctrl+chiffre est réservé aux onglets du navigateur)
      const index = Number(e.key) - 1;
      if (e.altKey && !e.ctrlKey && index >= 0 && index < SPACES.length) {
        e.preventDefault();
        selectSpace(SPACES[index].id);
      }
      if ((e.ctrlKey || e.metaKey) && e.key.toLowerCase() === "p") {
        e.preventDefault();
        setQuickOpen(true);
      }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [selectSpace]);

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
    chat: <ChatSidebar chats={chat.chats} live={chat.live} current={params.c ?? null}
      onSelect={(c) => navigate(href("chat", null, { c }))}
      onDelete={(id) => {
        chat.remove(id);
        if (params.c === id) navigate(href("chat"));
      }} />,
  }[space];

  const home = {
    notes: <NotesHome files={files} onOpen={open} />,
    taches: <TasksView config={config} revision={revision} tab={tasksTab} week={params.semaine ?? ""}
      project={params.projet ?? ""} onNavigate={tasksNav} onOpen={open} notify={notify} />,
    rapports: <ReportsView config={config} onOpen={open} notify={notify} />,
    imputations: <ImputationsView config={config} revision={revision} date={params.date ?? null}
      onDate={(date) => navigate(href("imputations", null, { date }))} onOpen={open} notify={notify} />,
    inbox: <InboxView config={config} revision={revision} onOpen={open} notify={notify} />,
    chat: <ChatView config={config} conversation={chat.chats.find((c) => c.id === params.c) ?? null}
      live={(params.c && chat.live[params.c]) || null}
      onSend={(question) => chat.send(params.c ?? null, question)}
      onReplace={replaceChatEntry} onOpen={open} notify={notify} />,
    brouillons: <DraftsView config={config} files={files} current={params.d ?? null}
      onSelect={(d) => navigate(href("brouillons", null, { d }))} onOpen={open} notify={notify} />,
  }[space];

  const layout = [sidebar ? "with-sidebar" : "", path ? "with-right" : ""].join(" ");

  return (
    <div className={`app ${layout}`}>
      <SpaceBar current={space} onSelect={selectSpace} shortcuts={config.shortcuts}
        badges={{ inbox: inboxCount, imputations: imputationMissing, chat: chat.chats.filter((c) => c.unread).length }}
        busy={{ chat: Object.keys(chat.live).length > 0 }} />
      {sidebar}

      <main>
        {path
          ? <EditorPane path={path} files={files} config={config} revision={revision} onOpen={open} notify={notify}
            actions={editorActions} />
          : home}
      </main>

      {path && <RightPanel path={path} revision={revision} semantic={config.semantic_enabled} onOpen={open}
        onLink={(name) => editorActions.current.appendLink?.(name)} />}

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
