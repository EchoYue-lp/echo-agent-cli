import { useRef, useCallback, useEffect } from 'react';
import { useChatStore } from '../stores/chatStore';
import { useConversationStore } from '../stores/conversationStore';
import { useSubagentStore, type SubagentEventPayload } from '../stores/subagentStore';
import { useWorkerTraceStore, type WorkerTraceEvent } from '../stores/workerTraceStore';
import { isTauri, apiInvoke, errorMessage } from '../lib/tauri-bridge';
import { handleChatEvent } from './chatEventHandler';
import type { Attachment, ChatRunStatus } from '../types/api';

type ChatEventBase = {
  message_key?: string;
  conversation_id?: string | null;
};

type ChatEvent = ChatEventBase &
  (
  | { type: 'token'; data: string }
  | { type: 'thinking_start' }
  | { type: 'thinking_end'; prompt_tokens: number; completion_tokens: number }
  | { type: 'tool_start'; name: string; args: unknown }
  | { type: 'tool_result'; name: string; result: string; success: boolean }
  | { type: 'chart'; spec: unknown }
  | { type: 'final_answer'; data: string }
  | { type: 'cancelled' }
  | { type: 'error'; message: string }
  | { type: 'run_status'; status: ChatRunStatus }
  | {
      type: 'approval_request';
      request_id: string;
      tool_name: string;
      args: unknown;
      prompt: string;
    }
  | { type: 'input_request'; request_id: string; prompt: string }
  | {
      type: 'selection_request';
      request_id: string;
      prompt: string;
      options: string[];
      task_id?: string | null;
      context?: unknown;
      phase?: string | null;
    }
  | { type: 'tool_batch_start'; tool_count: number }
  | { type: 'tool_batch_end' }
  | {
      type: 'plan_ready';
      run_id: string;
      goal: string;
      domain_profile: string;
      route: string;
      interaction_mode: string;
      permission_mode: string;
      approval_policy: string;
      route_reason: string;
      confidence: number;
      auto_execute: boolean;
      planned_workers: string[];
      suggested_workers: string[];
      route_signals: string[];
      classification_signals: string[];
    }
  | { type: 'done' }
  );

