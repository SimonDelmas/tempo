import { ChatProposal, ChatStep } from "./api";

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

export type Conversation = { id: string; title: string; updated: string; messages: ChatEntry[] };

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
