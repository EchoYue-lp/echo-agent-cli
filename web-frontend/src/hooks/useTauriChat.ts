import { useRef, useCallback, useEffect } from 'react';
import { useChatStore } from '../stores/chatStore';
import { useConversationStore } from '../stores/conversationStore';
import { useTaskRuntimeStore } from '../stores/taskRuntimeStore';
import { isTauri, apiInvoke } from '../lib/tauri-bridge';
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
      signals: string[];
    }
  | { type: 'done' }
  );

export function useTauriChat() {
  const assistantIdRef = useRef<string | null>(null);
  const inThinkingRef = useRef(false);
  const isCancelledRef = useRef(false);
  const unlistenRef = useRef<(() => void) | null>(null);
  const currentMessageKeyRef = useRef<string | null>(null);
  const currentConversationIdRef = useRef<string | null>(null);

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
    const store = useChatStore.getState();

    switch (event.type) {
      case 'token': {
        if (isCancelledRef.current) break;
        if (!assistantIdRef.current) {
          assistantIdRef.current = store.startAssistantMessage();
        }
        if (!inThinkingRef.current) {
          inThinkingRef.current = true;
          store.setThinking(true);
          store.startThinkingSegment(assistantIdRef.current);
        }
        store.appendThinking(assistantIdRef.current, event.data);
        break;
      }
      case 'thinking_start':
        if (isCancelledRef.current) break;
        inThinkingRef.current = true;
        store.setThinking(true);
        if (!assistantIdRef.current) {
          assistantIdRef.current = store.startAssistantMessage();
        }
        store.startThinkingSegment(assistantIdRef.current);
        break;
      case 'thinking_end':
        inThinkingRef.current = false;
        store.setThinking(false);
        break;
      case 'tool_start':
        if (isCancelledRef.current) break;
        inThinkingRef.current = false;
        store.setThinking(false);
        store.setRunStatus('using_tool');
        store.setToolCall(event.name, event.args);
        break;
      case 'tool_result':
        if (isCancelledRef.current) break;
        store.completeToolCall(event.name, event.result, event.success);
        store.setRunStatus('running');
        break;
      case 'tool_batch_start':
        if (isCancelledRef.current) break;
        store.startToolBatch((event as any).tool_count || 0);
        break;
      case 'tool_batch_end':
        if (isCancelledRef.current) break;
        store.endToolBatch();
        break;
      case 'final_answer': {
        if (!isCancelledRef.current && assistantIdRef.current) {
          store.finalizeAssistantMessage(assistantIdRef.current, event.data);
        }
        inThinkingRef.current = false;
        assistantIdRef.current = null;
        currentMessageKeyRef.current = null;
        isCancelledRef.current = false;
        break;
      }
      case 'approval_request':
        if (isCancelledRef.current) break;
        store.setApprovalRequest({
          requestId: event.request_id,
          toolName: event.tool_name,
          args: event.args,
          prompt: event.prompt,
        });
        break;
      case 'input_request':
        if (isCancelledRef.current) break;
        store.setInputRequest({ requestId: event.request_id, prompt: event.prompt });
        break;
      case 'selection_request':
        if (isCancelledRef.current) break;
        store.setSelectionRequest({
          requestId: event.request_id,
          prompt: event.prompt,
          options: event.options,
          taskId: event.task_id ?? undefined,
          context: event.context,
          phase: event.phase ?? undefined,
        });
        break;
      case 'chart':
        if (isCancelledRef.current) break;
        store.addChartMessage(event.spec);
        break;
      case 'error': {
        if (!isCancelledRef.current && assistantIdRef.current) {
          store.finalizeAssistantMessage(assistantIdRef.current, `[Error] ${event.message}`);
        }
        store.setRunStatus('failed');
        assistantIdRef.current = null;
        currentMessageKeyRef.current = null;
        isCancelledRef.current = false;
        break;
      }
      case 'run_status':
        store.setRunStatus(event.status);
        break;
      case 'cancelled':
        store.setRunStatus('cancelled');
        assistantIdRef.current = null;
        currentMessageKeyRef.current = null;
        isCancelledRef.current = false;
        break;
      case 'done':
        // If no final_answer was received, finalize with empty content
        if (assistantIdRef.current && !isCancelledRef.current) {
          store.finalizeAssistantMessage(assistantIdRef.current, '');
        }
        assistantIdRef.current = null;
        currentMessageKeyRef.current = null;
        isCancelledRef.current = false;
        break;
      case 'plan_ready': {
        // A complex input was routed to the TaskRuntime and a plan was
        // generated. Hand it to the TaskRuntime store so the right-rail
        // panel renders the plan + approval actions.
        const runId = (event as { run_id: string }).run_id;
        useTaskRuntimeStore.getState().notifyPlanReady(runId);
        break;
      }
    }
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
      unlistenRef.current = unlisten;
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
      assistantIdRef.current = store.startAssistantMessage();

      // Pass conversation_id for pool-based parallel execution
      const conversation_id = useConversationStore.getState().activeId;
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
        store.finalizeAssistantMessage(assistantIdRef.current, `[Error] ${e}`);
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
    connectionStatus: 'connected' as const,
  };
}
