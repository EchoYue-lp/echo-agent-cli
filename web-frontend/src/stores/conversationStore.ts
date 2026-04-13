import { create } from 'zustand';
import { useChatStore } from './chatStore';
import { sessionApi, conversationApi } from '../api/endpoints';
import type { ChatMessage } from '../types/api';

// ── Types ──

export interface ConversationMeta {
  id: string;
  title: string;
  lastMessage: string;
  messageCount: number;
  createdAt: number;
  updatedAt: number;
}

interface ConversationState {
  /** All conversations sorted by updatedAt desc */
  conversations: ConversationMeta[];
  /** Currently active conversation ID */
  activeId: string | null;
  /** Whether we're loading a conversation */
  isLoading: boolean;

  /** Initialize: load from backend */
  init: () => void;
  /** Save current chat messages as a conversation (upsert) */
  saveCurrent: (messages: ChatMessage[]) => void;
  /** Load a conversation and display it */
  loadConversation: (id: string) => Promise<void>;
  /** Delete a conversation */
  deleteConversation: (id: string) => void;
  /** Rename a conversation */
  renameConversation: (id: string, title: string) => void;
  /** Start a brand new chat */
  startNew: () => Promise<void>;
  /** Get all conversations grouped by date */
  getGroupedConversations: () => ConversationGroup[];
}

// ── Date grouping ──

export interface ConversationGroup {
  label: string;
  conversations: ConversationMeta[];
}

function groupByDate(conversations: ConversationMeta[]): ConversationGroup[] {
  const now = new Date();
  const today = new Date(now.getFullYear(), now.getMonth(), now.getDate()).getTime();
  const yesterday = today - 86400000;
  const lastWeek = today - 7 * 86400000;
  const lastMonth = today - 30 * 86400000;

  const groups: { key: string; label: string; items: ConversationMeta[] }[] = [
    { key: 'today', label: 'Today', items: [] },
    { key: 'yesterday', label: 'Yesterday', items: [] },
    { key: 'week', label: 'Previous 7 Days', items: [] },
    { key: 'month', label: 'Previous 30 Days', items: [] },
    { key: 'older', label: 'Older', items: [] },
  ];

  for (const c of conversations) {
    if (c.updatedAt >= today) {
      groups[0].items.push(c);
    } else if (c.updatedAt >= yesterday) {
      groups[1].items.push(c);
    } else if (c.updatedAt >= lastWeek) {
      groups[2].items.push(c);
    } else if (c.updatedAt >= lastMonth) {
      groups[3].items.push(c);
    } else {
      groups[4].items.push(c);
    }
  }

  return groups.filter((g) => g.items.length > 0).map((g) => ({ label: g.label, conversations: g.items }));
}

// ── Helpers ──

function generateId(): string {
  return `conv-${Date.now()}-${Math.random().toString(36).slice(2, 8)}`;
}

function chatMessagesToSaved(messages: ChatMessage[]) {
  return messages.map((m) => ({
    role: m.role,
    content: m.content,
    tool_calls: m.toolCalls?.map((tc) => ({
      id: `${Date.now()}-${Math.random().toString(36).slice(2, 6)}`,
      name: tc.name,
      arguments: JSON.stringify(tc.args),
    })),
  }));
}

// ── Store ──

export const useConversationStore = create<ConversationState>((set, get) => ({
  conversations: [],
  activeId: null,
  isLoading: false,

  init: async () => {
    try {
      const items = await conversationApi.list();
      const metas: ConversationMeta[] = items.map((item) => ({
        id: item.id,
        title: item.title,
        lastMessage: '',
        messageCount: item.message_count,
        createdAt: new Date(item.created_at).getTime(),
        updatedAt: new Date(item.updated_at).getTime(),
      })).sort((a, b) => b.updatedAt - a.updatedAt);
      set({ conversations: metas });
    } catch (e) {
      console.error('Failed to load conversations from backend:', e);
      // Fallback: keep empty list
      set({ conversations: [] });
    }
  },

  saveCurrent: async (messages: ChatMessage[]) => {
    if (messages.length === 0) return;

    const activeId = get().activeId;
    const firstUserMsg = messages.find((m) => m.role === 'user');
    const title = firstUserMsg?.content?.slice(0, 50) || 'New Chat';
    const savedMessages = chatMessagesToSaved(messages);

    try {
      if (activeId) {
        // Update existing
        await conversationApi.update(activeId, {
          title,
          messages: savedMessages,
        });
      } else {
        // Create new
        const newId = generateId();
        await conversationApi.save({
          id: newId,
          title,
          messages: savedMessages,
        });
        set({ activeId: newId });
      }
    } catch (e) {
      console.error('Failed to save conversation to backend:', e);
    }

    // Refresh list
    try {
      const items = await conversationApi.list();
      const metas: ConversationMeta[] = items.map((item) => ({
        id: item.id,
        title: item.title,
        lastMessage: '',
        messageCount: item.message_count,
        createdAt: new Date(item.created_at).getTime(),
        updatedAt: new Date(item.updated_at).getTime(),
      })).sort((a, b) => b.updatedAt - a.updatedAt);
      set({ conversations: metas });
    } catch {
      // ignore refresh error
    }
  },

  loadConversation: async (id: string) => {
    set({ isLoading: true });

    try {
      const record = await conversationApi.get(id);

      // Restore agent context on the backend so conversation can continue
      try {
        await conversationApi.restore(id);
      } catch (e) {
        console.error('Failed to restore agent context:', e);
      }

      // Convert saved messages to chat messages for frontend display
      const chatMessages: ChatMessage[] = record.messages
        .filter((m) => m.role === 'user' || m.role === 'assistant')
        .map((m) => ({
          id: `loaded-${Date.now()}-${Math.random().toString(36).slice(2, 8)}`,
          role: m.role as 'user' | 'assistant',
          content: m.content || '',
          isStreaming: false,
          timestamp: Date.now(),
        }));

      const chatStore = useChatStore.getState();
      chatStore.replaceMessages(chatMessages);
      chatStore.setHistoryView(false);  // Agent has context, can continue chatting
      chatStore.setCurrentSnapshot(id);

      set({ activeId: id, isLoading: false });
    } catch (e) {
      console.error('Failed to load conversation:', e);
      set({ isLoading: false });
    }
  },

  deleteConversation: async (id: string) => {
    try {
      await conversationApi.delete(id);
    } catch (e) {
      console.error('Failed to delete conversation:', e);
    }

    // Refresh list
    set((s) => {
      const conversations = s.conversations.filter((c) => c.id !== id);
      return {
        conversations,
        activeId: s.activeId === id ? null : s.activeId,
      };
    });
  },

  renameConversation: async (id: string, title: string) => {
    try {
      await conversationApi.update(id, { title });
    } catch (e) {
      console.error('Failed to rename conversation:', e);
    }

    // Update local state
    set((s) => ({
      conversations: s.conversations.map((c) =>
        c.id === id ? { ...c, title } : c
      ),
    }));
  },

  startNew: async () => {
    // Save current conversation first
    const currentMessages = useChatStore.getState().messages;
    if (currentMessages.length > 0) {
      await get().saveCurrent(currentMessages);
    }

    // Reset backend
    try {
      await sessionApi.reset();
    } catch {
      // ignore
    }

    useChatStore.getState().clearMessages();
    set({ activeId: null });
  },

  getGroupedConversations: () => {
    return groupByDate(get().conversations);
  },
}));
