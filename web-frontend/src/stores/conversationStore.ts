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
  /** Workspace this conversation belongs to */
  workspaceId?: string;
}

interface ConversationState {
  /** All conversations sorted by updatedAt desc */
  conversations: ConversationMeta[];
  /** Currently active conversation ID */
  activeId: string | null;
  /** Whether we're loading a conversation */
  isLoading: boolean;

  /** Initialize: load from backend */
  init: () => Promise<void>;
  /** Save current chat messages as a conversation (upsert) */
  saveCurrent: (messages: ChatMessage[]) => Promise<void>;
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

  return groups
    .filter((g) => g.items.length > 0)
    .map((g) => ({ label: g.label, conversations: g.items }));
}

// ── Helpers ──

function generateId(): string {
  return `conv-${Date.now()}-${Math.random().toString(36).slice(2, 8)}`;
}

function chatMessagesToSaved(messages: ChatMessage[]) {
  const saved: {
    role: string;
    content: string;
    tool_calls?: { id: string; name: string; arguments: string }[];
    thinking_segments?: string[];
    execution_steps?: { type: string; index: number }[];
    execution_rounds?: {
      thinking?: { content: string };
      tools: { name: string; args: unknown; result: string; success: boolean }[];
    }[];
    tool_result?: string;
  }[] = [];

  for (const m of messages) {
    // Save the main message
    const entry: (typeof saved)[0] = {
      role: m.role,
      content: m.content,
    };

    // Save thinking segments
    if (m.thinkingSegments && m.thinkingSegments.length > 0) {
      entry.thinking_segments = m.thinkingSegments.map((s) => s.content);
    }

    // Save execution steps (chronological order of thinking/tool interleaving)
    if (m.executionSteps && m.executionSteps.length > 0) {
      entry.execution_steps = m.executionSteps.map((s) => ({
        type: s.type,
        index: s.index,
      }));
    }

    // Save execution rounds (round-based model with thinking + parallel tools)
    if (m.executionRounds && m.executionRounds.length > 0) {
      entry.execution_rounds = m.executionRounds.map((r) => ({
        thinking: r.thinking ? { content: r.thinking.content } : undefined,
        tools: r.tools.map((t) => ({
          name: t.name,
          args: t.args,
          result: t.result,
          success: t.success,
        })),
      }));
    }

    // Save tool calls with stable IDs and results
    if (m.toolCalls && m.toolCalls.length > 0) {
      entry.tool_calls = m.toolCalls.map((tc, i) => ({
        id: `tc-${m.id}-${i}`,
        name: tc.name,
        arguments: typeof tc.args === 'string' ? tc.args : JSON.stringify(tc.args || {}),
      }));
    }

    saved.push(entry);
  }

  return saved;
}

// ── Store ──