export function useTauriChat() {
  const assistantIdRef = useRef<string | null>(null);
  const isCancelledRef = useRef(false);
  const unlistenRef = useRef<(() => void) | null>(null);
  const currentMessageKeyRef = useRef<string | null>(null);
  const currentConversationIdRef = useRef<string | null>(null);
  const thinkingIdRef = useRef<string | null>(null);

  const isCurrentRunEvent = (event: ChatEvent) => {
    if (event.message_key) {
      return currentMessageKeyRef.current === event.message_key;
    }
    if (event.conversation_id) {
      return currentConversationIdRef.current === event.conversation_id;
    }
    return true;
  };

  const handleEvent = useCallback((event: ChatEvent) => {
    if (!isCurrentRunEvent(event)) return;
    handleChatEvent(event as any, {
      assistantIdRef,
      currentMessageKeyRef,
      currentMessageIdRef: currentMessageKeyRef,
      isCancelledRef,
      currentThinkingIdRef: thinkingIdRef,
    });
  }, []);

  // Set up event listener on mount
  useEffect(() => {
    if (!isTauri()) return;

    let mounted = true;

    const setupListener = async () => {
      const { listen } = await import('@tauri-apps/api/event');
      const unlisten = await listen<ChatEvent>('chat://event', (event) => {
        if (mounted) {
          handleEvent(event.payload);
        }
      });
      // Subagent lifecycle events (Phase 5: Subagent visualization)
      const unlistenSub = await listen<SubagentEventPayload>('subagent://event', (event) => {
        if (mounted) {
          useSubagentStore.getState().upsert(event.payload);
        }
      });
      const unlistenWorkerTrace = await listen<WorkerTraceEvent>('worker://trace', (event) => {
        if (mounted) {
          useWorkerTraceStore.getState().append(event.payload);
        }
      });
      const origUnlisten = unlisten;
      unlistenRef.current = () => {
        origUnlisten();
        unlistenSub();
        unlistenWorkerTrace();
      };
    };

    setupListener();

    return () => {
      mounted = false;
      unlistenRef.current?.();
      unlistenRef.current = null;
    };
  }, [handleEvent]);

  const sendMessage = useCallback(async (text: string, attachments?: Attachment[]) => {
    const store = useChatStore.getState();
    const displayAttachments = attachments?.map((a) => ({
      name: a.name,
      mime_type: a.mime_type,
      url: `data:${a.mime_type};base64,${a.data}`,
      size: a.size,
    }));
    store.addUserMessage(text || '(附件)', displayAttachments);

    try {
      isCancelledRef.current = false;
      thinkingIdRef.current = null;
      assistantIdRef.current = store.startAssistantMessage();

      // TaskRuntime runs are keyed by conversation_id. On the first turn there
      // is no active conversation yet, so create it before routing; otherwise
      // the backend falls back to a message-scoped id and the right rail loses
      // the run as soon as the conversation is later saved as conv-*.
      if (!useConversationStore.getState().activeId) {
        await useConversationStore.getState().saveCurrent(useChatStore.getState().messages);
      }

      // Pass conversation_id for pool-based parallel execution and TaskRuntime
      // run binding.
      const conversation_id = useConversationStore.getState().activeId;
      if (!conversation_id) {
        throw new Error('创建会话失败，无法启动 TaskRuntime。');
      }
      const message_key =
        typeof crypto !== 'undefined' && 'randomUUID' in crypto
          ? crypto.randomUUID()
          : `chat-${Date.now()}-${Math.random().toString(36).slice(2)}`;
      currentMessageKeyRef.current = message_key;
      currentConversationIdRef.current = conversation_id ?? null;
      await apiInvoke('send_chat_message', {
        message: text,
        conversationId: conversation_id ?? undefined,
        conversation_id: conversation_id ?? undefined,
        messageKey: message_key,
        message_key,
      });
    } catch (e) {
      console.error('[TauriChat] Failed to send message:', e);
      if (assistantIdRef.current) {
        store.finalizeAssistantMessage(assistantIdRef.current, `[Error] ${errorMessage(e)}`);
      }
      store.setRunStatus('failed');
      assistantIdRef.current = null;
      currentMessageKeyRef.current = null;
    }
  }, []);

  const sendApproval = useCallback(
    async (requestId: string, approved: boolean, reason?: string, scope?: string) => {
      try {
        await apiInvoke('send_approval_response', {
          requestId,
          request_id: requestId,
          approved,
          reason,
          scope,
        });
        useChatStore.getState().setApprovalRequest(null);
        useChatStore.getState().setRunStatus('running');
      } catch (e) {
        console.error('[TauriChat] Failed to send approval:', e);
        throw e;
      }
    },
    []
  );

  const sendInput = useCallback(async (requestId: string, text: string) => {
    try {
      await apiInvoke('send_input_response', { requestId, request_id: requestId, text });
      useChatStore.getState().setInputRequest(null);
      useChatStore.getState().setRunStatus('running');
    } catch (e) {
      console.error('[TauriChat] Failed to send input:', e);
    }
  }, []);

  const sendSelection = useCallback(
    async (requestId: string, selection: string, instructions?: string) => {
      try {
        await apiInvoke('send_selection_response', {
          requestId,
          request_id: requestId,
          selection,
          instructions,
        });
        useChatStore.getState().setSelectionRequest(null);
        useChatStore.getState().setRunStatus('running');
      } catch (e) {
        console.error('[TauriChat] Failed to send selection:', e);
      }
    },
    []
  );

  const cancel = useCallback(async () => {
    const messageKey = currentMessageKeyRef.current;
    useChatStore.getState().markCancelled();
    assistantIdRef.current = null;
    isCancelledRef.current = true;
    try {
      await apiInvoke('cancel_chat', {
        messageKey: messageKey ?? undefined,
        message_key: messageKey ?? undefined,
      });
    } catch (e) {
      console.error('[TauriChat] Failed to cancel:', e);
    } finally {
      currentMessageKeyRef.current = null;
    }
  }, []);

  return {
    sendMessage,
    sendApproval,
    sendInput,
    sendSelection,
    cancel,
    reconnect: () => {}, // Tauri IPC is always connected; no-op for API compatibility
    connectionStatus: 'connected' as const,
  };
}
