import { restoredAgentInput } from '../lib/agentCommunication';
import { create } from 'zustand';
import { useChatStore } from './chatStore';
import {
  sessionApi,
  conversationApi,
  toolExecutionApi,
  type ConversationForkRequest,
  type ConversationForkReceipt,
} from '../api/endpoints';
import { useWorkspaceStore } from './workspaceStore';
import { productDataScope, sameProductDataScope } from '../lib/productDataScope';
import { useToastStore } from './toastStore';
import { useToolExecutionStore } from './toolExecutionStore';
import { ingestSubagentExecEvent, useSubagentRunStore } from './subagentRunStore';
import { useTaskRuntimeStore } from './taskRuntimeStore';
import { nativeSourceKey, savedPresentationBySource } from '../lib/nativePresentation';
import { transcriptToolExecution } from '../types/api';
import { GLOBAL_WORKSPACE_ID, viewAddress, type ViewAddress } from '../lib/viewAddress';
import type {
  ChatMessage,
  ConversationListItem,
  ExecutionStep,
  SavedMessage,
  SideConversationEntry,
  TranscriptToolRecord,
  ToolExecution,
} from '../types/api';

let listGeneration = 0;
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
  /** Archived in EKO's workspace-scoped conversation projection. */
  archived?: boolean;
}

function conversationMeta(item: ConversationListItem, workspaceId: string): ConversationMeta {
  return {
    id: item.conversation_id,
    title: item.title ?? '',
    lastMessage: '',
    messageCount: item.message_count,
    createdAt: new Date(item.created_at).getTime(),
    updatedAt: new Date(item.updated_at).getTime(),
    workspaceId,
    archived: item.archived ?? false,
  };
}

interface ConversationState {
  /** Workspace owning this list and active conversation projection. */
  workspaceId: string;
  /** All conversations sorted by updatedAt desc */
  conversations: ConversationMeta[];
  /** Durable parent/member projection from AgentRouter groups. */
  sideConversations: SideConversationEntry[];
  /** Conversation ids archived locally for the active workspace. */
  archivedConversationIds: string[];
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
  saveCurrent: (
    messages: ChatMessage[],
    onCreated?: (address: ViewAddress) => void
  ) => Promise<ViewAddress | null>;
  /** 仅本次已投影 transcript 时返回 true；包含上下文恢复失败后的只读历史。 */
  loadConversation: (id: string, canProject?: () => boolean) => Promise<boolean>;
  /** Branch the canonical transcript immediately before one user turn. */
  branchCurrent: (userTurnIndex: number) => Promise<{ id: string; targetContent: string }>;
  /** Copy through a completed reply, refresh the list, and open the ordinary fork. */
  forkCurrent: (
    fork: ConversationForkRequest,
    canOpen?: () => boolean
  ) => Promise<ConversationForkReceipt & { opened: boolean; open_error?: string }>;
  /** Create and immediately open a Side Conversation. */
  createSideConversation: (input: {
    requestId: string;
    prompt: string;
    title?: string;
    modelId?: string;
  }) => Promise<SideConversationEntry>;
  /** Update one child-local model; null means inherit the primary model. */
  updateSideConversationModel: (conversationId: string, modelId: string | null) => Promise<void>;
  /** Retry the stable initial prompt after a synchronous launch failure. */
  retrySideConversation: (conversationId: string) => Promise<void>;
  /** Publish a rebuildable running hint until the backend lifecycle poll settles. */
  noteSideConversationTurnStarted: (conversationId: string) => void;
  /** Archive a conversation in the active workspace projection. */
  archiveConversation: (id: string) => Promise<void>;
  /** Restore an archived conversation. */
  restoreConversation: (id: string) => Promise<void>;
  /** Delete a conversation */
  deleteConversation: (id: string) => Promise<void>;
  /** Rename a conversation */
  renameConversation: (id: string, title: string) => Promise<void>;
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
    // AgentRouter owns these observations; they must never become user input rows.
    if (m.agentInput) continue;
    // Save the main message
    const entry: SavedMessage = {
      message_id: m.id,
      role: m.role,
      created_at: new Date(m.timestamp).toISOString(),
      content: m.content,
      internal_agent: m.internalAgent,
      assistant_blocks: m.assistantBlocks,
      final_answer_confirmed: m.finalAnswerConfirmed,
      execution_status: m.executionStatus,
      final_answer_source: m.finalAnswerSource,
      turn_source: m.turnSource,
    };

