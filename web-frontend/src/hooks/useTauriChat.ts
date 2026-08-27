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
import { useWorkspaceStore } from '../stores/workspaceStore';
import { isTauri, apiInvoke, errorMessage } from '../lib/tauri-bridge';
import { viewAddress, viewAddressKey, workspaceIdForView } from '../lib/viewAddress';
import { handleChatEventEnvelope } from './chatEventHandler';
import { ChatEventSequencer } from './chatEventSequencer';
import { reorderById } from './queuedChat';
import type { ChatSteerReceipt, ForegroundTurnSnapshot } from '../generated';
import type { Attachment, ChatEventEnvelope, ChatEventReplay, ToolExecution } from '../types/api';

export type QueuedChatInput = {
  id: string;
  text: string;
  attachments?: Attachment[];
  workspaceId: string;
  conversationId: string;
  backendManaged?: boolean;
};

type SendChatResult = {
  success?: boolean;
  kind?: 'started' | 'queued' | 'task_run_conflict' | 'interrupt_prompt';
  run_id?: string;
  runId?: string;
  root_turn_id?: string;
  active_turn_id?: string;
  input_id?: string;
  goal?: string;
  new_message?: string;
};

type QueuedChatInputWire = {
  input_id: string;
  workspace_id: string;
  conversation_id: string;
  text: string;
  attachments: Attachment[];
  submitted_at_ms: number;
};

type CancelChatResponse = {
  success: boolean;
  turn_id: string;
  status: 'completed' | 'cancelled' | 'failed' | 'already_settled';
};

// The queue is a projection keyed by exact address. Keeping the buckets above
// the hook preserves accepted local fallbacks across ChatPanel remounts while
// the backend-managed entries are reconciled from their durable receipts.
const queuedInputBuckets = new Map<string, QueuedChatInput[]>();
const queuedDispatches = new Set<string>();

function chatStreamId(workspaceId: string, conversationId?: string, messageKey?: string): string {
  return JSON.stringify([workspaceId, conversationId ?? messageKey ?? '']);
}

