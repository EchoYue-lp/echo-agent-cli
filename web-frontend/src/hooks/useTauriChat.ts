import { useRef, useCallback, useEffect, useState } from 'react';
import { useChatStore } from '../stores/chatStore';
import { useConversationStore } from '../stores/conversationStore';
import {
  subagentRunStoreKey,
  useSubagentRunStore,
  type ExecutionEvent,
} from '../stores/subagentRunStore';
import { useTaskRuntimeStore } from '../stores/taskRuntimeStore';
import { useToastStore } from '../stores/toastStore';
import { useToolExecutionStore } from '../stores/toolExecutionStore';
import { isTauri, apiInvoke, errorMessage } from '../lib/tauri-bridge';
import { handleChatEvent } from './chatEventHandler';
import { reorderById } from './queuedChat';
import type { ForegroundTurnSnapshot } from '../generated';
import type { Attachment, ChatEvent, ToolExecution } from '../types/api';

export type QueuedChatInput = {
  id: string;
  text: string;
  attachments?: Attachment[];
};

type CancelChatResponse = {
  success: boolean;
  turn_id: string;
  status: 'completed' | 'cancelled' | 'failed';
};

export function useTauriChat() {
  const assistantIdRef = useRef<string | null>(null);
  const isCancelledRef = useRef(false);
  const currentMessageKeyRef = useRef<string | null>(null);
  const currentConversationIdRef = useRef<string | null>(null);
  const identityGenerationRef = useRef(0);
  const thinkingIdRef = useRef<string | null>(null);
  const queuedInputsRef = useRef<QueuedChatInput[]>([]);
  const dispatchMessageRef = useRef<
    ((text: string, attachments: Attachment[] | undefined) => Promise<boolean>) | null
  >(null);
  const [queuedInputs, setQueuedInputs] = useState<QueuedChatInput[]>([]);

  const getActiveTurnSnapshot = useCallback(async () => {
    const conversationId = useConversationStore.getState().activeId;
    return apiInvoke<ForegroundTurnSnapshot | null>('get_active_chat_turn', {
      conversationId: conversationId ?? undefined,
      conversation_id: conversationId ?? undefined,
    });
  }, []);

  const restoreActiveTurnRefs = useCallback((snapshot: ForegroundTurnSnapshot | null) => {
    if (snapshot && !currentMessageKeyRef.current) {
      currentConversationIdRef.current = snapshot.conversation_id;
      currentMessageKeyRef.current = snapshot.turn_id;
    }
  }, []);

  // The Rust owner survives WebView and hook remounts. Restore its exact scope
  // key so Stop never falls back to an unkeyed/global cancellation. When a
  // conversation has not been persisted yet, the backend returns the sole
  // active `message:<turn_id>` scope; it rejects an ambiguous global lookup.
  useEffect(() => {
    if (!isTauri()) return;
    let aborted = false;
    const identityGeneration = identityGenerationRef.current;
    const restoreActiveTurn = async () => {
      try {
        const snapshot = await getActiveTurnSnapshot();
        if (!aborted && identityGeneration === identityGenerationRef.current) {
          restoreActiveTurnRefs(snapshot);
        }
      } catch (error) {
        if (!aborted) {
          console.warn('[TauriChat] Failed to restore active foreground turn:', error);
        }
      }
    };
    void restoreActiveTurn();
    return () => {
      aborted = true;
    };
  }, [getActiveTurnSnapshot, restoreActiveTurnRefs]);

  const replaceQueue = (next: QueuedChatInput[]) => {
    queuedInputsRef.current = next;
    setQueuedInputs(next);
  };

  const dispatchNextQueued = useCallback(() => {
    const [next, ...remaining] = queuedInputsRef.current;
    replaceQueue(remaining);
    if (next) {
      queueMicrotask(() => {
        void dispatchMessageRef.current?.(next.text, next.attachments);
      });
    }
  }, []);

  const isCurrentRunEvent = (event: ChatEvent) => {
    if (event.message_key) {
      return currentMessageKeyRef.current === event.message_key;
    }
    if (event.conversation_id) {
      return currentConversationIdRef.current === event.conversation_id;
    }
    return true;
  };

  const handleEvent = useCallback(
    (event: ChatEvent) => {
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
    },
    [dispatchNextQueued]
  );

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
      // kind="subagent" → lifecycle, usage, and complete terminal result;
      // kind="tool" → shared lightweight summaries for main/subagent tools;
      // kind="run" → run lifecycle (run_started triggers loadByConversation).
      const unlistenExec = await listen<Record<string, unknown>>('execution://event', (event) => {
        if (aborted) return;
        const payload = event.payload;
        const kind = payload.kind as string | undefined;
        if (kind === 'subagent') {
          const subagentRunId = String(payload.subagent_run_id ?? '');
          const taskRunId = String(payload.run_id ?? '');
          const storeKey = subagentRunId ? subagentRunStoreKey(taskRunId, subagentRunId) : null;
          const prevStatus = storeKey
            ? useSubagentRunStore.getState().runs[storeKey]?.status
            : undefined;
          useSubagentRunStore.getState().ingest(payload as unknown as ExecutionEvent);
          // Background Subagent completion is already represented by its
          // message-bound execution card. Notify without appending a second
          // assistant message that duplicates the terminal summary.
          if (storeKey && payload.event === 'completed') {
            const run = useSubagentRunStore.getState().runs[storeKey];
            if (run?.background && prevStatus !== 'completed') {
              useToastStore
                .getState()
                .addToast('success', `Subagent ${run.agent || subagentRunId} 已完成`);
            }
          }
        } else if (kind === 'tool') {
          const tool = payload as unknown as ToolExecution;
          useToolExecutionStore.getState().ingest(tool);
          if (payload.event === 'started' && tool.owner.kind === 'chat') {
            useChatStore.getState().recordToolStart(tool.owner.message_id, tool.id);
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

  const dispatchMessage = useCallback(
    async (text: string, attachments?: Attachment[]) => {
      const store = useChatStore.getState();
      const displayAttachments = attachments?.map((a) => ({
        name: a.name,
        mime_type: a.mime_type,
        url: `data:${a.mime_type};base64,${a.data}`,
        size: a.size,
        source: a.source,
      }));
      const userMessageId = store.addUserMessage(text || '(附件)', displayAttachments);
      let pendingAssistantId: string | null = null;

      try {
        identityGenerationRef.current += 1;
        isCancelledRef.current = false;
        thinkingIdRef.current = null;
        const message_key =
          typeof crypto !== 'undefined' && 'randomUUID' in crypto
            ? crypto.randomUUID()
            : `chat-${Date.now()}-${Math.random().toString(36).slice(2)}`;
        currentMessageKeyRef.current = message_key;
        pendingAssistantId = store.startAssistantMessage(message_key);
        assistantIdRef.current = pendingAssistantId;

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
            .catch((e) => console.warn('[TauriChat] Failed to load task runtime:', e));
        }
        return true;
      } catch (e) {
        console.error('[TauriChat] Failed to send message:', e);
        store.removeMessages(
          pendingAssistantId ? [userMessageId, pendingAssistantId] : [userMessageId]
        );
        useToastStore.getState().addToast('error', `发送失败：${errorMessage(e)}`);
        store.setRunStatus('failed');
        assistantIdRef.current = null;
        currentMessageKeyRef.current = null;
        dispatchNextQueued();
        return false;
      }
    },
    [dispatchNextQueued]
  );

  dispatchMessageRef.current = dispatchMessage;

  const sendMessage = useCallback(
    async (text: string, attachments?: Attachment[]) => {
      if (currentMessageKeyRef.current) {
        const id =
          typeof crypto !== 'undefined' && 'randomUUID' in crypto
            ? crypto.randomUUID()
            : `queued-${Date.now()}-${Math.random().toString(36).slice(2)}`;
        replaceQueue([...queuedInputsRef.current, { id, text, attachments }]);
        return true;
      }
      return dispatchMessage(text, attachments);
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
    identityGenerationRef.current += 1;
    let messageKey = currentMessageKeyRef.current;
    let conversationId = currentConversationIdRef.current;
    try {
      // Mount recovery is asynchronous. Stop must independently recover the
      // exact registry identity when the effect has not populated refs yet.
      if (!messageKey || !conversationId) {
        const snapshot = await getActiveTurnSnapshot();
        if (snapshot) {
          restoreActiveTurnRefs(snapshot);
          messageKey = snapshot.turn_id;
          conversationId = snapshot.conversation_id;
        }
      }
      if (!messageKey || !conversationId) {
        useToastStore.getState().addToast('error', '无法定位正在运行的任务，请稍后重试');
        return;
      }
      const settlement = await apiInvoke<CancelChatResponse>('cancel_chat', {
        conversationId,
        conversation_id: conversationId,
        messageKey,
        message_key: messageKey,
      });
      if (!settlement.success) {
        throw new Error(`取消请求未完成（${settlement.status}）`);
      }
      // The existing chat terminal event is the sole UI projection authority.
      // Keep refs until that event arrives so the versioned event is accepted
      // and queued input advances exactly once through `done`.
    } catch (e) {
      console.error('[TauriChat] Failed to cancel:', e);
      useToastStore.getState().addToast('error', `停止任务失败：${errorMessage(e)}`);
    }
  }, [getActiveTurnSnapshot, restoreActiveTurnRefs]);

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
          source: attachment.source,
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
