import { useRef, useCallback, useEffect, useState } from 'react';
import { useChatStore } from '../stores/chatStore';
import { useConversationStore } from '../stores/conversationStore';
import { useSubagentRunStore, type ExecutionEvent } from '../stores/subagentRunStore';
import { useTaskRuntimeStore } from '../stores/taskRuntimeStore';
import { useToastStore } from '../stores/toastStore';
import { isTauri, apiInvoke, errorMessage } from '../lib/tauri-bridge';
import { handleChatEvent } from './chatEventHandler';
import { reorderById } from './queuedChat';
import type { Attachment, ChatEvent } from '../types/api';

export type QueuedChatInput = {
  id: string;
  text: string;
  attachments?: Attachment[];
};

export function useTauriChat() {
  const assistantIdRef = useRef<string | null>(null);
  const isCancelledRef = useRef(false);
  const currentMessageKeyRef = useRef<string | null>(null);
  const currentConversationIdRef = useRef<string | null>(null);
  const thinkingIdRef = useRef<string | null>(null);
  const queuedInputsRef = useRef<QueuedChatInput[]>([]);
  const dispatchMessageRef = useRef<
    ((text: string, attachments: Attachment[] | undefined) => void) | null
  >(null);
  const [queuedInputs, setQueuedInputs] = useState<QueuedChatInput[]>([]);

  const replaceQueue = (next: QueuedChatInput[]) => {
    queuedInputsRef.current = next;
    setQueuedInputs(next);
  };

  const dispatchNextQueued = () => {
    const [next, ...remaining] = queuedInputsRef.current;
    replaceQueue(remaining);
    if (next) {
      queueMicrotask(() => dispatchMessageRef.current?.(next.text, next.attachments));
    }
  };

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
    handleChatEvent(event, {
      assistantIdRef,
      currentMessageKeyRef,
      currentMessageIdRef: currentMessageKeyRef,
      isCancelledRef,
      currentThinkingIdRef: thinkingIdRef,
    });
    if (event.type === 'done') {
      dispatchNextQueued();
    }
  }, []);

  // Set up event listener on mount.
  //
  // P0-4 修复: 此前 cleanup 只在 setupListener 跑完 (unlistenRef.current 赋值后)
  // 才生效。若组件在 `await import()` / `await listen()` 之间卸载, cleanup 执行时
  // unlistenRef.current 仍是 null → 空操作; 但 setupListener 稍后 resolve 并注册了
  // 监听器, 该 unlisten 句柄永远没人调用 → 监听器泄漏 (内存 + 幽灵事件)。
  //
  // 修法: 用 `aborted` 标志 + `pendingCleanup` 数组收集 unlisten 函数, cleanup 时
  // 既设标志 (让后续 callback 短路) 又遍历清理已注册的监听器; setupListener resolve
  // 时若已 aborted, 立即注销刚拿到的监听器。
  useEffect(() => {
    if (!isTauri()) return;

    let aborted = false;
    const pendingCleanup: Array<() => void> = [];

    const setupListener = async () => {
      const { listen } = await import('@tauri-apps/api/event');
      if (aborted) return; // 卸载发生在 import 期间, 不再注册
      const unlisten = await listen<ChatEvent>('chat://event', (event) => {
        if (!aborted) {
          handleEvent(event.payload);
        }
      });
      // 卸载发生在两个 listen 之间: 立即注销刚注册的第一个, 不再注册第二个。
      if (aborted) {
        unlisten();
        return;
      }
      pendingCleanup.push(unlisten);
      // Unified execution://event channel (Subagent unification Phase 4).
      // Replaces the legacy subagent trace + subagent event channels.
      // kind="subagent" → subagentRunStore (thinking/tool/token/usage flows);
      // kind="run" → run lifecycle (run_started triggers loadByConversation).
      const unlistenExec = await listen<Record<string, unknown>>('execution://event', (event) => {
        if (aborted) return;
        const payload = event.payload;
        const kind = payload.kind as string | undefined;
        if (kind === 'subagent') {
          const runId = String(payload.subagent_run_id ?? '');
          const prevStatus = runId ? useSubagentRunStore.getState().runs[runId]?.status : undefined;
          useSubagentRunStore.getState().ingest(payload as unknown as ExecutionEvent);
          // Background subagent finished → inject a non-streaming chat note
          // (Cursor/Claude Code style: don't interrupt the parent ReAct turn).
          if (runId && payload.event === 'completed') {
            const run = useSubagentRunStore.getState().runs[runId];
            if (run?.background && prevStatus !== 'completed') {
              const summary =
                (typeof payload.summary === 'string' && payload.summary.trim()) ||
                (run.summary && run.summary.trim()) ||
                (run.output && run.output.trim()) ||
                '(no summary)';
              const note = `[subagent ${run.agent || runId} finished]\n${summary}`;
              useChatStore.getState().appendLocalAssistantNote(note);
            }
          }
        } else if (kind === 'run' && payload.event === 'run_started') {
          // 正式 plan / 自主 run 通过 run_started 事件激活右侧面板。
          const convId =
            (payload.conversation_id as string | undefined) ??
            useConversationStore.getState().activeId;
          if (convId) {
            useTaskRuntimeStore
              .getState()
              .loadByConversation(convId)
              .catch((e) => console.warn('[TauriChat] Failed to load task run on run_started:', e));
          }
        }
      });
      // 卸载发生在第二个 listen 之后、push 之前: 立即注销。
      if (aborted) {
        unlistenExec();
        return;
      }
      pendingCleanup.push(unlistenExec);
    };

    setupListener();

    return () => {
      aborted = true;
      // 清理所有已注册的监听器 (覆盖三种竞态窗口)。
      pendingCleanup.forEach((fn) => fn());
      pendingCleanup.length = 0;
    };
  }, [handleEvent]);

  const dispatchMessage = useCallback(async (text: string, attachments?: Attachment[]) => {
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
      const message_key =
        typeof crypto !== 'undefined' && 'randomUUID' in crypto
          ? crypto.randomUUID()
          : `chat-${Date.now()}-${Math.random().toString(36).slice(2)}`;
      currentMessageKeyRef.current = message_key;
      assistantIdRef.current = store.startAssistantMessage(message_key);

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
      // panel can show plan/todos/subagents/tokens (replaces the old plan_ready
      // event handler deleted in the 13→6 state machine migration).
      const createdRunId = chatResult?.run_id ?? chatResult?.runId;
      if (createdRunId && conversation_id) {
        useTaskRuntimeStore
          .getState()
          .loadByConversation(conversation_id)
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
      }
    } catch (e) {
      console.error('[TauriChat] Failed to send message:', e);
      if (assistantIdRef.current) {
        store.finalizeAssistantMessage(assistantIdRef.current, `[Error] ${errorMessage(e)}`);
      }
      store.setRunStatus('failed');
      assistantIdRef.current = null;
      currentMessageKeyRef.current = null;
      dispatchNextQueued();
    }
  }, []);

  dispatchMessageRef.current = dispatchMessage;

  const sendMessage = useCallback(
    (text: string, attachments?: Attachment[]) => {
      if (currentMessageKeyRef.current) {
        const id =
          typeof crypto !== 'undefined' && 'randomUUID' in crypto
            ? crypto.randomUUID()
            : `queued-${Date.now()}-${Math.random().toString(36).slice(2)}`;
        replaceQueue([...queuedInputsRef.current, { id, text, attachments }]);
        return;
      }
      void dispatchMessage(text, attachments);
    },
    [dispatchMessage]
  );

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
    }
  }, []);

  const clearQueuedMessages = useCallback(() => {
    replaceQueue([]);
  }, []);

  const removeQueuedMessage = useCallback((id: string) => {
    replaceQueue(queuedInputsRef.current.filter((item) => item.id !== id));
  }, []);

  const reorderQueuedMessage = useCallback((sourceId: string, targetId: string) => {
    replaceQueue(reorderById(queuedInputsRef.current, sourceId, targetId));
  }, []);

  const steerQueuedMessage = useCallback(
    async (id: string) => {
      const queued = queuedInputsRef.current.find((item) => item.id === id);
      const conversationId = useConversationStore.getState().activeId;
      if (!queued || !conversationId) return false;
      try {
        const result = await apiInvoke<{ kind: string }>('steer_chat_message', {
          message: queued.text,
          attachments: queued.attachments,
          conversationId,
          conversation_id: conversationId,
        });
        if (result.kind !== 'accepted') {
          useToastStore.getState().addToast('info', '当前阶段不能插入，已保留在排队队列中');
          return false;
        }
        const displayAttachments = queued.attachments?.map((attachment) => ({
          name: attachment.name,
          mime_type: attachment.mime_type,
          url: `data:${attachment.mime_type};base64,${attachment.data}`,
          size: attachment.size,
        }));
        assistantIdRef.current = useChatStore
          .getState()
          .continueAfterSteer(assistantIdRef.current, queued.text || '(附件)', displayAttachments);
        removeQueuedMessage(id);
        return true;
      } catch (error) {
        useToastStore.getState().addToast('error', `补充当前任务失败：${errorMessage(error)}`);
        return false;
      }
    },
    [removeQueuedMessage]
  );

  return {
    sendMessage,
    sendApproval,
    sendInput,
    sendSelection,
    cancel,
    queuedInputs,
    clearQueuedMessages,
    removeQueuedMessage,
    reorderQueuedMessage,
    steerQueuedMessage,
  };
}
