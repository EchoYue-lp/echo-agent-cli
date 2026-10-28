import type { AgentCommunicationInput } from '../lib/agentCommunication';
import { create } from 'zustand';
import { isHitlRequestIdentity, sameHitlIdentity } from '../types/api';
import type { HitlRequestIdentity } from '../types/api';
import type {
  AssistantMessageBlock,
  NativeExecutionStatus,
  NativeFinalAnswerSource,
  ChatMessage,
  ApprovalRequest,
  ChatRunStatus,
} from '../types/api';
import { useConversationStore } from './conversationStore';

export interface ChatActivity {
  key: string;
  label: string;
  startedAt: number;
  lastEventAt: number;
  scope?: string;
}

/** 当前上下文窗口占用快照（对齐 Claude Code statusline 语义）。 */
export interface ContextWindowUsage {
  /** 本次请求的实际输入 token（= 当前上下文主体），已含 cache 部分。 */
  inputTokens: number;
  /** 其中命中缓存的部分。 */
  cachedTokens: number;
  /** 写入缓存的部分。 */
  cacheCreationTokens: number;
  /** 本次生成 token（不计入占用，仅参考）。 */
  outputTokens: number;
  /** provider 是否上报了 usage；false 时不应写入 store。 */
  usageReported: boolean;
}

/** 会话级 LLM 用量累计（缓存命中率）；压缩不清，会话重置时清零。 */
export interface ContextUsageAccumulator {
  totalInput: number;
  totalCached: number;
}

export type PendingHitlRequest = { identity: HitlRequestIdentity } & (
  | ({ kind: 'approval' } & ApprovalRequest)
  | { kind: 'input'; requestId: string; prompt?: string }
  | {
      kind: 'selection';
      requestId: string;
      prompt: string;
      options: string[];
      taskId?: string;
      context?: unknown;
      phase?: string;
    }
);

function waitingStatus(request: PendingHitlRequest): ChatRunStatus {
  return request.kind === 'approval' ? 'waiting_approval' : 'waiting_input';
}

export function cacheHitRate(acc: ContextUsageAccumulator): number | null {
  if (acc.totalInput <= 0) return null;
  return acc.totalCached / acc.totalInput;
}

interface ChatState {
  agentInputs: AgentCommunicationInput[];
  agentInputsBinding: string | null;
  agentInputReadError: { binding: string; message: string } | null;
  noteAgentInputReadError: (binding: string, message: string) => void;
  observeAgentInputs: (
    workspaceId: string,
    conversationId: string,
    records: AgentCommunicationInput[],
    binding: string
  ) => void;
  messages: ChatMessage[];
  isStreaming: boolean;
  isCancelled: boolean;
  isThinking: boolean;
  runStatus: ChatRunStatus;
  /** 仅来自实际运行事件；计时刷新不产生新事件或发现。 */
  activity: ChatActivity | null;
  observeActivity: (key: string, label: string, at?: number, scope?: string) => void;
  noteActivityEvent: (at?: number) => void;
  pendingHitlRequests: PendingHitlRequest[];
  /** True when viewing a loaded historical conversation (agent has no context) */
  isHistoryView: boolean;
  /** 当前上下文窗口占用（来自最近一次 llm_usage 事件的真实 prompt_tokens）。null = 首次响应前 / 刚压缩后。 */
  contextWindow: ContextWindowUsage | null;
  /** 会话级缓存命中率累加器（压缩保留，clear/replace 清零）。 */
  usageAccumulator: ContextUsageAccumulator;