export function useTauriChat() {
  const activeConversationId = useConversationStore((state) => state.activeId);
  const newConversationEpoch = useConversationStore((state) => state.newConversationEpoch);
  const currentWorkspaceId = useWorkspaceStore((state) => workspaceIdForView(state.current?.id));
  const assistantIdRef = useRef<string | null>(null);
  const isCancelledRef = useRef(false);
  const activeTurnIdRef = useRef<string | null>(null);
  const currentMessageKeyRef = useRef<string | null>(null);
  const currentConversationIdRef = useRef<string | null>(activeConversationId);
  const currentWorkspaceIdRef = useRef(currentWorkspaceId);
  const identityGenerationRef = useRef(0);
  const previousActiveConversationIdRef = useRef(activeConversationId);
  const previousNewConversationEpochRef = useRef(newConversationEpoch);
  const thinkingIdRef = useRef<string | null>(null);
  const eventSequencerRef = useRef(new ChatEventSequencer());
  const queuedInputsByAddressRef = useRef(queuedInputBuckets);
  const visibleQueueKeyRef = useRef<string | null>(
    activeConversationId
      ? viewAddressKey(viewAddress(currentWorkspaceId, activeConversationId))
      : null
  );
  const pendingAdmissionRef = useRef<{
    messageKey: string;
    events: ChatEventEnvelope[];
  } | null>(null);
  const dispatchMessageRef = useRef<
    | ((text: string, attachments: Attachment[] | undefined, inputId?: string) => Promise<boolean>)
    | null
  >(null);
  const [queuedInputs, setQueuedInputs] = useState<QueuedChatInput[]>([]);

  const getActiveTurnSnapshot = useCallback(async () => {
    // Every dispatched GUI turn receives a durable conversation id before it
    // reaches Rust. An unscoped lookup from a blank new-chat view can only
    // attach an unrelated running conversation when the project has one.
    if (!activeConversationId) return null;
    return apiInvoke<ForegroundTurnSnapshot | null>('get_active_chat_turn', {
      workspaceId: currentWorkspaceId,
      conversationId: activeConversationId,
    });
  }, [activeConversationId, currentWorkspaceId]);

  const restoreActiveTurnRefs = useCallback((snapshot: ForegroundTurnSnapshot | null) => {
    const activeConversation = useConversationStore.getState().activeId;
    if (
      snapshot &&
      snapshot.workspace_id === currentWorkspaceIdRef.current &&
      snapshot.conversation_id === activeConversation
    ) {
      currentWorkspaceIdRef.current = snapshot.workspace_id;
      currentConversationIdRef.current = snapshot.conversation_id;
      activeTurnIdRef.current = snapshot.active_turn_id;
      const chat = useChatStore.getState();
      if (!chat.messages.some((message) => message.id === snapshot.root_turn_id)) {
        chat.startAssistantMessage(snapshot.root_turn_id);
      }
      currentMessageKeyRef.current = snapshot.root_turn_id;
      assistantIdRef.current = snapshot.root_turn_id;
    }
  }, []);

  const replaceQueue = useCallback((addressKey: string, next: QueuedChatInput[]) => {
    if (next.length === 0) queuedInputsByAddressRef.current.delete(addressKey);
    else queuedInputsByAddressRef.current.set(addressKey, next);
    if (visibleQueueKeyRef.current === addressKey) setQueuedInputs(next);
  }, []);

  useEffect(() => {
    const previous = previousActiveConversationIdRef.current;
    const startsNewConversation = previousNewConversationEpochRef.current !== newConversationEpoch;
    const workspaceChanged = currentWorkspaceIdRef.current !== currentWorkspaceId;
    if (previous === activeConversationId && !startsNewConversation && !workspaceChanged) return;
    currentWorkspaceIdRef.current = currentWorkspaceId;
    previousActiveConversationIdRef.current = activeConversationId;
    previousNewConversationEpochRef.current = newConversationEpoch;
    identityGenerationRef.current += 1;

    const adoptsCurrentTurn =
      !startsNewConversation &&
      previous === null &&
      activeConversationId !== null &&
      activeTurnIdRef.current !== null;
    currentConversationIdRef.current = activeConversationId;
    visibleQueueKeyRef.current = activeConversationId
      ? viewAddressKey(viewAddress(currentWorkspaceId, activeConversationId))
      : null;
    setQueuedInputs(
      visibleQueueKeyRef.current
        ? (queuedInputsByAddressRef.current.get(visibleQueueKeyRef.current) ?? [])
        : []
    );
    if (adoptsCurrentTurn) return;

    activeTurnIdRef.current = null;
    currentMessageKeyRef.current = null;
    assistantIdRef.current = null;
    thinkingIdRef.current = null;
    isCancelledRef.current = false;
    pendingAdmissionRef.current = null;
  }, [activeConversationId, currentWorkspaceId, newConversationEpoch]);

  const dispatchNextQueued = useCallback(
    (workspaceId: string, conversationId: string) => {
      const addressKey = viewAddressKey(viewAddress(workspaceId, conversationId));
      if (visibleQueueKeyRef.current !== addressKey) return;
      const queued = queuedInputsByAddressRef.current.get(addressKey) ?? [];
      const next = queued[0];
      if (!next) return;
      if (next.backendManaged) {
        if (queuedDispatches.has(next.id)) return;
        queuedDispatches.add(next.id);
        queueMicrotask(() => {
          void (async () => {
            try {
              const started = await dispatchMessageRef.current?.(
                next.text,
                next.attachments,
                next.id
              );
              if (!started) return;
              await apiInvoke('remove_queued_chat_input', {
                workspaceId,
                conversationId,
                inputId: next.id,
              });
              const current = queuedInputsByAddressRef.current.get(addressKey) ?? [];
              replaceQueue(
                addressKey,
                current.filter((item) => item.id !== next.id)
              );
            } finally {
              queuedDispatches.delete(next.id);
            }
          })();
        });
        return;
      }
      replaceQueue(addressKey, queued.slice(1));
      queueMicrotask(() => {
        void dispatchMessageRef.current?.(next.text, next.attachments);
      });
    },
    [replaceQueue]
  );

  const isCurrentStreamEvent = useCallback((event: ChatEventEnvelope) => {
    if (!event.workspace_id || event.workspace_id !== currentWorkspaceIdRef.current) return false;
    if (event.conversation_id) {
      const activeConversation =
        useConversationStore.getState().activeId ?? currentConversationIdRef.current;
      return activeConversation === event.conversation_id;
    }
    return (
      activeTurnIdRef.current === event.turn_id ||
      currentMessageKeyRef.current === event.root_turn_id ||
      useChatStore
        .getState()
        .messages.some((message) => message.id === event.root_turn_id && message.isStreaming)
    );
  }, []);

  const isCurrentRunEvent = useCallback(
    (event: ChatEventEnvelope) => {
      if (!isCurrentStreamEvent(event)) return false;
      if (!event.conversation_id) return true;
      const rootMessageId = currentMessageKeyRef.current;
      return !rootMessageId || rootMessageId === event.root_turn_id;
    },
    [isCurrentStreamEvent]
  );

  const rebindEventRefs = useCallback((event: ChatEventEnvelope) => {
    activeTurnIdRef.current = event.turn_id;
    const message = useChatStore
      .getState()
      .messages.find((candidate) => candidate.id === event.root_turn_id && candidate.isStreaming);
    if (!message) return;
    currentConversationIdRef.current = event.conversation_id;
    currentMessageKeyRef.current = event.root_turn_id;
    assistantIdRef.current = event.root_turn_id;
  }, []);

  const applyEvent = useCallback(
    (event: ChatEventEnvelope) => {
      if (!isCurrentRunEvent(event)) return;
      rebindEventRefs(event);
      handleChatEventEnvelope(event, {
        assistantIdRef,
        currentMessageKeyRef,
        currentMessageIdRef: currentMessageKeyRef,
        isCancelledRef,
        currentThinkingIdRef: thinkingIdRef,
      });
      const terminalStatus =
        event.payload.source === 'turn_status' &&
        ['completed', 'failed', 'cancelled'].includes(event.payload.event.status);
      const terminalAgent =
        event.payload.source === 'agent' &&
        ['final_answer', 'cancelled', 'error'].includes(event.payload.event.payload.type);
      if (terminalStatus || terminalAgent) {
        activeTurnIdRef.current = null;
        currentMessageKeyRef.current = null;
        assistantIdRef.current = null;
        thinkingIdRef.current = null;
        if (terminalStatus && event.conversation_id) {
          dispatchNextQueued(event.workspace_id, event.conversation_id);
        }
      }
    },
    [dispatchNextQueued, isCurrentRunEvent, rebindEventRefs]
  );

  const handleEvent = useCallback(
    (event: ChatEventEnvelope) => {
      const pendingAdmission = pendingAdmissionRef.current;
      if (pendingAdmission && pendingAdmission.messageKey === event.message_id) {
        pendingAdmission.events.push(event);
        return;
      }
      applyEvent(event);
    },
    [applyEvent]
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
      const setupIdentityGeneration = identityGenerationRef.current;
      const { listen } = await import('@tauri-apps/api/event');
      if (aborted) return; // 卸载发生在 import 期间, 不再注册
      const unlisten = await listen<ChatEventEnvelope>('chat://event', (event) => {
        if (!aborted && isCurrentStreamEvent(event.payload)) {
          eventSequencerRef.current.ingest(event.payload, handleEvent);
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
        const workspaceId =
          typeof payload.workspace_id === 'string' ? payload.workspace_id : undefined;
        const conversationId =
          typeof payload.conversation_id === 'string' ? payload.conversation_id : undefined;
        const activeConversation = useConversationStore.getState().activeId;
        if (!workspaceId || !conversationId) {
          console.error('[TauriChat] Ignored execution event without workspace address', payload);
          return;
        }
        if (
          workspaceId !== currentWorkspaceIdRef.current ||
          conversationId !== activeConversation
        ) {
          return;
        }
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
          useTaskRuntimeStore
            .getState()
            .loadByConversation(workspaceId, conversationId)
            .catch((e) => console.warn('[TauriChat] Failed to load task run on run_started:', e));
        } else if (
          kind === 'run' &&
          ['run_completed', 'run_failed', 'run_cancelled'].includes(String(payload.event))
        ) {
          dispatchNextQueued(workspaceId, conversationId);
        }
      });
      // 卸载发生在第二个 listen 之后、push 之前: 立即注销。
      if (aborted) {
        unlistenExec();
        return;
      }
      pendingCleanup.push(unlistenExec);
      try {
        const snapshot = await getActiveTurnSnapshot();
        if (!aborted && setupIdentityGeneration === identityGenerationRef.current) {
          restoreActiveTurnRefs(snapshot);
          const activeConversation = useConversationStore.getState().activeId;
          const snapshotIsMessageScope = snapshot?.conversation_id.startsWith('message:') ?? false;
          const conversationId = snapshotIsMessageScope
            ? undefined
            : (activeConversation ?? snapshot?.conversation_id);
          const messageKey = snapshot?.root_turn_id;
          if (conversationId || messageKey) {
            const streamId = chatStreamId(currentWorkspaceId, conversationId, messageKey);
            const replay = await apiInvoke<ChatEventReplay>('replay_chat_events', {
              workspaceId: currentWorkspaceId,
              conversationId,
              messageKey,
              afterCursor: eventSequencerRef.current.cursor(streamId),
            });
            if (!aborted && setupIdentityGeneration === identityGenerationRef.current) {
              eventSequencerRef.current.ingestReplay(replay, handleEvent);
              if (!snapshot) {
                const hasTerminal = replay.events.some((event) => {
                  if (event.payload.source === 'turn_status') {
                    return ['completed', 'failed', 'cancelled'].includes(
                      event.payload.event.status
                    );
                  }
                  return (
                    event.payload.source === 'agent' &&
                    ['final_answer', 'cancelled', 'error'].includes(
                      event.payload.event.payload.type
                    )
                  );
                });
                const orphanRoot = replay.events.at(-1)?.root_turn_id;
                if (!hasTerminal && orphanRoot && conversationId) {
                  await apiInvoke('cancel_chat', {
                    workspaceId: currentWorkspaceId,
                    conversationId,
                    expectedRootTurnId: orphanRoot,
                    expectedActiveTurnId: null,
                  });
                  const repaired = await apiInvoke<ChatEventReplay>('replay_chat_events', {
                    workspaceId: currentWorkspaceId,
                    conversationId,
                    messageKey: orphanRoot,
                    afterCursor: eventSequencerRef.current.cursor(streamId),
                  });
                  eventSequencerRef.current.ingestReplay(repaired, handleEvent);
                }
              }
            }
          }
          if (activeConversation) {
            const queuedResult = await apiInvoke<QueuedChatInputWire[]>('list_queued_chat_inputs', {
              workspaceId: currentWorkspaceId,
              conversationId: activeConversation,
            });
            const queued = Array.isArray(queuedResult) ? queuedResult : [];
            if (!aborted && setupIdentityGeneration === identityGenerationRef.current) {
              const addressKey = viewAddressKey(
                viewAddress(currentWorkspaceId, activeConversation)
              );
              replaceQueue(
                addressKey,
                queued.map((input) => ({
                  id: input.input_id,
                  text: input.text,
                  attachments: input.attachments,
                  workspaceId: input.workspace_id,
                  conversationId: input.conversation_id,
                  backendManaged: true,
                }))
              );
              if (!snapshot) {
                dispatchNextQueued(currentWorkspaceId, activeConversation);
              }
            }
          }
        }
      } catch (error) {
        if (!aborted) {
          console.warn('[TauriChat] Failed to replay journaled chat events:', error);
        }
      }
    };

    void setupListener().catch((error) => {
      if (!aborted) console.warn('[TauriChat] Failed to attach event listeners:', error);
    });

    return () => {
      aborted = true;
      // 清理所有已注册的监听器 (覆盖三种竞态窗口)。
      pendingCleanup.forEach((fn) => fn());
      pendingCleanup.length = 0;
    };
  }, [
    currentWorkspaceId,
    dispatchNextQueued,
    getActiveTurnSnapshot,
    handleEvent,
    isCurrentStreamEvent,
    restoreActiveTurnRefs,
    replaceQueue,
  ]);

  const dispatchMessage = useCallback(
    async (text: string, attachments?: Attachment[], inputId?: string) => {
      const store = useChatStore.getState();
      const displayAttachments = attachments?.map((a) => ({
        name: a.name,
        mime_type: a.mime_type,
        url: `data:${a.mime_type};base64,${a.data}`,
        size: a.size,
        source: a.source,
      }));
      const messageKey =
        inputId ??
        (typeof crypto !== 'undefined' && 'randomUUID' in crypto
          ? crypto.randomUUID()
          : `chat-${Date.now()}-${Math.random().toString(36).slice(2)}`);
      const workspaceId = currentWorkspaceIdRef.current;

      try {
        identityGenerationRef.current += 1;
        isCancelledRef.current = false;
        thinkingIdRef.current = null;

        // TaskRuntime runs are keyed by conversation_id. On the first turn there
        // is no active conversation yet, so create it before routing; otherwise
        // the backend falls back to a message-scoped id and the right rail loses
        // the run as soon as the conversation is later saved as conv-*.
        if (!useConversationStore.getState().activeId) {
          await useConversationStore.getState().saveCurrent([
            {
              id: messageKey,
              role: 'user',
              content: text || '(附件)',
              timestamp: Date.now(),
              attachments: displayAttachments,
            },
          ]);
        }

        const conversationState = useConversationStore.getState();
        const conversationId = conversationState.activeId;
        if (!conversationId || conversationState.workspaceId !== workspaceId) {
          throw new Error('创建会话失败，无法启动 TaskRuntime。');
        }
        currentConversationIdRef.current = conversationId;
        pendingAdmissionRef.current = { messageKey, events: [] };
        const chatResult = await apiInvoke<SendChatResult>('send_chat_message', {
          workspaceId,
          message: text,
          // Multimodal: forward attachments (base64-encoded) so the backend can
          // persist them and build a multimodal Message for the LLM.
          attachments: attachments && attachments.length > 0 ? attachments : undefined,
          conversationId,
          messageKey,
        });
        const pendingEvents = pendingAdmissionRef.current?.events ?? [];
        pendingAdmissionRef.current = null;
        const outcome = chatResult.kind ?? (chatResult.success === false ? undefined : 'started');

        if (
          outcome === 'queued' ||
          outcome === 'task_run_conflict' ||
          outcome === 'interrupt_prompt'
        ) {
          const addressKey = viewAddressKey(viewAddress(workspaceId, conversationId));
          const currentQueue = queuedInputsByAddressRef.current.get(addressKey) ?? [];
          replaceQueue(addressKey, [
            ...currentQueue,
            {
              id: chatResult.input_id ?? messageKey,
              text,
              attachments,
              workspaceId,
              conversationId,
              backendManaged: Boolean(chatResult.input_id),
            },
          ]);
          for (const event of pendingEvents) applyEvent(event);
          if (outcome !== 'queued' && chatResult.run_id && chatResult.goal) {
            useTaskRuntimeStore.getState().openInterruptPrompt({
              runId: chatResult.run_id,
              goal: chatResult.goal,
              newMessage: chatResult.new_message ?? text,
              resolve: async (action) => {
                if (action === 'continue') {
                  const run = useTaskRuntimeStore.getState().activeRun;
                  if (run?.status === 'paused') {
                    await useTaskRuntimeStore.getState().resumeTaskRun();
                  }
                  useTaskRuntimeStore.getState().dismissInterruptPrompt();
                  return;
                }
                if (action === 'edit') {
                  useTaskRuntimeStore.getState().dismissInterruptPrompt();
                  return;
                }
                await apiInvoke('cancel_task_run', {
                  workspaceId,
                  runId: chatResult.run_id,
                });
                useTaskRuntimeStore.getState().dismissInterruptPrompt();
                const started = await dispatchMessageRef.current?.(
                  text,
                  attachments,
                  chatResult.input_id
                );
                if (started && chatResult.input_id) {
                  await apiInvoke('remove_queued_chat_input', {
                    workspaceId,
                    conversationId,
                    inputId: chatResult.input_id,
                  });
                  const queue = queuedInputsByAddressRef.current.get(addressKey) ?? [];
                  replaceQueue(
                    addressKey,
                    queue.filter((item) => item.id !== chatResult.input_id)
                  );
                }
              },
            });
          }
          currentMessageKeyRef.current = null;
          activeTurnIdRef.current = null;
          assistantIdRef.current = null;
          return true;
        }

        if (outcome !== 'started') {
          throw new Error('后端未接受本次消息');
        }

        store.addUserMessage(text || '(附件)', displayAttachments);
        currentMessageKeyRef.current = chatResult.root_turn_id ?? messageKey;
        activeTurnIdRef.current = chatResult.active_turn_id ?? messageKey;
        assistantIdRef.current = store.startAssistantMessage(currentMessageKeyRef.current);
        for (const event of pendingEvents) applyEvent(event);

        // If the backend created a TaskRuntime run, load it so the right rail
        // panel can show plan/todos/subagents/tokens (replaces the old plan_ready
        // event handler deleted in the 13→6 state machine migration).
        const createdRunId = chatResult?.run_id ?? chatResult?.runId;
        if (createdRunId) {
          useTaskRuntimeStore
            .getState()
            .loadByConversation(workspaceId, conversationId)
            .catch((e) => console.warn('[TauriChat] Failed to load task runtime:', e));
        }
        return true;
      } catch (e) {
        pendingAdmissionRef.current = null;
        console.error('[TauriChat] Failed to send message:', e);
        useToastStore.getState().addToast('error', `发送失败：${errorMessage(e)}`);
        assistantIdRef.current = null;
        currentMessageKeyRef.current = null;
        activeTurnIdRef.current = null;
        return false;
      }
    },
    [applyEvent, replaceQueue]
  );

  dispatchMessageRef.current = dispatchMessage;

  const sendMessage = useCallback(
    async (text: string, attachments?: Attachment[]) => {
      const activeConversation = useConversationStore.getState().activeId;
      const belongsToActiveConversation = activeConversation
        ? currentConversationIdRef.current === activeConversation
        : currentConversationIdRef.current === null && currentMessageKeyRef.current !== null;
      if (activeTurnIdRef.current && belongsToActiveConversation) {
        const id =
          typeof crypto !== 'undefined' && 'randomUUID' in crypto
            ? crypto.randomUUID()
            : `queued-${Date.now()}-${Math.random().toString(36).slice(2)}`;
        if (!activeConversation) return false;
        const workspaceId = currentWorkspaceIdRef.current;
        const addressKey = viewAddressKey(viewAddress(workspaceId, activeConversation));
        const accepted = await apiInvoke<QueuedChatInputWire>('queue_chat_input', {
          workspaceId,
          conversationId: activeConversation,
          inputId: id,
          text,
          attachments: attachments && attachments.length > 0 ? attachments : undefined,
        });
        const currentQueue = queuedInputsByAddressRef.current.get(addressKey) ?? [];
        replaceQueue(addressKey, [
          ...currentQueue,
          {
            id: accepted.input_id,
            text: accepted.text,
            attachments: accepted.attachments,
            workspaceId: accepted.workspace_id,
            conversationId: accepted.conversation_id,
            backendManaged: true,
          },
        ]);
        return true;
      }
      if (activeTurnIdRef.current) {
        activeTurnIdRef.current = null;
        currentMessageKeyRef.current = null;
        assistantIdRef.current = null;
      }
      return dispatchMessage(text, attachments);
    },
    [dispatchMessage, replaceQueue]
  );

  const sendApproval = useCallback(
    async (requestId: string, approved: boolean, reason?: string, scope?: string) => {
      try {
        await apiInvoke('send_approval_response', {
          workspaceId: currentWorkspaceIdRef.current,
          conversationId:
            currentConversationIdRef.current ?? useConversationStore.getState().activeId,
          expectedRootTurnId: currentMessageKeyRef.current,
          expectedActiveTurnId: activeTurnIdRef.current,
          requestId,
          approved,
          reason,
          scope,
        });
        useChatStore.getState().removeHitlRequest(requestId);
      } catch (e) {
        console.error('[TauriChat] Failed to send approval:', e);
        throw e;
      }
    },
    []
  );

  const sendInput = useCallback(async (requestId: string, text: string) => {
    try {
      await apiInvoke('send_input_response', {
        workspaceId: currentWorkspaceIdRef.current,
        conversationId:
          currentConversationIdRef.current ?? useConversationStore.getState().activeId,
        expectedRootTurnId: currentMessageKeyRef.current,
        expectedActiveTurnId: activeTurnIdRef.current,
        requestId,
        text,
      });
      useChatStore.getState().removeHitlRequest(requestId);
    } catch (e) {
      console.error('[TauriChat] Failed to send input:', e);
    }
  }, []);

  const sendSelection = useCallback(
    async (requestId: string, selection: string, instructions?: string) => {
      try {
        await apiInvoke('send_selection_response', {
          workspaceId: currentWorkspaceIdRef.current,
          conversationId:
            currentConversationIdRef.current ?? useConversationStore.getState().activeId,
          expectedRootTurnId: currentMessageKeyRef.current,
          expectedActiveTurnId: activeTurnIdRef.current,
          requestId,
          selection,
          instructions,
        });
        useChatStore.getState().removeHitlRequest(requestId);
      } catch (e) {
        console.error('[TauriChat] Failed to send selection:', e);
      }
    },
    []
  );

  const cancel = useCallback(async () => {
    identityGenerationRef.current += 1;
    let rootTurnId = currentMessageKeyRef.current;
    let conversationId = currentConversationIdRef.current;
    try {
      // Mount recovery is asynchronous. Stop must independently recover the
      // exact registry identity when the effect has not populated refs yet.
      if (!activeTurnIdRef.current || !rootTurnId || !conversationId) {
        const snapshot = await getActiveTurnSnapshot();
        if (snapshot) {
          restoreActiveTurnRefs(snapshot);
          rootTurnId = snapshot.root_turn_id;
          conversationId = snapshot.conversation_id;
        } else {
          rootTurnId = null;
          activeTurnIdRef.current = null;
        }
      }
      if (!rootTurnId || !conversationId) {
        const activeConversation = useConversationStore.getState().activeId;
        if (activeConversation) {
          const workspaceId = currentWorkspaceIdRef.current;
          const streamId = chatStreamId(workspaceId, activeConversation);
          const replay = await apiInvoke<ChatEventReplay>('replay_chat_events', {
            workspaceId,
            conversationId: activeConversation,
            afterCursor: eventSequencerRef.current.cursor(streamId),
          });
          if (replay?.events) eventSequencerRef.current.ingestReplay(replay, handleEvent);
        }
        if (useChatStore.getState().isStreaming) {
          useChatStore.getState().settleOrphanedTurn();
        }
        activeTurnIdRef.current = null;
        currentMessageKeyRef.current = null;
        assistantIdRef.current = null;
        return;
      }
      const settlement = await apiInvoke<CancelChatResponse>('cancel_chat', {
        workspaceId: currentWorkspaceIdRef.current,
        conversationId,
        expectedRootTurnId: rootTurnId,
        expectedActiveTurnId: activeTurnIdRef.current,
      });
      if (!settlement.success) {
        throw new Error(`取消请求未完成（${settlement.status}）`);
      }
      if (settlement.status === 'already_settled') {
        const workspaceId = currentWorkspaceIdRef.current;
        const streamId = chatStreamId(workspaceId, conversationId);
        try {
          const replay = await apiInvoke<ChatEventReplay>('replay_chat_events', {
            workspaceId,
            conversationId,
            messageKey: rootTurnId,
            afterCursor: eventSequencerRef.current.cursor(streamId),
          });
          if (replay?.events) eventSequencerRef.current.ingestReplay(replay, handleEvent);
        } catch (error) {
          console.warn('[TauriChat] Failed to reconcile an already-settled turn:', error);
        }
        if (useChatStore.getState().isStreaming) {
          useChatStore.getState().settleOrphanedTurn();
        }
        activeTurnIdRef.current = null;
        currentMessageKeyRef.current = null;
        assistantIdRef.current = null;
        return;
      }
      // The existing chat terminal event is the sole UI projection authority.
      // Keep refs until that event arrives so the versioned event is accepted
      // and queued input advances exactly once through `done`.
    } catch (e) {
      console.error('[TauriChat] Failed to cancel:', e);
      useToastStore.getState().addToast('error', `停止任务失败：${errorMessage(e)}`);
    }
  }, [getActiveTurnSnapshot, handleEvent, restoreActiveTurnRefs]);

  const clearQueuedMessages = useCallback(() => {
    const addressKey = visibleQueueKeyRef.current;
    if (!addressKey) return;
    const queue = queuedInputsByAddressRef.current.get(addressKey) ?? [];
    void (async () => {
      for (const item of queue.filter((candidate) => candidate.backendManaged)) {
        await apiInvoke('remove_queued_chat_input', {
          workspaceId: item.workspaceId,
          conversationId: item.conversationId,
          inputId: item.id,
        });
      }
      replaceQueue(addressKey, []);
    })().catch((error) => {
      useToastStore.getState().addToast('error', `清空排队消息失败：${errorMessage(error)}`);
    });
  }, [replaceQueue]);

  const removeQueuedMessage = useCallback(
    (id: string) => {
      const addressKey = visibleQueueKeyRef.current;
      if (!addressKey) return;
      const currentQueue = queuedInputsByAddressRef.current.get(addressKey) ?? [];
      const item = currentQueue.find((candidate) => candidate.id === id);
      if (item?.backendManaged) {
        void apiInvoke('remove_queued_chat_input', {
          workspaceId: item.workspaceId,
          conversationId: item.conversationId,
          inputId: item.id,
        })
          .then(() => {
            const latest = queuedInputsByAddressRef.current.get(addressKey) ?? [];
            replaceQueue(
              addressKey,
              latest.filter((candidate) => candidate.id !== id)
            );
          })
          .catch((error) => {
            useToastStore.getState().addToast('error', `删除排队消息失败：${errorMessage(error)}`);
          });
        return;
      }
      replaceQueue(
        addressKey,
        currentQueue.filter((candidate) => candidate.id !== id)
      );
    },
    [replaceQueue]
  );

  const reorderQueuedMessage = useCallback(
    (sourceId: string, targetId: string) => {
      const addressKey = visibleQueueKeyRef.current;
      if (!addressKey) return;
      const currentQueue = queuedInputsByAddressRef.current.get(addressKey) ?? [];
      const next = reorderById(currentQueue, sourceId, targetId);
      replaceQueue(addressKey, next);
      const addressed = next.find((item) => item.backendManaged);
      if (!addressed) return;
      void apiInvoke('reorder_queued_chat_inputs', {
        workspaceId: addressed.workspaceId,
        conversationId: addressed.conversationId,
        inputIds: next.filter((item) => item.backendManaged).map((item) => item.id),
      }).catch((error) => {
        replaceQueue(addressKey, currentQueue);
        useToastStore.getState().addToast('error', `调整排队顺序失败：${errorMessage(error)}`);
      });
    },
    [replaceQueue]
  );

  const steerQueuedMessage = useCallback(
    async (id: string) => {
      const addressKey = visibleQueueKeyRef.current;
      const queued = addressKey
        ? queuedInputsByAddressRef.current.get(addressKey)?.find((item) => item.id === id)
        : undefined;
      const conversationId = useConversationStore.getState().activeId;
      if (!queued || !conversationId) return false;
      try {
        const result = await apiInvoke<ChatSteerReceipt>('steer_chat_message', {
          workspaceId: queued.workspaceId,
          message: queued.text,
          attachments: queued.attachments,
          conversationId,
          expectedRootTurnId: currentMessageKeyRef.current,
          expectedActiveTurnId: activeTurnIdRef.current,
        });
        if (result.kind !== 'accepted' || result.phase !== 'drained') {
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