export const useConversationStore = create<ConversationState>((set, get) => ({
  conversations: [],
  activeId: null,
  isLoading: false,

  init: async () => {
    try {
      const items = await conversationApi.list();
      if (import.meta.env.DEV) console.debug('[conversationStore] init: loaded', items.length, 'conversations');
      const metas: ConversationMeta[] = items
        .map((item) => ({
          id: item.conversation_id,
          title: item.title ?? '',
          lastMessage: '',
          messageCount: item.message_count,
          createdAt: new Date(item.created_at).getTime(),
          updatedAt: new Date(item.updated_at).getTime(),
        }))
        .sort((a, b) => b.updatedAt - a.updatedAt);
      set({ conversations: metas });
    } catch (e) {
      console.error('[conversationStore] init FAILED:', e);
      set({ conversations: [] });
    }
  },

  saveCurrent: async (messages: ChatMessage[]) => {
    if (messages.length === 0) return;

    const activeId = get().activeId;
    const firstUserMsg = messages.find((m) => m.role === 'user');
    const title = firstUserMsg?.content?.slice(0, 50) || 'New Chat';
    const savedMessages = chatMessagesToSaved(messages);

    if (import.meta.env.DEV) console.debug('[saveCurrent] activeId:', activeId, 'msgCount:', messages.length);

    try {
      if (activeId) {
        // Update existing
        const res = await conversationApi.update(activeId, {
          title,
          messages: savedMessages,
        });
        if (import.meta.env.DEV) console.debug('[saveCurrent] update result:', res);
      } else {
        // Create new
        const newId = generateId();
        const res = await conversationApi.save({
          id: newId,
          title,
          messages: savedMessages,
        });
        if (import.meta.env.DEV) console.debug('[saveCurrent] save result:', res, 'newId:', newId);
        set({ activeId: newId });
      }
    } catch (e) {
      console.error('[saveCurrent] FAILED:', e);
      return; // Don't attempt list refresh if save failed
    }

    // Refresh list (best-effort, must not throw)
    try {
      const items = await conversationApi.list();
      if (import.meta.env.DEV) console.debug('[saveCurrent] refreshed list:', items.length, 'conversations');
      const metas: ConversationMeta[] = items
        .map((item) => ({
          id: item.conversation_id,
          title: item.title ?? '',
          lastMessage: '',
          messageCount: item.message_count,
          createdAt: new Date(item.created_at).getTime(),
          updatedAt: new Date(item.updated_at).getTime(),
        }))
        .sort((a, b) => b.updatedAt - a.updatedAt);
      set({ conversations: metas });
    } catch (e) {
      console.error('[saveCurrent] refresh FAILED:', e);
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

      // Convert saved messages to chat messages for frontend display.
      // Include tool messages to show the full thinking chain (tool calls + results).
      const chatMessages: ChatMessage[] = record.messages.map((m, idx) => {
        const base: ChatMessage = {
          id: `loaded-${Date.now()}-${idx}`,
          role: (m.role === 'tool' ? 'assistant' : m.role) as 'user' | 'assistant',
          content: m.content || '',
          isStreaming: false,
          timestamp: Date.now(),
        };

        // Restore thinking segments
        if (m.thinking_segments && m.thinking_segments.length > 0) {
          base.thinkingSegments = m.thinking_segments.map((s) => ({ content: s }));
        }

        // Restore execution steps (chronological order of thinking/tool interleaving)
        if (m.execution_steps && m.execution_steps.length > 0) {
          base.executionSteps = m.execution_steps.map((s) => ({
            type: s.type as 'thinking' | 'tool',
            index: s.index,
          }));
        }

        // Restore execution rounds (round-based model).
        // Typed via the SavedMessage shape instead of `any` (P1-40).
        if (m.execution_rounds && m.execution_rounds.length > 0) {
          type ExecutionRoundTool = { name: string; args: unknown; result: string; success: boolean };
          type ExecutionRound = {
            thinking?: { content: string };
            tools: ExecutionRoundTool[];
          };
          base.executionRounds = (m.execution_rounds as ExecutionRound[]).map((r) => ({
            thinking: r.thinking ? { content: r.thinking.content } : undefined,
            tools: (r.tools || []).map((t): ExecutionRoundTool => ({
              name: t.name,
              args: t.args || {},
              result: t.result || '',
              success: t.success ?? true,
            })),
          }));
        }

        // Restore tool calls on assistant messages
        if (m.role === 'assistant' && m.tool_calls && m.tool_calls.length > 0) {
          base.toolCalls = m.tool_calls.map((tc) => ({
            name: tc.name,
            args: (() => {
              try {
                return JSON.parse(tc.arguments);
              } catch {
                return tc.arguments;
              }
            })(),
            result: '',
            success: true,
          }));
        }

        // For tool result messages, show as assistant with tool result content
        if (m.role === 'tool') {
          base.content = m.tool_result || m.content || '';
          base.toolCalls = [
            {
              name: 'tool',
              args: {},
              result: m.tool_result || m.content || '',
              success: true,
            },
          ];
        }

        return base;
      });

      const chatStore = useChatStore.getState();
      chatStore.replaceMessages(chatMessages);
      chatStore.setHistoryView(false); // Agent has context, can continue chatting

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
      conversations: s.conversations.map((c) => (c.id === id ? { ...c, title } : c)),
    }));
  },

  startNew: async () => {
    // Save current conversation first — must not block clearing even if save fails
    const currentMessages = useChatStore.getState().messages;
    if (currentMessages.length > 0) {
      try {
        await get().saveCurrent(currentMessages);
      } catch (e) {
        console.error('Failed to save conversation before new chat:', e);
      }
    }

    // Reset backend session
    try {
      await sessionApi.reset();
    } catch {
      // ignore
    }

    // Always clear — this is the user's expected outcome
    useChatStore.getState().clearMessages();
    set({ activeId: null });
  },

  getGroupedConversations: () => {
    return groupByDate(get().conversations);
  },
}));
