import { create } from 'zustand';
import type { ChatMessage, ApprovalRequest, ExecutionRound, ChatRunStatus } from '../types/api';
import { useConversationStore } from './conversationStore';

/** In-progress round being built during streaming */
interface CurrentRound {
  thinking?: { content: string };
  toolCallIds: string[];
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

export function cacheHitRate(acc: ContextUsageAccumulator): number | null {
  if (acc.totalInput <= 0) return null;
  return acc.totalCached / acc.totalInput;
}

interface ChatState {
  messages: ChatMessage[];
  isStreaming: boolean;
  isCancelled: boolean;
  isThinking: boolean;
  runStatus: ChatRunStatus;
  approvalRequest: ApprovalRequest | null;
  inputRequest: { requestId: string; prompt?: string } | null;
  selectionRequest: {
    requestId: string;
    prompt: string;
    options: string[];
    taskId?: string;
    context?: unknown;
    phase?: string;
  } | null;
  /** True when viewing a loaded historical conversation (agent has no context) */
  isHistoryView: boolean;
  /** Current round being built during streaming */
  currentRound: CurrentRound | null;
  /** 当前上下文窗口占用（来自最近一次 llm_usage 事件的真实 prompt_tokens）。null = 首次响应前 / 刚压缩后。 */
  contextWindow: ContextWindowUsage | null;
  /** 会话级缓存命中率累加器（压缩保留，clear/replace 清零）。 */
  usageAccumulator: ContextUsageAccumulator;

  addUserMessage: (content: string, attachments?: ChatMessage['attachments']) => void;
  startAssistantMessage: (idOverride?: string) => string;
  continueAfterSteer: (
    assistantId: string | null,
    content: string,
    attachments?: ChatMessage['attachments']
  ) => string;
  appendToken: (id: string, token: string) => void;
  appendThinking: (id: string, token: string) => void;
  startThinkingSegment: (id: string) => void;
  recordToolStart: (messageId: string, toolExecutionId: string) => void;
  startToolBatch: (toolCount: number) => void;
  endToolBatch: () => void;
  finalizeAssistantMessage: (id: string, content: string) => void;
  handoffToTaskRuntime: (id: string, content: string, isRunning: boolean) => void;
  /** Insert a non-streaming assistant note (e.g. background subagent finished). */
  appendLocalAssistantNote: (content: string) => void;
  setStreaming: (v: boolean) => void;
  setThinking: (v: boolean) => void;
  setRunStatus: (status: ChatRunStatus) => void;
  markCancelled: () => void;
  setApprovalRequest: (r: ApprovalRequest | null) => void;
  setInputRequest: (r: { requestId: string; prompt?: string } | null) => void;
  setSelectionRequest: (r: ChatState['selectionRequest']) => void;
  addChartMessage: (spec: unknown) => void;
  clearMessages: () => void;
  replaceMessages: (messages: ChatMessage[]) => void;
  setHistoryView: (v: boolean) => void;
  /** Delete last assistant message, return last user message content for resend */
  prepareRegenerate: () => string | null;
  /** Edit a user message, delete all messages after it, return new content for resend */
  prepareEditAndResend: (messageId: string, newContent: string) => string | null;
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
  const msgs = useChatStore.getState().messages;
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
  messages: [],
  isStreaming: false,
  isCancelled: false,
  isThinking: false,
  runStatus: 'idle',
  approvalRequest: null,
  inputRequest: null,
  selectionRequest: null,
  isHistoryView: false,
  currentRound: null,
  contextWindow: null,
  usageAccumulator: { totalInput: 0, totalCached: 0 },

