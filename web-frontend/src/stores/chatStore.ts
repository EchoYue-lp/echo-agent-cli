import { create } from 'zustand';
import type {
  ChatMessage,
  ApprovalRequest,
  ToolCallInfo,
  ExecutionRound,
  ChatRunStatus,
} from '../types/api';
import { useConversationStore } from './conversationStore';

/** In-progress round being built during streaming */
interface CurrentRound {
  thinking?: { content: string };
  tools: { name: string; args: unknown; result?: string; success?: boolean }[];
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
  pendingToolCalls: { name: string; args: unknown }[];
  /** True when viewing a loaded historical conversation (agent has no context) */
  isHistoryView: boolean;
  /** Current round being built during streaming */
  currentRound: CurrentRound | null;

  addUserMessage: (content: string, attachments?: ChatMessage['attachments']) => void;
  startAssistantMessage: (idOverride?: string) => string;
  appendToken: (id: string, token: string) => void;
  appendThinking: (id: string, token: string) => void;
  startThinkingSegment: (id: string) => void;
  setToolCall: (name: string, args: unknown) => void;
  completeToolCall: (name: string, result: string, success: boolean) => void;
  startToolBatch: (toolCount: number) => void;
  endToolBatch: () => void;
  finalizeAssistantMessage: (id: string, content: string) => void;
  handoffToTaskRuntime: (id: string, content: string, isRunning: boolean) => void;
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
  pendingToolCalls: [],
  isHistoryView: false,
  currentRound: null,

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
          toolCalls: [],
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

  setToolCall: (name, args) => {
    set((s) => {
      const pendingToolCalls = [...s.pendingToolCalls, { name, args }];
      // Record execution step NOW (at tool_start), not at tool_result time.
      // This ensures correct chronological interleaving with thinking segments.
      const messages = s.messages.map((m) => {
        if (!m.isStreaming) return m;
        // Index = completed tool calls + currently pending (before this one)
        const toolIndex = (m.toolCalls || []).length + s.pendingToolCalls.length;
        const executionSteps = [
          ...(m.executionSteps || []),
          { type: 'tool' as const, index: toolIndex },
        ];
        return { ...m, executionSteps };
      });
      // Also add tool to current round for round-based rendering
      const currentRound = s.currentRound
        ? { ...s.currentRound, tools: [...s.currentRound.tools, { name, args }] }
        : { tools: [{ name, args }] };
      return { pendingToolCalls, messages, currentRound, runStatus: 'using_tool' };
    });
  },

  completeToolCall: (name, result, success) => {
    const { pendingToolCalls } = get();
    // 按顺序匹配：取第一个 pending tool call（FIFO）
    const idx = pendingToolCalls.findIndex((tc) => tc.name === name);
    if (idx === -1) return;
    const matched = pendingToolCalls[idx];
    const tc: ToolCallInfo = { name, args: matched.args, result, success };
    set((s) => {
      // Update tool result in current round if exists
      const currentRound = s.currentRound
        ? {
            ...s.currentRound,
            tools: s.currentRound.tools.map((t) =>
              t.name === name && t.result === undefined ? { ...t, result, success } : t
            ),
          }
        : null;
      return {
        pendingToolCalls: s.pendingToolCalls.filter((_, i) => i !== idx),
        messages: s.messages.map((m) => {
          if (!m.isStreaming) return m;
          const toolCalls = [...(m.toolCalls || []), tc];
          // executionStep already recorded at setToolCall (tool_start) time — don't add again
          return { ...m, toolCalls };
        }),
        currentRound,
      };
    });
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
        tools: [],
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
      tools: currentRound.tools.map((t) => ({
        name: t.name,
        args: t.args,
        result: t.result || '',
        success: t.success ?? true,
      })),
    };
    set((s) => ({
      currentRound: null,
      messages: s.messages.map((m) => {
        if (!m.isStreaming) return m;
        const executionRounds = [...(m.executionRounds || []), round];
        return { ...m, executionRounds };
      }),
    }));
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
      pendingToolCalls: [],
      messages: s.messages.map((m) =>
        m.isStreaming ? { ...m, isStreaming: false, content: m.content || '' } : m
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
      pendingToolCalls: [],
      currentRound: null,
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
      runStatus: 'idle',
    }),

  setHistoryView: (v) => set({ isHistoryView: v }),

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