    // Save execution steps (chronological order of thinking/tool interleaving)
    if (m.executionSteps && m.executionSteps.length > 0) {
      entry.execution_steps = m.executionSteps.map((s) =>
        s.type === 'assistant'
          ? { type: s.type, index: s.index }
          : { type: s.type, call_id: s.callId }
      );
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

function canonicalMessageTimestamp(message: SavedMessage): number {
  const timestamp = Date.parse(message.created_at);
  if (!Number.isFinite(timestamp)) throw new Error('会话消息缺少有效的原始时间');
  return timestamp;
}

export function restoredMessageId(
  conversationId: string,
  index: number,
  message: SavedMessage
): string {
  return message.message_id ?? `loaded-${conversationId}-${index}`;
}

/** Restore display references without turning transcript text into runtime state. */
function restoreTranscriptTools(
  saved: SavedMessage[],
  messages: ChatMessage[],
  workspaceId: string,
  conversationId: string,
  tools: Readonly<Record<string, ToolExecution>>
): ChatMessage[] {
  const callCounts = new Map<string, number>();
  for (const message of saved) {
    for (const call of message.tool_calls ?? []) {
      callCounts.set(call.id, (callCounts.get(call.id) ?? 0) + 1);
    }
  }
  const resultMeta = saved.map((message) => {
    if (message.role !== 'tool' || !message.tool_result) return null;
    try {
      const value: unknown = JSON.parse(message.tool_result);
      if (!value || typeof value !== 'object') return null;
      const fields = value as Record<string, unknown>;
      return {
        callId: typeof fields.tool_call_id === 'string' ? fields.tool_call_id : undefined,
        name: typeof fields.name === 'string' ? fields.name : undefined,
      };
    } catch {
      return null;
    }
  });
  const pairedResults = new Set<number>();
  const byIndex = new Map<number, ChatMessage>();
  let visibleIndex = 0;
  saved.forEach((source, sourceIndex) => {
    if (source.role !== 'user' && source.role !== 'assistant') return;
    const message = messages.at(visibleIndex);
    visibleIndex += 1;
    if (!message) return;
    byIndex.set(sourceIndex, message);
    if (source.role !== 'assistant') return;
    const existingIds = new Set([
      ...(message.executionSteps?.flatMap((step) => (step.type === 'tool' ? [step.callId] : [])) ??
        []),
    ]);
    const claimedReferences = new Set<string>();
    const records = (source.tool_calls ?? []).map((call, callIndex): TranscriptToolRecord => {
      const unique = callCounts.get(call.id) === 1;
      const record: TranscriptToolRecord = {
        id: `transcript:${JSON.stringify([workspaceId, conversationId, sourceIndex, callIndex, call.id])}`,
        workspaceId,
        conversationId,
        messageId: source.message_id,
        callId: call.id,
        callType: call.type,
        name: call.function.name,
        requestedArguments: call.function.arguments,
        identityAmbiguous: !unique,
      };
      const summary = transcriptToolExecution(record, tools);
      // An old opaque reference can identify a display row without proving its
      // runtime owner. Reusing that key never grants a terminal status.
      const matchingReferences = [...existingIds].filter((id) => {
        const referenced = tools[id];
        return (
          !claimedReferences.has(id) &&
          referenced &&
          referenced.workspace_id === workspaceId &&
          referenced.conversation_id === conversationId &&
          referenced.call_id === call.id &&
          referenced.name === call.function.name
        );
      });
      if (summary && existingIds.has(summary.id)) record.id = summary.id;
      else if (existingIds.has(call.id) && !claimedReferences.has(call.id)) record.id = call.id;
      else if (matchingReferences.length === 1) record.id = matchingReferences.at(0) ?? record.id;
      else if (summary) record.id = summary.id;
      if (existingIds.has(record.id)) claimedReferences.add(record.id);
      const resultIndexes: number[] = [];
      if (unique) {
        for (let index = sourceIndex + 1; index < saved.length; index += 1) {
          if (saved.at(index)?.role === 'user') break;
          const meta = resultMeta.at(index);
          if (meta?.callId === call.id && meta.name === call.function.name)
            resultIndexes.push(index);
        }
      }
      if (resultIndexes.length === 1) {
        const resultIndex = resultIndexes.at(0);
        if (resultIndex != null) {
          record.recordedOutput = saved.at(resultIndex)?.content ?? '';
          pairedResults.add(resultIndex);
        }
      }
      return record;
    });
    // Every retained reference gets the same message-bound status gate. A bare
    // execution key with no source owner must not bypass it through tools[id].
    for (const id of existingIds) {
      if (records.some((record) => record.id === id)) continue;
      const referenced = tools[id];
      const exactOwner =
        referenced &&
        source.message_id &&
        referenced.workspace_id === workspaceId &&
        referenced.conversation_id === conversationId &&
        referenced.owner.kind === 'chat' &&
        referenced.owner.message_id === source.message_id;
      records.push({
        id,
        workspaceId,
        conversationId,
        messageId: source.message_id,
        callId: exactOwner ? referenced.call_id : undefined,
        name: exactOwner ? referenced.name : undefined,
        identityAmbiguous: !exactOwner || (callCounts.get(referenced.call_id) ?? 0) > 1,
      });
    }
    message.transcriptTools = records;
    const missing = records.filter((record) => !existingIds.has(record.id));
    if (missing.length === 0) return;
    message.executionSteps = [
      ...(message.executionSteps ?? []),
      ...missing.map((record) => ({ type: 'tool' as const, callId: record.id })),
    ];
  });
  return saved.flatMap((source, sourceIndex) => {
    const message = byIndex.get(sourceIndex);
    if (message) return [message];
    if (source.role !== 'tool' || pairedResults.has(sourceIndex)) return [];
    const id = `transcript-result:${JSON.stringify([workspaceId, conversationId, sourceIndex])}`;
    const meta = resultMeta.at(sourceIndex);
    return [
      {
        id,
        role: 'tool' as const,
        content: source.content ?? '',
        internalAgent: source.internal_agent ?? false,
        isStreaming: false,
        timestamp: canonicalMessageTimestamp(source),
        transcriptTools: [
          {
            id,
            workspaceId,
            conversationId,
            callId: meta?.callId,
            name: meta?.name,
            recordedOutput: source.content ?? '',
            identityAmbiguous: true,
          },
        ],
      },
    ];
  });
}

function projectSideInitialPrompt(conversationId: string, messageId: string, prompt: string): void {
  if (useConversationStore.getState().activeId !== conversationId) return;
  const chatStore = useChatStore.getState();
  if (chatStore.messages.some((message) => message.id === messageId)) return;
  chatStore.replaceMessages([
    ...chatStore.messages,
    {
      id: messageId,
      role: 'user',
      content: prompt,
      timestamp: Date.now(),
    },
  ]);
}

// ── Store ──

export const useConversationStore = create<ConversationState>((set, get) => ({
  workspaceId: GLOBAL_WORKSPACE_ID,
  conversations: [],
  sideConversations: [],
  archivedConversationIds: [],
  activeId: null,
  newConversationEpoch: 0,
  isLoading: false,

  init: async (workspaceId: string) => {
    const generation = listGeneration + 1;
    listGeneration = generation;
    if (get().workspaceId !== workspaceId) {
      loadGeneration += 1;
      loadingConversationId = null;
      set({ workspaceId, isLoading: false });
    } else {
      set({ workspaceId });
    }
    try {
      const items = await conversationApi.list(workspaceId);
      if (generation !== listGeneration || get().workspaceId !== workspaceId) return;
      if (import.meta.env.DEV)
        console.debug('[conversationStore] init: loaded', items.length, 'conversations');
      const metas: ConversationMeta[] = items
        .map((item) => conversationMeta(item, workspaceId))
        .sort((a, b) => b.updatedAt - a.updatedAt);
      const activeId = get().activeId;
      const sideConversations = items
        .flatMap((item) => (item.side_conversation ? [item.side_conversation] : []))
        .map((entry) =>
          entry.conversation_id === activeId ? { ...entry, unread_count: 0 } : entry
        );
      set({
        conversations: metas,
        sideConversations,
        archivedConversationIds: metas.filter((item) => item.archived).map((item) => item.id),
      });
      if (
        activeId &&
        items.some(
          (item) =>
            item.side_conversation?.conversation_id === activeId &&
            item.side_conversation.unread_count > 0
        )
      ) {
        void conversationApi.markSideViewed(workspaceId, activeId).catch((error) => {
          console.error('[conversationStore] mark Side Conversation viewed FAILED:', error);
        });
      }
    } catch (e) {
      console.error('[conversationStore] init FAILED:', e);
      if (generation === listGeneration && get().workspaceId === workspaceId) {
        set({ conversations: [], sideConversations: [] });
      }
    }
  },

  detachForWorkspace: (workspaceId: string) => {
    listGeneration += 1;
    loadGeneration += 1;
    loadingConversationId = null;
    set((state) => ({
      workspaceId,
      conversations: [],
      sideConversations: [],
      archivedConversationIds: [],
      activeId: null,
      newConversationEpoch: state.newConversationEpoch + 1,
      isLoading: false,
    }));
  },

  saveCurrent: async (messages: ChatMessage[], onCreated?: (address: ViewAddress) => void) => {
    const activeId = get().activeId;
    const workspaceId = get().workspaceId;
    const originalLoadGeneration = loadGeneration;
    const originalNewConversationEpoch = get().newConversationEpoch;
    if (messages.length === 0) return null;
    let savedId = activeId;

    const firstUserMsg = messages.find(
      (m) => m.role === 'user' && !m.internalAgent && !m.agentInput
    );
    // A received observation cannot create or rename a conversation as if the user submitted it.
    if (!firstUserMsg) return activeId ? viewAddress(workspaceId, activeId) : null;
    const title =
      firstUserMsg?.content?.slice(0, 50) ||
      get().conversations.find((conversation) => conversation.id === activeId)?.title ||
      'New Chat';
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
        if (!res?.id || typeof res.id !== 'string')
          throw new Error('Conversation save did not return its identity');
        savedId = res.id;
        // The save belongs to its original address even when selection moved.
        // Only its original view may adopt the newly created identity.
        if (
          get().workspaceId === workspaceId &&
          get().activeId === activeId &&
          loadGeneration === originalLoadGeneration &&
          get().newConversationEpoch === originalNewConversationEpoch
        ) {
          // Let the original sender bind its existing admission before the
          // selection effect observes this one legitimate null-to-id change.
          // A view observer failure cannot undo the successful canonical save.
          try {
            onCreated?.(viewAddress(workspaceId, savedId));
          } catch (error) {
            console.error('[saveCurrent] identity observer FAILED:', error);
          }
          set({ activeId: savedId });
        }
      }
    } catch (e) {
      console.error('[saveCurrent] FAILED:', e);
      return null; // Don't attempt list refresh if save failed
    }
    if (!savedId) return null;
    const savedAddress = viewAddress(workspaceId, savedId);
    if (get().workspaceId !== workspaceId) return savedAddress;

    // Refresh list (best-effort, must not throw)
    try {
      const items = await conversationApi.list(workspaceId);
      if (get().workspaceId !== workspaceId) return savedAddress;
      if (import.meta.env.DEV)
        console.debug('[saveCurrent] refreshed list:', items.length, 'conversations');
      const metas: ConversationMeta[] = items
        .map((item) => conversationMeta(item, workspaceId))
        .sort((a, b) => b.updatedAt - a.updatedAt);
      set({
        conversations: metas,
        archivedConversationIds: metas.filter((item) => item.archived).map((item) => item.id),
      });
    } catch (e) {
      console.error('[saveCurrent] refresh FAILED:', e);
    }
    return savedAddress;
  },

  loadConversation: async (id: string, canProject = () => true) => {
    const workspaceId = get().workspaceId;
    const generation = loadGeneration + 1;
    loadGeneration = generation;
    loadingConversationId = id;
    // Keep the current view and its live projection until the requested record
    // has loaded successfully. A parse error must not erase the prior view.
    set({ isLoading: true });

    try {
      const [record, tools] = await Promise.all([
        conversationApi.get(workspaceId, id),
        toolExecutionApi.list(workspaceId, id),
      ]);
      if (generation !== loadGeneration || get().workspaceId !== workspaceId || !canProject())
        return false;
      // Restore agent context on the backend so conversation can continue.
      let restoreReady = true;
      try {
        await conversationApi.restore(workspaceId, id);
      } catch (e) {
        if (generation !== loadGeneration || get().workspaceId !== workspaceId || !canProject())
          return false;
        restoreReady = false;
        console.error('Failed to restore agent context:', e);
        useToastStore.getState().addToast('error', '会话上下文恢复失败，已切换为只读历史视图');
        useChatStore.getState().setHistoryView(true);
      }
      if (generation !== loadGeneration || get().workspaceId !== workspaceId || !canProject())
        return false;
      // Keep the source projection until hydration and navigation admission
      // both finish; closing a fork observer must not erase the current chat.
      useChatStore.getState().clearMessages();
      useTaskRuntimeStore.getState().reset();
      useToolExecutionStore.getState().clear();
      useSubagentRunStore.getState().clear();
      set({ activeId: null });
      useToolExecutionStore.getState().hydrateConversation(workspaceId, id, tools);

      const presentations = savedPresentationBySource(record.messages);
      for (const value of presentations.values()) {
        for (const entry of value?.records ?? []) {
          if (entry.payload.kind === 'subagent')
            ingestSubagentExecEvent(entry.payload.data.event, {
              event_id: entry.event_id,
              stream_id: entry.cursor.stream_id,
              sequence: entry.cursor.sequence,
              timestamp: entry.timestamp,
            });
        }
      }
      // Convert user/assistant messages for display. Tool-role payloads stay in
      // the agent context; the GUI renders tools from lightweight summaries.
      const chatMessages: ChatMessage[] = record.messages
        .filter((message) => message.role === 'user' || message.role === 'assistant')
        .map((m, idx) => {
          const base: ChatMessage = {
            // P2-7: 此前用 `loaded-${Date.now()}-${idx}`, 每次加载产生不同 id,
            // React key 变化导致消息列表全量重渲染。改用会话 id + 索引；
            // 时间只读后端原始 created_at，不用加载时刻冒消息顺序。
            id: restoredMessageId(id, idx, m),
            role: m.role as 'user' | 'assistant',
            content: m.content || '',
            internalAgent: m.internal_agent ?? false,
            presentationRecords: m.turn_source
              ? presentations.get(nativeSourceKey(m.turn_source))?.records
              : undefined,
            presentationUnavailable:
              m.presentation_unavailable ||
              (m.turn_source && presentations.get(nativeSourceKey(m.turn_source)) === null) ||
              false,
            assistantBlocks: m.assistant_blocks,
            finalAnswerConfirmed: m.final_answer_confirmed,
            executionStatus: m.execution_status,
            finalAnswerSource: m.final_answer_source,
            turnSource: m.turn_source,
            finalAnswerExecutionId: m.final_answer_confirmed
              ? m.execution_status?.execution_id
              : undefined,
            isStreaming: false,
            // 会话时间只读原canonical行，来源payload的创建时间不覆盖接收顺序。
            timestamp: canonicalMessageTimestamp(m),
            ...(m.agent_input ? restoredAgentInput(m.agent_input, workspaceId, id) : {}),
          };

          // Restore chronological order using stable tool execution IDs.
          if (m.execution_steps && m.execution_steps.length > 0) {
            base.executionSteps = m.execution_steps.reduce<ExecutionStep[]>((steps, step) => {
              if (step.type === 'assistant' && step.index != null) {
                steps.push({ type: 'assistant', index: step.index });
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
      chatStore.replaceMessages(
        restoreTranscriptTools(
          record.messages,
          chatMessages,
          workspaceId,
          id,
          useToolExecutionStore.getState().tools
        )
      );
      chatStore.setHistoryView(!restoreReady);

      set((state) => ({
        activeId: id,
        isLoading: false,
        sideConversations: state.sideConversations.map((entry) =>
          entry.conversation_id === id ? { ...entry, unread_count: 0 } : entry
        ),
      }));
      loadingConversationId = null;
      return true;
    } catch (e) {
      console.error('Failed to load conversation:', e);
      if (generation === loadGeneration && get().workspaceId === workspaceId && canProject()) {
        loadingConversationId = null;
        set({ isLoading: false });
        useToastStore.getState().addToast('error', '无法加载会话或工具历史；原始记录保留，请重试');
      }
      return false;
    } finally {
      if (generation === loadGeneration && get().workspaceId === workspaceId && !canProject()) {
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

  forkCurrent: async (fork, canOpen = () => true) => {
    const sourceId = get().activeId;
    const workspaceId = get().workspaceId;
    const scope = productDataScope(useWorkspaceStore.getState().current);
    const generation = loadGeneration;
    const epoch = get().newConversationEpoch;
    const preparation = fork.preparation;
    if (
      !sourceId ||
      preparation.source_id !== sourceId ||
      preparation.workspace_id !== workspaceId ||
      preparation.workspace_id !== scope.workspaceId ||
      preparation.workspace_generation !== scope.workspaceGeneration
    )
      throw new Error('分叉来源已变化，请从原回复重新打开');
    const ownsSource = () =>
      canOpen() &&
      get().workspaceId === workspaceId &&
      get().activeId === sourceId &&
      get().newConversationEpoch === epoch &&
      generation === loadGeneration &&
      !useWorkspaceStore.getState().isLoading &&
      sameProductDataScope(scope, productDataScope(useWorkspaceStore.getState().current));
    const result = await conversationApi.branch(
      workspaceId,
      sourceId,
      preparation.user_turn_index,
      true,
      fork
    );
    if (
      result.request_id !== preparation.request_id ||
      result.source_id !== sourceId ||
      result.source_workspace_id !== workspaceId ||
      result.destination !== fork.destination
    )
      throw new Error('分叉回执身份不匹配，请从工作空间或 Worktree 列表核对已创建资源');
    const receipt = result as ConversationForkReceipt;
    if (receipt.workspace) {
      const workspace = receipt.workspace;
      const targetState = useWorkspaceStore.getState();
      const known = targetState.workspaces.find((item) => item.id === workspace.id);
      if (
        workspace.id !== receipt.workspace_id ||
        workspace.product_data_generation !== receipt.workspace_generation ||
        (known && known.product_data_generation !== workspace.product_data_generation) ||
        (targetState.current?.id === workspace.id &&
          targetState.current.product_data_generation !== workspace.product_data_generation)
      )
        return {
          ...receipt,
          opened: false,
          open_error: '目标工作空间代际已变化，保留原资源回执；请从当前工作空间列表检查。',
        };
      try {
        await useWorkspaceStore.getState().refreshList();
      } catch (error) {
        return {
          ...receipt,
          opened: false,
          open_error: error instanceof Error ? error.message : '已创建资源的列表尚未核实',
        };
      }
      const actual = useWorkspaceStore
        .getState()
        .workspaces.find((item) => item.id === workspace.id);
      if (!actual || actual.product_data_generation !== receipt.workspace_generation)
        return {
          ...receipt,
          opened: false,
          open_error: '目标已删除或重开，原资源回执保留，请检查当前列表。',
        };
    }
    if (!receipt.success || !receipt.imported || !receipt.id || !ownsSource())
      return { ...receipt, opened: false };
    try {
      if (receipt.workspace_id === workspaceId) {
        if (receipt.workspace_generation !== scope.workspaceGeneration)
          throw new Error('目标工作空间代际已变化，请从最近任务重新打开');
        await get().init(workspaceId);
        if (!ownsSource()) return { ...receipt, opened: false };
      } else {
        const target = receipt.workspace;
        if (
          !target ||
          target.id !== receipt.workspace_id ||
          target.product_data_generation !== receipt.workspace_generation
        )
          throw new Error('目标工作空间回执不完整，请从工作空间列表打开');
        await useWorkspaceStore
          .getState()
          .switchTo(target.id, undefined, receipt.workspace_generation);
        if (
          !canOpen() ||
          get().newConversationEpoch !== epoch + 1 ||
          get().activeId !== null ||
          !sameProductDataScope(
            {
              workspaceId: target.id,
              workspaceGeneration: target.product_data_generation,
            },
            productDataScope(useWorkspaceStore.getState().current)
          )
        )
          return { ...receipt, opened: false };
      }
      const selectionGeneration = loadGeneration + 1;
      const canProject = () =>
        canOpen() &&
        sameProductDataScope(
          {
            workspaceId: receipt.workspace_id,
            workspaceGeneration: receipt.workspace_generation,
          },
          productDataScope(useWorkspaceStore.getState().current)
        );
      await get().loadConversation(receipt.id, canProject);
      const opened =
        canProject() && get().activeId === receipt.id && get().workspaceId === receipt.workspace_id;
      return {
        ...receipt,
        opened,
        ...(!opened && canProject() && selectionGeneration === loadGeneration
          ? { open_error: '会话已创建，但历史尚未打开；可从最近任务重试。' }
          : {}),
      };
    } catch (error) {
      return {
        ...receipt,
        opened: false,
        open_error: error instanceof Error ? error.message : '打开分叉失败',
      };
    }
  },

  createSideConversation: async ({ requestId, prompt, title, modelId }) => {
    const parentConversationId = get().activeId;
    const workspaceId = get().workspaceId;
    if (!parentConversationId) throw new Error('A persisted primary conversation is required');
    if (get().sideConversations.some((entry) => entry.conversation_id === parentConversationId)) {
      throw new Error('Side Conversations cannot create nested Side Conversations');
    }
    const receipt = await conversationApi.createSide({
      workspace_id: workspaceId,
      parent_conversation_id: parentConversationId,
      request_id: requestId,
      prompt,
      title: title?.trim() || null,
      model_id: modelId?.trim() || null,
    });
    await get().init(workspaceId);
    await get().loadConversation(receipt.creation.entry.conversation_id);
    projectSideInitialPrompt(
      receipt.creation.entry.conversation_id,
      receipt.first_turn?.message_key ?? `side-start:${receipt.creation.entry.group_id}`,
      receipt.initial_prompt
    );
    if (receipt.launch_error) {
      useToastStore
        .getState()
        .addToast('error', `支线已创建，但首轮启动失败：${receipt.launch_error}`);
    }
    return receipt.creation.entry;
  },

  updateSideConversationModel: async (conversationId, modelId) => {
    const workspaceId = get().workspaceId;
    await conversationApi.updateSideModel(workspaceId, conversationId, modelId);
    if (get().workspaceId !== workspaceId) return;
    set((state) => ({
      sideConversations: state.sideConversations.map((item) =>
        item.conversation_id === conversationId ? { ...item, model_id: modelId } : item
      ),
    }));
    if (get().activeId === conversationId) {
      await get().loadConversation(conversationId);
    }
  },

  retrySideConversation: async (conversationId) => {
    const workspaceId = get().workspaceId;
    const receipt = await conversationApi.retrySide(workspaceId, conversationId);
    if (get().workspaceId !== workspaceId) return;
    await get().init(workspaceId);
    await get().loadConversation(conversationId);
    projectSideInitialPrompt(
      conversationId,
      receipt.first_turn?.message_key ?? `side-start:${receipt.creation.entry.group_id}`,
      receipt.initial_prompt
    );
    if (receipt.launch_error) {
      useToastStore.getState().addToast('error', `支线首轮仍未启动：${receipt.launch_error}`);
    }
  },

  noteSideConversationTurnStarted: (conversationId) => {
    set((state) => ({
      sideConversations: state.sideConversations.map((entry) =>
        entry.conversation_id === conversationId ? { ...entry, status: 'running' } : entry
      ),
    }));
  },

  archiveConversation: async (id: string) => {
    const workspaceId = get().workspaceId;
    await conversationApi.setArchived(workspaceId, id, true);
    const archivedIds = new Set(get().archivedConversationIds);
    archivedIds.add(id);
    set((state) => ({
      archivedConversationIds: [...archivedIds],
      conversations: state.conversations.map((conversation) =>
        conversation.id === id ? { ...conversation, archived: true } : conversation
      ),
    }));
  },

  restoreConversation: async (id: string) => {
    const workspaceId = get().workspaceId;
    await conversationApi.setArchived(workspaceId, id, false);
    const archivedIds = new Set(get().archivedConversationIds);
    archivedIds.delete(id);
    set((state) => ({
      archivedConversationIds: [...archivedIds],
      conversations: state.conversations.map((conversation) =>
        conversation.id === id ? { ...conversation, archived: false } : conversation
      ),
    }));
  },

  deleteConversation: async (id: string) => {
    // P1-12: 此前 catch 吞错后仍执行本地删除 → 后端还在但前端已移除,
    // 刷新后数据"恢复"又"丢失", 体验混乱。改为 API 失败则不更新本地 + 报错。
    const initialChildIds = new Set(
      get()
        .sideConversations.filter((entry) => entry.parent_conversation_id === id)
        .map((entry) => entry.conversation_id)
    );
    const removesConversation = (conversationId: string | null) =>
      conversationId === id || (conversationId != null && initialChildIds.has(conversationId));
    if (removesConversation(get().activeId) || removesConversation(loadingConversationId)) {
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

    const archivedIds = new Set(get().archivedConversationIds);
    archivedIds.delete(id);
    let clearedActive = false;
    set((s) => {
      const removedSideIds = new Set(
        s.sideConversations
          .filter((entry) => entry.conversation_id === id || entry.parent_conversation_id === id)
          .map((entry) => entry.conversation_id)
      );
      const conversations = s.conversations.filter(
        (conversation) => conversation.id !== id && !removedSideIds.has(conversation.id)
      );
      const activeRemoved =
        s.activeId === id || (s.activeId != null && removedSideIds.has(s.activeId));
      clearedActive = activeRemoved;
      return {
        conversations,
        sideConversations: s.sideConversations.filter(
          (entry) => entry.conversation_id !== id && entry.parent_conversation_id !== id
        ),
        archivedConversationIds: [...archivedIds],
        activeId: activeRemoved ? null : s.activeId,
      };
    });
    if (clearedActive) {
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
      throw e;
    }

    set((s) => ({
      conversations: s.conversations.map((c) => (c.id === id ? { ...c, title } : c)),
      sideConversations: s.sideConversations.map((entry) =>
        entry.conversation_id === id ? { ...entry, title } : entry
      ),
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
    const generation = loadGeneration;
    const { workspaceId, activeId, newConversationEpoch } = get();
    loadingConversationId = null;
    set({ isLoading: false });
    try {
      if (activeId) await sessionApi.reset(workspaceId, activeId);
    } catch (e) {
      console.error('Failed to reset session while clearing chat:', e);
    }

    // The original reset still settles, but its late result must not erase a
    // newer selection or a reopened instance of the same conversation.
    if (
      generation !== loadGeneration ||
      get().workspaceId !== workspaceId ||
      get().activeId !== activeId ||
      get().newConversationEpoch !== newConversationEpoch
    ) {
      return;
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

/** 复用既有加载/新会话epoch，为异步只读呈现补件保留原view身份。 */
export function captureConversationView(): () => boolean {
  const initial = useConversationStore.getState();
  const generation = loadGeneration;
  const epoch = initial.newConversationEpoch;
  const workspaceId = initial.workspaceId;
  const conversationId = initial.activeId;
  return () => {
    const current = useConversationStore.getState();
    return (
      generation === loadGeneration &&
      epoch === current.newConversationEpoch &&
      workspaceId === current.workspaceId &&
      conversationId === current.activeId
    );
  };
}
