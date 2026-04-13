import { create } from 'zustand';
import type { ChatMessage, ApprovalRequest, ToolCallInfo } from '../types/api';

interface ChatState {
  messages: ChatMessage[];
  isStreaming: boolean;
  isCancelled: boolean;
  approvalRequest: ApprovalRequest | null;
  inputRequest: { requestId: string; prompt?: string } | null;
  pendingToolCall: { name: string; args: unknown } | null;
  /** True when viewing a loaded historical conversation (agent has no context) */
  isHistoryView: boolean;

  addUserMessage: (content: string) => void;
  startAssistantMessage: () => string;
  appendToken: (id: string, token: string) => void;
  setToolCall: (name: string, args: unknown) => void;
  completeToolCall: (name: string, result: string, success: boolean) => void;
  finalizeAssistantMessage: (id: string, content: string) => void;
  setStreaming: (v: boolean) => void;
  markCancelled: () => void;
  setApprovalRequest: (r: ApprovalRequest | null) => void;
  setInputRequest: (r: { requestId: string; prompt?: string } | null) => void;
  clearMessages: () => void;
  replaceMessages: (messages: ChatMessage[]) => void;
  setCurrentSnapshot: (id: string | null) => void;
  setHistoryView: (v: boolean) => void;
  /** Delete last assistant message, return last user message content for resend */
  prepareRegenerate: () => string | null;
  /** Edit a user message, delete all messages after it, return new content for resend */
  prepareEditAndResend: (messageId: string, newContent: string) => string | null;
}

let msgCounter = 0;
const nextId = () => `msg-${++msgCounter}-${Date.now()}`;

/** Auto-save to conversationStore after state changes that add messages */
function autoSave() {
  // Defer to next tick so store has updated
  setTimeout(() => {
    import('./conversationStore').then(({ useConversationStore }) => {
      const msgs = useChatStore.getState().messages;
      if (msgs.length > 0) {
        useConversationStore.getState().saveCurrent(msgs);
      }
    });
  }, 100);
}

export const useChatStore = create<ChatState>((set, get) => ({
  messages: [],
  isStreaming: false,
  isCancelled: false,
  approvalRequest: null,
  inputRequest: null,
  pendingToolCall: null,
  isHistoryView: false,

  addUserMessage: (content) => {
    set((s) => ({
      isCancelled: false,
      messages: [...s.messages, { id: nextId(), role: 'user', content, timestamp: Date.now() }],
    }));
    autoSave();
  },

  startAssistantMessage: () => {
    const id = nextId();
    set((s) => ({
      messages: [...s.messages, { id, role: 'assistant', content: '', toolCalls: [], isStreaming: true, timestamp: Date.now() }],
      isStreaming: true,
    }));
    return id;
  },

  appendToken: (id, token) => {
    set((s) => ({
      messages: s.messages.map((m) =>
        m.id === id ? { ...m, content: m.content + token } : m
      ),
    }));
  },

  setToolCall: (name, args) => {
    set({ pendingToolCall: { name, args } });
  },

  completeToolCall: (name, result, success) => {
    const { pendingToolCall } = get();
    if (!pendingToolCall) return;
    const tc: ToolCallInfo = { name, args: pendingToolCall.args, result, success };
    set((s) => ({
      pendingToolCall: null,
      messages: s.messages.map((m) =>
        m.isStreaming ? { ...m, toolCalls: [...(m.toolCalls || []), tc] } : m
      ),
    }));
  },

  finalizeAssistantMessage: (id, content) => {
    set((s) => ({
      isStreaming: false,
      messages: s.messages.map((m) =>
        m.id === id ? { ...m, content, isStreaming: false } : m
      ),
    }));
    autoSave();
  },

  setStreaming: (v) => set({ isStreaming: v }),

  markCancelled: () => {
    set((s) => ({
      isStreaming: false,
      isCancelled: true,
      pendingToolCall: null,
      messages: s.messages.map((m) =>
        m.isStreaming ? { ...m, isStreaming: false, content: m.content || '' } : m
      ),
    }));
    autoSave();
  },

  setApprovalRequest: (r) => set({ approvalRequest: r }),
  setInputRequest: (r) => set({ inputRequest: r }),

  clearMessages: () => set({ messages: [], isStreaming: false, isCancelled: false, isHistoryView: false }),

  replaceMessages: (messages) => set({ messages, isStreaming: false, isCancelled: false, isHistoryView: true }),

  setCurrentSnapshot: (_id) => {
    // Kept for compatibility - actual ID tracking is in conversationStore
  },

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
    return newContent;
  },
}));
