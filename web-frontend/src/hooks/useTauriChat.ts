import { useRef, useCallback, useEffect } from 'react';
import { useChatStore } from '../stores/chatStore';
import { useConversationStore } from '../stores/conversationStore';
import { isTauri, apiInvoke } from '../lib/tauri-bridge';
import type { Attachment } from '../types/api';

type ChatEvent =
  | { type: 'token'; data: string }
  | { type: 'thinking_start' }
  | { type: 'thinking_end'; prompt_tokens: number; completion_tokens: number }
  | { type: 'tool_start'; name: string; args: unknown }
  | { type: 'tool_result'; name: string; result: string; success: boolean }
  | { type: 'chart'; spec: unknown }
  | { type: 'final_answer'; data: string }
  | { type: 'cancelled' }
  | { type: 'error'; message: string }
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
  | { type: 'done' };

export function useTauriChat() {
  const assistantIdRef = useRef<string | null>(null);
  const inThinkingRef = useRef(false);
  const isCancelledRef = useRef(false);
  const unlistenRef = useRef<(() => void) | null>(null);
  const streamTimer = useRef<ReturnType<typeof setTimeout>>(undefined);

  const clearStreamTimer = () => {
    if (streamTimer.current) {
      clearTimeout(streamTimer.current);
      streamTimer.current = undefined;
    }
  };

  const handleEvent = useCallback((event: ChatEvent) => {
    const store = useChatStore.getState();

    switch (event.type) {
      case 'token': {
        clearStreamTimer();
        if (isCancelledRef.current) break;
        if (!assistantIdRef.current) {
          assistantIdRef.current = store.startAssistantMessage();
        }
        if (inThinkingRef.current) {
          store.appendThinking(assistantIdRef.current, event.data);
        } else {
          store.appendToken(assistantIdRef.current, event.data);
        }
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
        store.setToolCall(event.name, event.args);
        break;
      case 'tool_result':
        if (isCancelledRef.current) break;
        store.completeToolCall(event.name, event.result, event.success);
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
        assistantIdRef.current = null;
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
        clearStreamTimer();
        if (!isCancelledRef.current && assistantIdRef.current) {
          store.finalizeAssistantMessage(assistantIdRef.current, `[Error] ${event.message}`);
        }
        assistantIdRef.current = null;
        isCancelledRef.current = false;
        break;
      }
      case 'cancelled':
        clearStreamTimer();
        assistantIdRef.current = null;
        isCancelledRef.current = false;
        break;
      case 'done':
        clearStreamTimer();
        // If no final_answer was received, finalize with empty content
        if (assistantIdRef.current && !isCancelledRef.current) {
          store.finalizeAssistantMessage(assistantIdRef.current, '');
        }
        assistantIdRef.current = null;
        isCancelledRef.current = false;
        break;
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
      clearStreamTimer();
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

      // 60s streaming timeout
      clearStreamTimer();
      streamTimer.current = setTimeout(() => {
        if (useChatStore.getState().isStreaming) {
          useChatStore.getState().markCancelled();
        }
      }, 60_000);

      // Pass conversation_id for pool-based parallel execution
      const conversation_id = useConversationStore.getState().activeId;
      await apiInvoke('send_chat_message', {
        message: text,
        conversation_id: conversation_id ?? undefined,
      });
    } catch (e) {
      console.error('[TauriChat] Failed to send message:', e);
      if (assistantIdRef.current) {
        store.finalizeAssistantMessage(assistantIdRef.current, `[Error] ${e}`);
      }
      assistantIdRef.current = null;
    }
  }, []);

  const sendApproval = useCallback(
    async (requestId: string, approved: boolean, reason?: string, scope?: string) => {
      try {
        await apiInvoke('send_approval_response', { request_id: requestId, approved, reason, scope });
        useChatStore.getState().setApprovalRequest(null);
      } catch (e) {
        console.error('[TauriChat] Failed to send approval:', e);
      }
    },
    []
  );

  const sendInput = useCallback(async (requestId: string, text: string) => {
    try {
      await apiInvoke('send_input_response', { request_id: requestId, text });
      useChatStore.getState().setInputRequest(null);
    } catch (e) {
      console.error('[TauriChat] Failed to send input:', e);
    }
  }, []);

  const sendSelection = useCallback(
    async (requestId: string, selection: string, instructions?: string) => {
      try {
        await apiInvoke('send_selection_response', {
          request_id: requestId,
          selection,
          instructions,
        });
        useChatStore.getState().setSelectionRequest(null);
      } catch (e) {
        console.error('[TauriChat] Failed to send selection:', e);
      }
    },
    []
  );

  const cancel = useCallback(async () => {
    useChatStore.getState().markCancelled();
    assistantIdRef.current = null;
    isCancelledRef.current = true;
    try {
      await apiInvoke('cancel_chat');
    } catch (e) {
      console.error('[TauriChat] Failed to cancel:', e);
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
