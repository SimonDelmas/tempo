import { useCallback, useEffect, useRef, useState } from "react";
import { api, ChatProposal, ChatStep } from "./api";

/** Conversations avec l'agent, conservées dans le navigateur (localStorage). */

export type ChatEntry = {
  role: "user" | "assistant";
  content: string;
  steps?: ChatStep[];
  links?: Record<string, string>;
  proposals?: ProposalState[];
  error?: boolean;
};

/** proposition de l'agent et suite donnée par l'utilisateur */
export type ProposalState = {
  proposal: ChatProposal;
  state: "pending" | "applied" | "ignored";
  path?: string;
  message?: string;
};

export type Conversation = {
  id: string;
  title: string;
  updated: string;
  messages: ChatEntry[];
  /** réponse arrivée pendant que l'utilisateur regardait ailleurs */
  unread?: boolean;
};

/** réponse en cours de diffusion */
export type LiveReply = { text: string; steps: ChatStep[] };

const KEY = "tempo.chats";
const MAX_CONVERSATIONS = 50;

export function loadChats(): Conversation[] {
  try {
    const data = JSON.parse(localStorage.getItem(KEY) ?? "[]");
    return Array.isArray(data) ? data : [];
  } catch {
    return [];
  }
}

export function saveChats(chats: Conversation[]) {
  try {
    localStorage.setItem(KEY, JSON.stringify(chats.slice(0, MAX_CONVERSATIONS)));
  } catch {
    /* stockage plein ou indisponible : les conversations restent en mémoire */
  }
}

export const newConversation = (question: string): Conversation => ({
  id: Date.now().toString(36) + Math.random().toString(36).slice(2, 6),
  title: question.length > 60 ? question.slice(0, 57) + "…" : question,
  updated: new Date().toISOString(),
  messages: [],
});

type ChatEvents = {
  /** conversation créée par le premier message */
  onCreated: (id: string) => void;
  /** réponse terminée ; `seen` indique si la conversation était affichée à ce moment */
  onFinished: (conversation: Conversation, seen: boolean, error: boolean) => void;
};

/**
 * Conversations et réponses en cours, tenues au niveau de l'application : une réponse
 * continue de se diffuser quand on quitte l'espace Chat, et on la retrouve en revenant.
 */
export function useChats({ onCreated, onFinished }: ChatEvents) {
  const [chats, setChats] = useState<Conversation[]>(loadChats);
  const [live, setLive] = useState<Record<string, LiveReply>>({});
  /** conversation actuellement affichée (null : aucune) */
  const viewing = useRef<string | null>(null);
  const latest = useRef(chats);
  latest.current = chats;
  const events = useRef({ onCreated, onFinished });
  events.current = { onCreated, onFinished };

  useEffect(() => saveChats(chats), [chats]);

  const append = useCallback((id: string | null, entry: ChatEntry, extra: Partial<Conversation> = {}) => {
    const created = id ? null : newConversation(entry.content);
    const target = id ?? created!.id;
    const update = (prev: Conversation[]) => (created ? [created, ...prev] : prev).map((c) => c.id === target
      ? { ...c, ...extra, updated: new Date().toISOString(), messages: [...c.messages, entry] }
      : c);
    latest.current = update(latest.current);
    setChats(update);
    if (created) events.current.onCreated(target);
    return target;
  }, []);

  const replace = useCallback((id: string, index: number, entry: ChatEntry) => {
    setChats((prev) => prev.map((c) => c.id === id
      ? { ...c, messages: c.messages.map((m, i) => (i === index ? entry : m)) }
      : c));
  }, []);

  const remove = useCallback((id: string) => setChats((prev) => prev.filter((c) => c.id !== id)), []);

  /** la conversation affichée change : elle n'est plus « non lue » */
  const view = useCallback((id: string | null) => {
    viewing.current = id;
    if (id && latest.current.some((c) => c.id === id && c.unread)) {
      setChats((prev) => prev.map((c) => (c.id === id ? { ...c, unread: false } : c)));
    }
  }, []);

  const send = useCallback(async (id: string | null, question: string) => {
    const previous = id ? latest.current.find((c) => c.id === id)?.messages ?? [] : [];
    const history = [...previous, { role: "user" as const, content: question }]
      .filter((m) => !m.error)
      .map(({ role, content }) => ({ role, content }));
    const target = append(id, { role: "user", content: question });
    const update = (change: (l: LiveReply) => LiveReply) =>
      setLive((all) => (all[target] ? { ...all, [target]: change(all[target]) } : all));
    setLive((all) => ({ ...all, [target]: { text: "", steps: [] } }));
    let entry: ChatEntry;
    try {
      const reply = await api.chatStream(history, (event) => {
        if (event.type === "delta") update((l) => ({ ...l, text: l.text + event.text }));
        if (event.type === "step") {
          const { type: _, ...step } = event;
          update((l) => ({ ...l, steps: [...l.steps, step] }));
        }
      });
      entry = {
        role: "assistant", content: reply.answer, steps: reply.steps, links: reply.links,
        proposals: reply.proposals.map((proposal) => ({ proposal, state: "pending" })),
      };
    } catch (e) {
      entry = { role: "assistant", content: (e as Error).message, error: true };
    }
    // conversation supprimée entre-temps : on abandonne la réponse
    const exists = latest.current.some((c) => c.id === target);
    const seen = viewing.current === target;
    if (exists) append(target, entry, { unread: !seen });
    setLive((all) => {
      const { [target]: _, ...rest } = all;
      return rest;
    });
    const conversation = latest.current.find((c) => c.id === target);
    if (conversation) events.current.onFinished(conversation, seen, !!entry.error);
  }, [append]);

  return { chats, live, send, replace, remove, view };
}
