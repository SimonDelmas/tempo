import { useCallback, useEffect, useRef, useState } from "react";
import { Config } from "../api";
import { ChatEntry, Conversation, LiveReply } from "../chats";
import ChatView from "./ChatView";

/** État du panneau de chat, conservé dans le stockage local. */
export type ChatPanelState = {
  /** panneau déplié (sinon réduit à une barre étroite) */
  open: boolean;
  /** largeur du panneau déplié, en pixels */
  width: number;
  /** conversation affichée ; `null` : nouvelle conversation */
  current: string | null;
};

const PANEL_KEY = "tempo.chatPanel";
const MIN_WIDTH = 300;
const DEFAULT_WIDTH = 420;
/** le panneau laisse au moins cette largeur au reste de l'application */
const MIN_REST = 480;

const clampWidth = (width: number) =>
  Math.round(Math.max(MIN_WIDTH, Math.min(width, window.innerWidth - MIN_REST)));

function loadPanel(): ChatPanelState {
  const fallback = { open: true, width: DEFAULT_WIDTH, current: null };
  try {
    return { ...fallback, ...JSON.parse(localStorage.getItem(PANEL_KEY) ?? "{}") };
  } catch {
    return fallback;
  }
}

/** état du panneau de chat, enregistré à chaque changement */
export function useChatPanel() {
  const [panel, setPanel] = useState(loadPanel);
  useEffect(() => {
    try {
      localStorage.setItem(PANEL_KEY, JSON.stringify(panel));
    } catch {
      /* stockage indisponible */
    }
  }, [panel]);
  const update = useCallback((changes: Partial<ChatPanelState>) => setPanel((p) => ({ ...p, ...changes })), []);
  return [panel, update] as const;
}

type Props = {
  config: Config;
  panel: ChatPanelState;
  onPanel: (changes: Partial<ChatPanelState>) => void;
  chats: Conversation[];
  /** réponses en cours, par conversation */
  live: Record<string, LiveReply>;
  onSend: (question: string) => void;
  onReplace: (id: string, index: number, entry: ChatEntry) => void;
  onDelete: (id: string) => void;
  onOpen: (path: string) => void;
  notify: (message: string, error?: boolean) => void;
};

const ICON_CHAT = "M4 5h16v11H9l-5 4z M8 9h8 M8 12h5";
const ICON_LIST = "M8 6h12 M8 12h12 M8 18h12 M4 6h.01 M4 12h.01 M4 18h.01";
const ICON_NEW = "M12 5v14 M5 12h14";
const ICON_COLLAPSE = "M13 5l7 7-7 7 M5 5l7 7-7 7";

const Icon = ({ d }: { d: string }) => <svg viewBox="0 0 24 24" aria-hidden="true"><path d={d} /></svg>;

/** Panneau de chat à droite de l'application : toujours accessible, réductible et redimensionnable. */
export default function ChatPanel({ config, panel, onPanel, chats, live, onSend, onReplace, onDelete, onOpen, notify }: Props) {
  const [history, setHistory] = useState(false);
  const [resizing, setResizing] = useState(false);
  const root = useRef<HTMLElement>(null);
  const conversation = chats.find((c) => c.id === panel.current) ?? null;
  const unread = chats.filter((c) => c.unread).length;
  const busy = Object.keys(live).length > 0;

  // la fenêtre rétrécit : le panneau ne doit pas écraser le reste de l'application
  useEffect(() => {
    const onResize = () => {
      const width = clampWidth(panel.width);
      if (width !== panel.width) onPanel({ width });
    };
    window.addEventListener("resize", onResize);
    return () => window.removeEventListener("resize", onResize);
  }, [panel.width, onPanel]);

  const startResize = (e: React.PointerEvent) => {
    e.preventDefault();
    const startX = e.clientX;
    const startWidth = root.current?.offsetWidth ?? panel.width;
    const onMove = (ev: PointerEvent) => onPanel({ width: clampWidth(startWidth + startX - ev.clientX) });
    const onUp = () => {
      setResizing(false);
      window.removeEventListener("pointermove", onMove);
      window.removeEventListener("pointerup", onUp);
    };
    setResizing(true);
    window.addEventListener("pointermove", onMove);
    window.addEventListener("pointerup", onUp);
  };

  if (!panel.open) {
    return (
      <aside className="chat-panel collapsed">
        <button className="chat-panel-tab" onClick={() => onPanel({ open: true })} title="Ouvrir le chat (Alt+C)">
          <Icon d={ICON_CHAT} />
          <span>Chat</span>
          {!!unread && <span className="space-badge">{unread}</span>}
          {busy && !unread && <span className="space-busy" title="Réponse de l'agent en cours" />}
        </button>
      </aside>
    );
  }

  const select = (current: string | null) => {
    onPanel({ current });
    setHistory(false);
  };
  const sorted = [...chats].sort((a, b) => b.updated.localeCompare(a.updated));

  return (
    <aside ref={root} className={`chat-panel ${resizing ? "resizing" : ""}`} style={{ width: panel.width }}>
      <div className="chat-panel-resize" onPointerDown={startResize}
        onDoubleClick={() => onPanel({ width: clampWidth(DEFAULT_WIDTH) })}
        title="Glisser pour redimensionner (double-clic : largeur par défaut)" />
      <header className="chat-panel-header">
        <button className={`icon-button ${history ? "active" : ""}`} onClick={() => setHistory(!history)}
          title="Conversations">
          <Icon d={ICON_LIST} />
          {!!unread && <span className="chat-status unread" />}
        </button>
        <span className="chat-panel-title" title={conversation?.title}>
          {history ? "Conversations" : conversation?.title ?? "Nouvelle conversation"}
        </span>
        <button className="icon-button" onClick={() => select(null)} title="Nouvelle conversation">
          <Icon d={ICON_NEW} />
        </button>
        <button className="icon-button" onClick={() => onPanel({ open: false })} title="Réduire le chat (Alt+C)">
          <Icon d={ICON_COLLAPSE} />
        </button>
      </header>

      {history ? (
        <div className="chat-history">
          {sorted.length === 0 && <p className="muted small">Aucune conversation</p>}
          {sorted.map((c) => (
            <button key={c.id} className={`nav ${c.id === panel.current ? "active" : ""}`} onClick={() => select(c.id)}
              title={c.title}>
              <span className="nav-label">
                {live[c.id] && <span className="chat-status busy" title="Réponse en cours" />}
                {c.unread && !live[c.id] && <span className="chat-status unread" title="Nouvelle réponse" />}
                {c.title}
              </span>
              <span className="nav-delete" title="Supprimer" onClick={(e) => {
                e.stopPropagation();
                if (window.confirm(`Supprimer la conversation « ${c.title} » ?`)) onDelete(c.id);
              }}>×</span>
            </button>
          ))}
          <p className="muted small sidebar-note">Conversations conservées dans ce navigateur.</p>
        </div>
      ) : (
        <ChatView config={config} conversation={conversation}
          live={(panel.current && live[panel.current]) || null}
          onSend={onSend} onReplace={onReplace} onOpen={onOpen} notify={notify} />
      )}
    </aside>
  );
}