  addUserMessage: (content, attachments) => {
    set((s) => {
      const newMsg: ChatMessage = {
        id: nextId(),
        role: 'user',
        content,
        attachments,
        timestamp: Date.now(),
      };
      const msgs = trimToMax([...s.messages, newMsg]);
      return { isCancelled: false, runStatus: 'running', messages: msgs };
    });
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
          thinkingSegments: [],
          executionSteps: [],
          executionRounds: [],
          isStreaming: true,
          timestamp: Date.now(),
        },
      ]),
      isStreaming: true,
      isCancelled: false,
      runStatus: 'running',
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
            message.content || message.thinkingSegments?.length || message.executionSteps?.length
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
            thinkingSegments: [],
            executionSteps: [],
            executionRounds: [],
            isStreaming: true,
            timestamp: Date.now(),
          },
        ]),
        isStreaming: true,
        isCancelled: false,
        runStatus: 'running' as const,
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

  appendThinking: (id, token) => {
    set((s) => {
      const idx = s.messages.findIndex((m) => m.id === id);
      if (idx === -1) return { messages: s.messages };
      const m = s.messages[idx];
      const segments = [...(m.thinkingSegments || [])];
      if (segments.length === 0) {
        segments.push({ content: token });
      } else {
        const last = { ...segments[segments.length - 1] };
        segments[segments.length - 1] = { ...last, content: last.content + token };
      }
      const updated = [...s.messages];
      updated[idx] = { ...m, thinkingSegments: segments };
      return { messages: updated };
    });
  },

  startThinkingSegment: (id) => {
    set((s) => {
      const messages = s.messages.map((m) => {
        if (m.id !== id) return m;
        const thinkingSegments = [...(m.thinkingSegments || []), { content: '' }];
        const executionSteps = [
          ...(m.executionSteps || []),
          { type: 'thinking' as const, index: thinkingSegments.length - 1 },
        ];
        return { ...m, thinkingSegments, executionSteps };
      });
      // If there's an in-progress round, it's now complete (thinking→tools→done),
      // so start a fresh round for this new thinking phase.
      return { messages };
    });
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
      const currentRound = s.currentRound
        ? {
            ...s.currentRound,
            toolCallIds: s.currentRound.toolCallIds.includes(toolExecutionId)
              ? s.currentRound.toolCallIds
              : [...s.currentRound.toolCallIds, toolExecutionId],
          }
        : { toolCallIds: [toolExecutionId] };
      return { messages, currentRound, runStatus: 'using_tool' };
    });
    scheduleAutoSave();
  },

  /** Start a new tool batch (received tool_batch_start event from backend) */
  startToolBatch: (_toolCount: number) => {
    const state = get();
    // Capture the last thinking segment from the streaming message.
    // During streaming, thinking content is accumulated on message.thinkingSegments,
    // not on currentRound. When a tool batch starts, we must explicitly associate
    // the preceding thinking with the upcoming tools to form a complete ExecutionRound.
    const streamingMsg = state.messages.find((m) => m.isStreaming);
    const segments = streamingMsg?.thinkingSegments;
    const lastSegment = segments && segments.length > 0 ? segments[segments.length - 1] : null;
    set({
      currentRound: {
        thinking: lastSegment?.content ? { content: lastSegment.content } : undefined,
        toolCallIds: [],
      },
    });
  },

  /** End current tool batch (received tool_batch_end event from backend).
   *  Push the current round into the streaming message's executionRounds. */
  endToolBatch: () => {
    const { currentRound } = get();
    if (!currentRound) return;
    const round: ExecutionRound = {
      thinking: currentRound.thinking,
      toolCallIds: [...currentRound.toolCallIds],
    };
    set((s) => ({
      currentRound: null,
      messages: s.messages.map((m) => {
        if (!m.isStreaming) return m;
        const executionRounds = [...(m.executionRounds || []), round];
        return { ...m, executionRounds };
      }),
    }));
    scheduleAutoSave();
  },

  finalizeAssistantMessage: (id, content) => {
    set((s) => ({
      isStreaming: false,
      isThinking: false,
      runStatus: 'completed',
      messages: s.messages.map((m) => (m.id === id ? { ...m, content, isStreaming: false } : m)),
    }));
    scheduleAutoSave();
  },

  handoffToTaskRuntime: (id, content, isRunning) => {
    set((s) => ({
      isStreaming: false,
      isThinking: false,
      runStatus: isRunning ? 'running' : 'waiting_approval',
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
          isStreaming: false,
          timestamp: Date.now(),
        },
      ]),
    }));
    scheduleAutoSave();
  },

  setStreaming: (v) => set({ isStreaming: v }),
  setRunStatus: (status) =>
    set({
      runStatus: status,
      isStreaming: !['idle', 'completed', 'failed', 'cancelled'].includes(status),
      isCancelled: status === 'cancelled',
      isThinking: status === 'thinking',
    }),

  markCancelled: () => {
    set((s) => ({
      isStreaming: false,
      isCancelled: true,
      isThinking: false,
      runStatus: 'cancelled',
      messages: s.messages.map((m) =>
        m.isStreaming
          ? {
              ...m,
              isStreaming: false,
              content: m.content || '',
            }
          : m
      ),
    }));
    scheduleAutoSave();
  },

  setApprovalRequest: (r) =>
    set({ approvalRequest: r, runStatus: r ? 'waiting_approval' : get().runStatus }),
  setInputRequest: (r) =>
    set({ inputRequest: r, runStatus: r ? 'waiting_input' : get().runStatus }),
  setSelectionRequest: (r) =>
    set({ selectionRequest: r, runStatus: r ? 'waiting_input' : get().runStatus }),

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
      isStreaming: false,
      isCancelled: false,
      isThinking: false,
      isHistoryView: false,
      approvalRequest: null,
      inputRequest: null,
      selectionRequest: null,
      currentRound: null,
      contextWindow: null,
      usageAccumulator: { totalInput: 0, totalCached: 0 },
      runStatus: 'idle',
    }),

  replaceMessages: (messages) =>
    set({
      messages: trimToMax(messages),
      isStreaming: false,
      isCancelled: false,
      isHistoryView: true,
      approvalRequest: null,
      inputRequest: null,
      selectionRequest: null,
      // 加载历史会话时清空实时上下文占用（历史视图无 live llm_usage，显示旧值会误导）。
      contextWindow: null,
      usageAccumulator: { totalInput: 0, totalCached: 0 },
      runStatus: 'idle',
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

  prepareRegenerate: () => {
    const { messages } = get();
    if (messages.length < 2) return null;
    // Find the last assistant message and remove it
    let lastAssistantIdx = -1;
    for (let i = messages.length - 1; i >= 0; i--) {
      if (messages[i].role === 'assistant' && !messages[i].isStreaming) {
        lastAssistantIdx = i;
        break;
      }
    }
    if (lastAssistantIdx === -1) return null;

    // Find the last user message before this assistant message
    let lastUserIdx = -1;
    for (let i = lastAssistantIdx - 1; i >= 0; i--) {
      if (messages[i].role === 'user') {
        lastUserIdx = i;
        break;
      }
    }
    if (lastUserIdx === -1) return null;

    const userContent = messages[lastUserIdx].content;

    // Remove everything from the last user message onwards
    set({ messages: messages.slice(0, lastUserIdx) });
    // P1-11: 这两个 prepare* 方法改了 messages 后未保存, 刷新/崩溃会丢失修改。
    scheduleAutoSave();
    return userContent;
  },

  prepareEditAndResend: (messageId: string, newContent: string) => {
    const { messages } = get();
    const idx = messages.findIndex((m) => m.id === messageId);
    if (idx === -1) return null;

    // Update the message content and remove all messages after it
    const updated = messages.slice(0, idx);
    updated.push({
      ...messages[idx],
      content: newContent,
      timestamp: Date.now(),
    });
    set({ messages: updated });
    // P1-11: 同 prepareRegenerate, 改完要保存。
    scheduleAutoSave();
    return newContent;
  },
}));