  addUserMessage: (content: string, attachments?: ChatMessage['attachments']) => string;
  removeMessages: (ids: string[]) => void;
  startAssistantMessage: (idOverride?: string) => string;
  continueAfterSteer: (
    assistantId: string | null,
    content: string,
    attachments?: ChatMessage['attachments']
  ) => string;
  appendToken: (id: string, token: string) => void;
  appendAssistantBlock: (
    id: string,
    block: AssistantMessageBlock,
    executionId?: string | null,
    publicationEventId?: string | null
  ) => void;
  observeExecutionStatus: (id: string, status: NativeExecutionStatus) => void;
  recordToolStart: (messageId: string, toolExecutionId: string) => void;
  applyFinalAnswer: (id: string, content: string, executionId?: string | null) => void;
  applyFinalAnswerSource: (id: string, source: NativeFinalAnswerSource) => void;
  settleAssistantMessage: (id: string) => void;
  projectAssistantError: (id: string, error: string) => void;
  handoffToTaskRuntime: (id: string, content: string, isRunning: boolean) => void;
  /** Insert a non-streaming assistant note (e.g. background subagent finished). */
  appendLocalAssistantNote: (content: string) => void;
  setStreaming: (v: boolean) => void;
  setThinking: (v: boolean) => void;
  setRunStatus: (status: ChatRunStatus) => void;
  clearInactiveTurnProjection: () => void;
  enqueueHitlRequest: (request: PendingHitlRequest) => void;
  removeHitlRequest: (identity: HitlRequestIdentity) => void;
  clearHitlRequests: () => void;
  addChartMessage: (spec: unknown) => void;
  clearMessages: () => void;
  replaceMessages: (messages: ChatMessage[]) => void;
  setHistoryView: (v: boolean) => void;
  /** 更新上下文窗口占用快照（由 llm_usage 事件驱动，仅 usageReported=true）。 */
  setContextWindow: (usage: ContextWindowUsage) => void;
  /** 压缩边界：只清 Snapshot，保留 Accumulator。 */
  clearContextWindow: () => void;
  /** 累加一次 usage（仅 usageReported=true 时由 handler 调用）。 */
  recordUsage: (input: number, cached: number) => void;
}

/// Maximum number of messages retained in-memory. Beyond this, oldest messages
/// are evicted to prevent OOM on very long conversations (P0-4).
const MAX_MESSAGES = 500;

/// Trim oldest messages when an array exceeds MAX_MESSAGES. Applied to every
/// path that grows or replaces the message list (addUserMessage,
/// startAssistantMessage, replaceMessages) so the cap cannot be bypassed via
/// the streaming path or a loaded historical conversation (P0-4).
function trimToMax(msgs: ChatMessage[]): ChatMessage[] {
  return msgs.length > MAX_MESSAGES ? msgs.slice(-MAX_MESSAGES) : msgs;
}

let msgCounter = 0;
const nextId = () => `msg-${++msgCounter}-${Date.now()}`;

/** Auto-save to conversationStore after state changes that add messages. */
function autoSave() {
  const state = useChatStore.getState();
  if (state.isStreaming) return;
  const msgs = state.messages;
  if (msgs.length > 0) {
    void useConversationStore.getState().saveCurrent(msgs);
  }
}

/// Debounced wrapper that replaces synchronous `autoSave()` calls inside
/// set() closures. Prevents N writes per streaming token and decouples the
/// chatStore→conversationStore dependency from the hot path.
let autoSaveTimer: ReturnType<typeof setTimeout> | undefined;
function scheduleAutoSave() {
  clearTimeout(autoSaveTimer);
  autoSaveTimer = setTimeout(autoSave, 300);
}

