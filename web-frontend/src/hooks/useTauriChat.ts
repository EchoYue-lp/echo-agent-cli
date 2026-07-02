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
    | {
        type: 'llm_usage';
        model: string;
        prompt_tokens: number;
        completion_tokens: number;
        total_tokens: number;
        cached_prompt_tokens: number;
        cache_creation_prompt_tokens: number;
        usage_reported: boolean;
      }
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
    | { type: 'done' }
  );

function normalizeWorkerTraceEvent(event: WorkerTraceEvent): WorkerTraceEvent {
  if (!event.payload || typeof event.payload !== 'object' || Array.isArray(event.payload)) {
    return event;
  }
  const payloadRunId = (event.payload as Record<string, unknown>).run_id;
  if (
    typeof payloadRunId === 'string' &&
    payloadRunId.length > 0 &&
    payloadRunId !== event.run_id
  ) {
    return { ...event, run_id: payloadRunId };
  }
  return event;
}

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
          const traceEvent = normalizeWorkerTraceEvent(event.payload);
          useWorkerTraceStore.getState().append(traceEvent);
          // inline task / 自主 run 通过 RunStarted 事件激活右侧面板。
          // send_chat_message 返回值不带 run_id（run 在 agent ReAct 循环内异步建），
          // 故靠此事件驱动 loadByConversation → 激活 activeRun，
          // 否则 worker 卡片 / 任务进度 / Token 面板全空。
          if (traceEvent.event_type === 'run_started') {
            const payload = traceEvent.payload as Record<string, unknown> | undefined;
            const convId =
              (payload?.conversation_id as string | undefined) ??
              useConversationStore.getState().activeId;
            if (convId) {
              import('../stores/taskRuntimeStore')
                .then(({ useTaskRuntimeStore }) => {
                  useTaskRuntimeStore.getState().loadByConversation(convId);
                })
                .catch((e) =>
                  console.warn('[TauriChat] Failed to load task run on run_started:', e)
                );
            }
          }
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
      const chatResult = await apiInvoke<{
        success: boolean;
        run_id?: string;
        status?: string;
        mode?: string;
        route?: string;
        runId?: string;
      }>('send_chat_message', {
        message: text,
        // Multimodal: forward attachments (base64-encoded) so the backend can
        // persist them and build a multimodal Message for the LLM.
        attachments: attachments && attachments.length > 0 ? attachments : undefined,
        conversationId: conversation_id ?? undefined,
        conversation_id: conversation_id ?? undefined,
        messageKey: message_key,
        message_key,
      });
      // If the backend created a TaskRuntime run, load it so the right rail
      // panel can show plan/todos/workers/tokens (replaces the old plan_ready
      // event handler deleted in the 13→6 state machine migration).
      const createdRunId = chatResult?.run_id ?? chatResult?.runId;
      if (createdRunId && conversation_id) {
        import('../stores/taskRuntimeStore')
          .then(({ useTaskRuntimeStore }) => {
            useTaskRuntimeStore
              .getState()
              .loadByConversation(conversation_id!)
              .then(() => {
                const store = useTaskRuntimeStore.getState();
                const run = store.activeRun;
                if (
                  run &&
                  (run.status === 'pending' || run.status === 'running' || run.status === 'paused')
                ) {
                  store.startPolling(run.run_id);
                }
              })
              .catch((e) => console.warn('[TauriChat] Failed to load task runtime:', e));
          })
          .catch((e) => console.warn('[TauriChat] Failed to import taskRuntimeStore:', e));
      }
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
  };
}
