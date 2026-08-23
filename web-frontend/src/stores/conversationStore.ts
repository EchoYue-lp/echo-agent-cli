import { create } from 'zustand';
import { useChatStore } from './chatStore';
import { sessionApi, conversationApi, toolExecutionApi } from '../api/endpoints';
import { useToastStore } from './toastStore';
import { useToolExecutionStore } from './toolExecutionStore';
import { useSubagentRunStore } from './subagentRunStore';
import { useTaskRuntimeStore } from './taskRuntimeStore';
import { GLOBAL_WORKSPACE_ID } from '../lib/viewAddress';
import type { ChatMessage, ExecutionStep, SavedMessage } from '../types/api';

let loadGeneration = 0;
let loadingConversationId: string | null = null;

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
  /** Workspace owning this list and active conversation projection. */
  workspaceId: string;
  /** All conversations sorted by updatedAt desc */
  conversations: ConversationMeta[];
  /** Currently active conversation ID */
  activeId: string | null;
  /** Increments for every explicit new-chat action, including null -> null transitions. */
  newConversationEpoch: number;
  /** Whether we're loading a conversation */
  isLoading: boolean;

  /** Initialize: load from backend */
  init: (workspaceId: string) => Promise<void>;
  /** Immediately detach the visible conversation projection during a workspace transition. */
  detachForWorkspace: (workspaceId: string) => void;
  /** Save current chat messages as a conversation (upsert) */
  saveCurrent: (messages: ChatMessage[]) => Promise<void>;
  /** Load a conversation and display it */
  loadConversation: (id: string) => Promise<void>;
  /** Branch the canonical transcript immediately before one user turn. */
  branchCurrent: (userTurnIndex: number) => Promise<{ id: string; targetContent: string }>;
  /** Delete a conversation */
  deleteConversation: (id: string) => void;
  /** Rename a conversation */
  renameConversation: (id: string, title: string) => void;
  /** Start a brand new chat */
  startNew: () => Promise<void>;
  /** Clear current chat immediately without saving it */
  clearCurrent: () => Promise<void>;
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

function isTextAttachment(name: string, mimeType: string): boolean {
  if (
    mimeType.startsWith('text/') ||
    ['application/json', 'application/xml', 'application/yaml'].includes(mimeType)
  ) {
    return true;
  }
  const extension = name.split('.').pop()?.toLowerCase();
  return Boolean(
    extension &&
    [
      'txt',
      'log',
      'md',
      'markdown',
      'json',
      'xml',
      'yaml',
      'yml',
      'csv',
      'tsv',
      'rs',
      'py',
      'js',
      'ts',
      'tsx',
      'jsx',
      'go',
      'java',
      'c',
      'cpp',
      'h',
      'sh',
      'toml',
      'ini',
      'sql',
    ].includes(extension)
  );
}

export function chatMessagesToSaved(messages: ChatMessage[]): SavedMessage[] {
  const saved: SavedMessage[] = [];

  for (const m of messages) {
    // Save the main message
    const entry: SavedMessage = {
      message_id: m.id,
      role: m.role,
      content: m.content,
    };

    // Save thinking segments
    if (m.thinkingSegments && m.thinkingSegments.length > 0) {
      entry.thinking_segments = m.thinkingSegments.map((s) => s.content);
    }

    // Save execution steps (chronological order of thinking/tool interleaving)
    if (m.executionSteps && m.executionSteps.length > 0) {
      entry.execution_steps = m.executionSteps.map((s) =>
        s.type === 'thinking'
          ? { type: s.type, index: s.index }
          : { type: s.type, call_id: s.callId }
      );
    }

    // Save execution rounds (round-based model with thinking + parallel tools)
    if (m.executionRounds && m.executionRounds.length > 0) {
      entry.execution_rounds = m.executionRounds.map((r) => ({
        thinking: r.thinking ? { content: r.thinking.content } : undefined,
        tool_call_ids: [...r.toolCallIds],
      }));
    }

    // Save user-uploaded attachments (data URLs) so the message renders on reload
    if (m.attachments && m.attachments.length > 0) {
      entry.attachments = m.attachments.map((a) => ({
        name: a.name,
        mime_type: a.mime_type,
        // Large/pasted text is durably represented by the backend's canonical
        // message projection. Do not duplicate its body as a data URL in the
        // UI projection.
        url:
          isTextAttachment(a.name, a.mime_type) &&
          (a.source === 'paste' || a.url.length > 64 * 1024)
            ? ''
            : a.url,
        size: a.size,
        source: a.source,
      }));
    }

    saved.push(entry);
  }

  return saved;
}