export const useChatStore = create<ChatState>((set, get) => ({
  agentInputs: [],
  agentInputsBinding: null,
  agentInputReadError: null,
  noteAgentInputReadError: (binding, message) => set({ agentInputReadError: { binding, message } }),
  observeAgentInputs: (workspaceId, conversationId, records, binding) => {
    const view = useConversationStore.getState();
    if (view.workspaceId !== workspaceId || view.activeId !== conversationId) return;
    set({ agentInputs: records, agentInputsBinding: binding, agentInputReadError: null });
  },
  messages: [],
  isStreaming: false,
  isCancelled: false,
  isThinking: false,
  runStatus: 'idle',
  activity: null,
  pendingHitlRequests: [],
  isHistoryView: false,
  contextWindow: null,
  usageAccumulator: { totalInput: 0, totalCached: 0 },

  observeActivity: (key, label, at = Date.now(), scope) =>
    set((state) => {
      if (!state.isStreaming || !Number.isFinite(at)) return state;
      const previous = state.activity;
      if (previous && at < previous.lastEventAt) return state;
      return {
        activity: {
          key,
          label,
          startedAt: previous?.key === key ? previous.startedAt : at,
          lastEventAt: Math.max(previous?.lastEventAt ?? at, at),
          scope,
        },
      };
    }),
  noteActivityEvent: (at = Date.now()) =>
    set((state) => ({
      activity:
        state.isStreaming && state.activity && Number.isFinite(at)
          ? {
              ...state.activity,
              lastEventAt: Math.max(state.activity.lastEventAt, at),
            }
          : state.activity,
    })),

  addUserMessage: (content, attachments) => {
    const id = nextId();
    set((s) => {
      const newMsg: ChatMessage = {
        id,
        role: 'user',
        content,
        attachments,
        timestamp: Date.now(),
      };
      const msgs = trimToMax([...s.messages, newMsg]);
      return { isCancelled: false, runStatus: 'running', messages: msgs };
    });
    scheduleAutoSave();
    return id;
  },

  removeMessages: (ids) => {
    const removed = new Set(ids);
    set((state) => ({
      messages: state.messages.filter((message) => !removed.has(message.id)),
      isStreaming: false,
      isThinking: false,
      activity: null,
    }));
    scheduleAutoSave();
  },

  startAssistantMessage: (idOverride) => {
    const id = idOverride || nextId();
    set((s) => ({
      messages: trimToMax([
        ...s.messages,
        {
          id,
          role: 'assistant',
          content: '',
          finalAnswerConfirmed: false,
          executionSteps: [],
          isStreaming: true,
          timestamp: Date.now(),
        },
      ]),
      isStreaming: true,
      isCancelled: false,
      runStatus: 'running',
      activity: null,
    }));
    return id;
  },

  continueAfterSteer: (assistantId, content, attachments) => {
    const nextAssistantId = nextId();
    set((s) => {
      const finalized = s.messages
        .map((message) =>
          message.id === assistantId ? { ...message, isStreaming: false } : message
        )
        .filter((message) => {
          if (message.id !== assistantId || message.role !== 'assistant') return true;
          return Boolean(
            message.content || message.assistantBlocks?.length || message.executionSteps?.length
          );
        });
      return {
        messages: trimToMax([
          ...finalized,
          {
            id: nextId(),
            role: 'user' as const,
            content,
            attachments,
            timestamp: Date.now(),
          },
          {
            id: nextAssistantId,
            role: 'assistant' as const,
            content: '',
            finalAnswerConfirmed: false,
            executionSteps: [],
            isStreaming: true,
            timestamp: Date.now(),
          },
        ]),
        isStreaming: true,
        isCancelled: false,
        runStatus: 'running' as const,
        activity: null,
      };
    });
    scheduleAutoSave();
    return nextAssistantId;
  },

  appendToken: (id, token) => {
    set((s) => {
      const idx = s.messages.findIndex((m) => m.id === id);
      if (idx === -1) return { messages: s.messages };
      const updated = [...s.messages];
      updated[idx] = { ...updated[idx], content: updated[idx].content + token };
      return { messages: updated };
    });
  },

  appendAssistantBlock: (id, block, executionId, publicationEventId) => {
    set((state) => ({
      messages: state.messages.map((message) => {
        if (message.id !== id || !message.isStreaming) return message;
        const blocks = [...(message.assistantBlocks ?? [])];
        const index = blocks.findIndex(
          (entry) =>
            (entry.execution_id ?? null) === (executionId ?? null) &&
            entry.block.request_id === block.request_id &&
            entry.block.block_id === block.block_id
        );
        if (index >= 0) {
          const previous = blocks[index];
          if (!previous || previous.block.complete || block.sequence < previous.block.sequence)
            return message;
          if (previous.block.phase !== 'unknown' && previous.block.phase !== block.phase)
            return message;
          blocks[index] = {
            block,
            content: previous.content + block.delta,
            execution_id: executionId ?? previous.execution_id,
            publication_event_id: publicationEventId ?? previous.publication_event_id,
          };
          return { ...message, assistantBlocks: blocks };
        }
        blocks.push({
          block,
          content: block.delta,
          execution_id: executionId,
          publication_event_id: publicationEventId,
        });
        return {
          ...message,
          assistantBlocks: blocks,
          executionSteps: [
            ...(message.executionSteps ?? []),
            { type: 'assistant' as const, index: blocks.length - 1 },
          ],
        };
      }),
    }));
  },

  setThinking: (v) => set({ isThinking: v, runStatus: v ? 'thinking' : get().runStatus }),

  recordToolStart: (messageId, toolExecutionId) => {
    set((s) => {
      const messages = s.messages.map((m) => {
        if (m.id !== messageId) return m;
        if (
          m.executionSteps?.some((step) => step.type === 'tool' && step.callId === toolExecutionId)
        ) {
          return m;
        }
        const executionSteps = [
          ...(m.executionSteps || []),
          { type: 'tool' as const, callId: toolExecutionId },
        ];
        return { ...m, executionSteps };
      });
      return { messages, runStatus: 'using_tool' };
    });
    scheduleAutoSave();
  },

  // 原生终态仅提供消息来源坐标，不更新 TaskRun/TurnStatus 或最终发布标记。
  observeExecutionStatus: (id, status) => {
    if (!['settled', 'failed', 'cancelled'].includes(status.status)) return;
    set((state) => ({
      messages: state.messages.map((message) =>
        message.id === id && !message.executionStatus
          ? { ...message, executionStatus: status }
          : message
      ),
    }));
  },

  // 只薄接后端已校验的呈现关联；不写正文、phase、完成标记或运行状态。
  applyFinalAnswerSource: (id, source) => {
    set((state) => ({
      messages: state.messages.map((message) => {
        const status = message.executionStatus;
        return message.id === id &&
          message.finalAnswerConfirmed &&
          status?.status === 'settled' &&
          message.finalAnswerExecutionId === source.execution_id &&
          status.execution_id === source.execution_id &&
          status.request_id === source.request_id &&
          status.sequence === source.sequence
          ? { ...message, finalAnswerSource: source }
          : message;
      }),
    }));
  },

  applyFinalAnswer: (id, content, executionId) => {
    if (!content.trim()) return;
    set((state) => ({
      messages: state.messages.map((message) =>
        message.id === id && !message.finalAnswerConfirmed
          ? { ...message, content, finalAnswerConfirmed: true, finalAnswerExecutionId: executionId }
          : message
      ),
    }));
    scheduleAutoSave();
  },

  settleAssistantMessage: (id) => {
    set((state) => ({
      isStreaming: false,
      isThinking: false,
      pendingHitlRequests: [],
      activity: null,
      messages: state.messages.map((message) =>
        message.id === id ? { ...message, isStreaming: false } : message
      ),
    }));
    scheduleAutoSave();
  },

  projectAssistantError: (id, error) => {
    set((s) => ({
      messages: s.messages.map((message) => {
        if (message.id !== id) return message;
        const errorNote = `[Error] ${error}`;
        return {
          ...message,
          content: message.content ? `${message.content}\n\n${errorNote}` : errorNote,
        };
      }),
    }));
    scheduleAutoSave();
  },

  handoffToTaskRuntime: (id, content, isRunning) => {
    set((s) => ({
      isStreaming: false,
      isThinking: false,
      runStatus: isRunning ? 'running' : 'waiting_approval',
      activity: null,
      messages: s.messages.map((m) => (m.id === id ? { ...m, content, isStreaming: false } : m)),
    }));
    scheduleAutoSave();
  },

  appendLocalAssistantNote: (content) => {
    set((s) => ({
      messages: trimToMax([
        ...s.messages,
        {
          id: nextId(),
          role: 'assistant',
          content,
          finalAnswerConfirmed: false,
          isStreaming: false,
          timestamp: Date.now(),
        },
      ]),
    }));
    scheduleAutoSave();
  },

  setStreaming: (v) => set((state) => ({ isStreaming: v, activity: v ? state.activity : null })),
  setRunStatus: (status) =>
    set((state) => ({
      runStatus: status,
      activity: ['idle', 'completed', 'failed', 'cancelled'].includes(status)
        ? null
        : state.activity,
      isStreaming: !['idle', 'completed', 'failed', 'cancelled'].includes(status),
      isCancelled: status === 'cancelled',
      isThinking: status === 'thinking',
      pendingHitlRequests: ['completed', 'failed', 'cancelled'].includes(status)
        ? []
        : state.pendingHitlRequests,
    })),

  clearInactiveTurnProjection: () => {
    set((state) => ({
      isStreaming: false,
      isThinking: false,
      isCancelled: false,
      runStatus: 'idle',
      activity: null,
      pendingHitlRequests: [],
      messages: state.messages.map((message) =>
        message.isStreaming ? { ...message, isStreaming: false } : message
      ),
    }));
    scheduleAutoSave();
  },

  enqueueHitlRequest: (request) =>
    set((state) => {
      if (
        !isHitlRequestIdentity(request.identity) ||
        request.identity.request_id !== request.requestId ||
        request.identity.kind !== request.kind
      )
        return state;
      if (
        state.pendingHitlRequests.some((pending) =>
          sameHitlIdentity(pending.identity, request.identity)
        )
      ) {
        return state;
      }
      const pendingHitlRequests = [...state.pendingHitlRequests, request];
      const front = pendingHitlRequests.at(0);
      return {
        pendingHitlRequests,
        runStatus: front ? waitingStatus(front) : state.runStatus,
      };
    }),
  removeHitlRequest: (identity) =>
    set((state) => {
      if (!isHitlRequestIdentity(identity)) return state;
      const pendingHitlRequests = state.pendingHitlRequests.filter(
        (request) => !sameHitlIdentity(request.identity, identity)
      );
      if (pendingHitlRequests.length === state.pendingHitlRequests.length) {
        return state;
      }
      const front = pendingHitlRequests.at(0);
      return {
        pendingHitlRequests,
        runStatus: front ? waitingStatus(front) : 'running',
      };
    }),
  clearHitlRequests: () => set({ pendingHitlRequests: [] }),

  addChartMessage: (spec) => {
    set((s) => ({
      messages: s.messages.map((m) =>
        m.isStreaming ? { ...m, chartSpecs: [...(m.chartSpecs || []), spec] } : m
      ),
    }));
  },

  clearMessages: () =>
    set({
      messages: [],
      agentInputs: [],
      agentInputsBinding: null,
      agentInputReadError: null,
      isStreaming: false,
      isCancelled: false,
      isThinking: false,
      isHistoryView: false,
      pendingHitlRequests: [],
      contextWindow: null,
      usageAccumulator: { totalInput: 0, totalCached: 0 },
      runStatus: 'idle',
      activity: null,
    }),

  replaceMessages: (messages) =>
    set({
      messages: trimToMax(messages),
      isStreaming: false,
      isCancelled: false,
      isHistoryView: true,
      pendingHitlRequests: [],
      // 加载历史会话时清空实时上下文占用（历史视图无 live llm_usage，显示旧值会误导）。
      contextWindow: null,
      usageAccumulator: { totalInput: 0, totalCached: 0 },
      runStatus: 'idle',
      activity: null,
    }),

  setHistoryView: (v) => set({ isHistoryView: v }),

  setContextWindow: (usage) => set({ contextWindow: usage }),

  clearContextWindow: () => set({ contextWindow: null }),

  recordUsage: (input, cached) =>
    set((s) => ({
      usageAccumulator: {
        totalInput: s.usageAccumulator.totalInput + input,
        totalCached: s.usageAccumulator.totalCached + cached,
      },
    })),
}));