export function restoredMessageId(
  conversationId: string,
  index: number,
  message: SavedMessage
): string {
  return message.message_id ?? `loaded-${conversationId}-${index}`;
}

// ── Store ──

export const useConversationStore = create<ConversationState>((set, get) => ({
  workspaceId: GLOBAL_WORKSPACE_ID,
  conversations: [],
  activeId: null,
  newConversationEpoch: 0,
  isLoading: false,

  init: async (workspaceId: string) => {
    const generation = loadGeneration + 1;
    loadGeneration = generation;
    set({ workspaceId });
    try {
      const items = await conversationApi.list(workspaceId);
      if (generation !== loadGeneration || get().workspaceId !== workspaceId) return;
      if (import.meta.env.DEV)
        console.debug('[conversationStore] init: loaded', items.length, 'conversations');
      const metas: ConversationMeta[] = items
        .map((item) => ({
          id: item.conversation_id,
          title: item.title ?? '',
          lastMessage: '',
          messageCount: item.message_count,
          createdAt: new Date(item.created_at).getTime(),
          updatedAt: new Date(item.updated_at).getTime(),
          workspaceId,
        }))
        .sort((a, b) => b.updatedAt - a.updatedAt);
      set({ conversations: metas });
    } catch (e) {
      console.error('[conversationStore] init FAILED:', e);
      if (generation === loadGeneration && get().workspaceId === workspaceId) {
        set({ conversations: [] });
      }
    }
  },

  detachForWorkspace: (workspaceId: string) => {
    loadGeneration += 1;
    loadingConversationId = null;
    set((state) => ({
      workspaceId,
      conversations: [],
      activeId: null,
      newConversationEpoch: state.newConversationEpoch + 1,
      isLoading: false,
    }));
  },

  saveCurrent: async (messages: ChatMessage[]) => {
    const activeId = get().activeId;
    const workspaceId = get().workspaceId;
    if (messages.length === 0) return;

    const firstUserMsg = messages.find((m) => m.role === 'user');
    const title = firstUserMsg?.content?.slice(0, 50) || 'New Chat';
    if (import.meta.env.DEV)
      console.debug('[saveCurrent] activeId:', activeId, 'msgCount:', messages.length);

    try {
      if (activeId) {
        // Update existing
        const res = await conversationApi.update(workspaceId, activeId, {
          title,
        });
        if (import.meta.env.DEV) console.debug('[saveCurrent] update result:', res);
      } else {
        // Create new
        const newId = generateId();
        const res = await conversationApi.save(workspaceId, {
          id: newId,
          title,
          // The Agent backend is the sole transcript writer. Creating the
          // conversation before dispatch only establishes its stable identity.
          messages: [],
        });
        if (import.meta.env.DEV) console.debug('[saveCurrent] save result:', res, 'newId:', newId);
        if (get().workspaceId !== workspaceId) return;
        set({ activeId: newId });
      }
    } catch (e) {
      console.error('[saveCurrent] FAILED:', e);
      return; // Don't attempt list refresh if save failed
    }

    // Refresh list (best-effort, must not throw)
    try {
      const items = await conversationApi.list(workspaceId);
      if (get().workspaceId !== workspaceId) return;
      if (import.meta.env.DEV)
        console.debug('[saveCurrent] refreshed list:', items.length, 'conversations');
      const metas: ConversationMeta[] = items
        .map((item) => ({
          id: item.conversation_id,
          title: item.title ?? '',
          lastMessage: '',
          messageCount: item.message_count,
          createdAt: new Date(item.created_at).getTime(),
          updatedAt: new Date(item.updated_at).getTime(),
          workspaceId,
        }))
        .sort((a, b) => b.updatedAt - a.updatedAt);
      set({ conversations: metas });
    } catch (e) {
      console.error('[saveCurrent] refresh FAILED:', e);
    }
  },

  loadConversation: async (id: string) => {
    const workspaceId = get().workspaceId;
    const generation = loadGeneration + 1;
    loadGeneration = generation;
    loadingConversationId = id;
    useChatStore.getState().clearMessages();
    useTaskRuntimeStore.getState().reset();
    useToolExecutionStore.getState().clear();
    useSubagentRunStore.getState().clear();
    // Publish the selected id only after transcript/tool hydration completes.
    // This makes the chat hook rebind the live turn after `replaceMessages`
    // instead of creating a placeholder that the late transcript load erases.
    set({ isLoading: true, activeId: null });

    try {
      const [record, tools] = await Promise.all([
        conversationApi.get(workspaceId, id),
        toolExecutionApi.list(workspaceId, id),
      ]);
      if (generation !== loadGeneration || get().workspaceId !== workspaceId) return;

      // Restore agent context on the backend so conversation can continue.
      let restoreReady = true;
      try {
        await conversationApi.restore(workspaceId, id);
      } catch (e) {
        restoreReady = false;
        console.error('Failed to restore agent context:', e);
        useToastStore.getState().addToast('error', '会话上下文恢复失败，已切换为只读历史视图');
        useChatStore.getState().setHistoryView(true);
      }
      if (generation !== loadGeneration || get().workspaceId !== workspaceId) return;
      useToolExecutionStore.getState().hydrateConversation(workspaceId, id, tools);

      // Convert user/assistant messages for display. Tool-role payloads stay in
      // the agent context; the GUI renders tools from lightweight summaries.
      const chatMessages: ChatMessage[] = record.messages
        .filter((message) => message.role === 'user' || message.role === 'assistant')
        .map((m, idx) => {
          const base: ChatMessage = {
            // P2-7: 此前用 `loaded-${Date.now()}-${idx}`, 每次加载产生不同 id,
            // React key 变化导致消息列表全量重渲染。改用会话 id + 索引, 同一会话
            // 每次加载产生确定性 id, key 稳定。timestamp 仍用 now (无服务端时间)。
            id: restoredMessageId(id, idx, m),
            role: m.role as 'user' | 'assistant',
            content: m.content || '',
            isStreaming: false,
            timestamp: Date.now(),
          };

          // Restore thinking segments
          if (m.thinking_segments && m.thinking_segments.length > 0) {
            base.thinkingSegments = m.thinking_segments.map((s) => ({ content: s }));
          }

          // Restore execution rounds (round-based model).
          // Typed via the SavedMessage shape instead of `any` (P1-40).
          if (m.execution_rounds && m.execution_rounds.length > 0) {
            base.executionRounds = m.execution_rounds.map((round) => ({
              thinking: round.thinking ? { content: round.thinking.content } : undefined,
              toolCallIds: [...round.tool_call_ids],
            }));
          }

          // Restore chronological order using stable tool execution IDs.
          if (m.execution_steps && m.execution_steps.length > 0) {
            base.executionSteps = m.execution_steps.reduce<ExecutionStep[]>((steps, step) => {
              if (step.type === 'thinking' && step.index != null) {
                steps.push({ type: 'thinking', index: step.index });
              }
              if (step.type === 'tool' && step.call_id) {
                steps.push({ type: 'tool', callId: step.call_id });
              }
              return steps;
            }, []);
          }

          // Restore attachments (data URLs) so images/files render on reload.
          if (m.attachments && m.attachments.length > 0) {
            base.attachments = m.attachments.map((a) => ({
              name: a.name,
              mime_type: a.mime_type,
              url: a.url,
              size: a.size,
              source: a.source,
            }));
          }

          return base;
        });

      const chatStore = useChatStore.getState();
      chatStore.replaceMessages(chatMessages);
      chatStore.setHistoryView(!restoreReady);

      set({ activeId: id, isLoading: false });
      loadingConversationId = null;
    } catch (e) {
      console.error('Failed to load conversation:', e);
      if (generation === loadGeneration) {
        loadingConversationId = null;
        set({ isLoading: false });
      }
    }
  },

  branchCurrent: async (userTurnIndex: number) => {
    const activeId = get().activeId;
    const workspaceId = get().workspaceId;
    if (!activeId) throw new Error('No active conversation to branch');
    const result = await conversationApi.branch(workspaceId, activeId, userTurnIndex);
    set({ activeId: result.id, isLoading: false });
    await get().init(workspaceId);
    return { id: result.id, targetContent: result.target_content };
  },

  deleteConversation: async (id: string) => {
    // P1-12: 此前 catch 吞错后仍执行本地删除 → 后端还在但前端已移除,
    // 刷新后数据"恢复"又"丢失", 体验混乱。改为 API 失败则不更新本地 + 报错。
    if (get().activeId === id || loadingConversationId === id) {
      loadGeneration += 1;
      loadingConversationId = null;
      set({ isLoading: false });
    }
    try {
      const receipt = await conversationApi.delete(get().workspaceId, id);
      if (receipt.cleanup_pending) {
        useToastStore.getState().addToast('warning', '会话已删除，剩余本地清理将在下次启动时继续');
      }
    } catch (e) {
      console.error('Failed to delete conversation:', e);
      useToastStore.getState().addToast('error', '删除会话失败，请重试');
      return;
    }

    const wasActive = get().activeId === id;
    set((s) => {
      const conversations = s.conversations.filter((c) => c.id !== id);
      return {
        conversations,
        activeId: s.activeId === id ? null : s.activeId,
      };
    });
    if (wasActive) {
      useChatStore.getState().clearMessages();
      useTaskRuntimeStore.getState().reset();
      useToolExecutionStore.getState().clear();
      useSubagentRunStore.getState().clear();
    }
  },

  renameConversation: async (id: string, title: string) => {
    // P1-12: 同 deleteConversation, API 失败则不更新本地 + 报错。
    try {
      await conversationApi.update(get().workspaceId, id, { title });
    } catch (e) {
      console.error('Failed to rename conversation:', e);
      useToastStore.getState().addToast('error', '重命名会话失败，请重试');
      return;
    }

    set((s) => ({
      conversations: s.conversations.map((c) => (c.id === id ? { ...c, title } : c)),
    }));
  },

  startNew: async () => {
    loadGeneration += 1;
    loadingConversationId = null;
    // The backend is the canonical transcript writer. Only refresh metadata for
    // an already-persisted conversation; a blank draft must not be materialized
    // merely because the user starts another chat.
    const activeId = get().activeId;
    const currentMessages = useChatStore.getState().messages;
    const pendingSave =
      activeId && currentMessages.length > 0 ? get().saveCurrent(currentMessages) : null;

    // Detach the UI immediately. Existing conversation agents are intentionally
    // left running in their own pool slots and can be resumed from the sidebar.
    useChatStore.getState().clearMessages();
    useTaskRuntimeStore.getState().reset();
    useToolExecutionStore.getState().clear();
    useSubagentRunStore.getState().clear();
    set((state) => ({
      activeId: null,
      newConversationEpoch: state.newConversationEpoch + 1,
      isLoading: false,
    }));

    if (pendingSave) {
      try {
        await pendingSave;
      } catch (e) {
        console.error('Failed to save conversation before new chat:', e);
      }
    }
  },

  clearCurrent: async () => {
    loadGeneration += 1;
    loadingConversationId = null;
    set({ isLoading: false });
    try {
      const { workspaceId, activeId } = get();
      if (activeId) await sessionApi.reset(workspaceId, activeId);
    } catch (e) {
      console.error('Failed to reset session while clearing chat:', e);
    }

    useChatStore.getState().clearMessages();
    useTaskRuntimeStore.getState().reset();
    useToolExecutionStore.getState().clear();
    useSubagentRunStore.getState().clear();
    set({ activeId: null, isLoading: false });
  },

  getGroupedConversations: () => {
    return groupByDate(get().conversations);
  },
}));
