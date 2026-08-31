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
import { workspaceIdForView } from '../lib/viewAddress';
import { handleChatEventEnvelope } from './chatEventHandler';
import { ChatEventSequencer } from './chatEventSequencer';
import { reorderById } from './queuedChat';
import type { ForegroundTurnSnapshot } from '../generated';
import type {
  Attachment,
  ChatEventEnvelope,
  ChatEventReplay,
  ConversationInputFrontier,
  ConversationInputIdentity,
  ConversationInputProjection,
  ConversationInputReceipt,
  ToolExecution,
} from '../types/api';

export type QueuedChatInput = {
  id: string;
  text: string;
  attachments?: Attachment[];
  workspaceId: string;
  conversationId: string;
  identity: ConversationInputIdentity;
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

type CancelChatResponse = {
  success: boolean;
  turn_id: string;
  status: 'completed' | 'cancelled' | 'failed' | 'already_settled';
};

const EMPTY_FRONTIER: ConversationInputFrontier = { queue_revision: 0, items: [] };

type QueuedDispatch = {
  identity: ConversationInputIdentity;
  queueRevision: number;
};

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
  const frontierRef = useRef<ConversationInputFrontier>(EMPTY_FRONTIER);
  const frontierRequestOrdinalRef = useRef(0);
  const frontierAppliedOrdinalRef = useRef(0);
  const dispatchingInputIdRef = useRef<string | null>(null);
  const pendingAdmissionRef = useRef<{
    messageKey: string;
    events: ChatEventEnvelope[];
  } | null>(null);
  const dispatchMessageRef = useRef<
    | ((
        text: string,
        attachments: Attachment[] | undefined,
        queued?: QueuedDispatch
      ) => Promise<boolean>)
    | null
  >(null);
  const [frontier, setFrontier] = useState<ConversationInputFrontier>(EMPTY_FRONTIER);

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

  const refreshFrontier = useCallback(async (workspaceId: string, conversationId: string) => {
    const generation = identityGenerationRef.current;
    frontierRequestOrdinalRef.current += 1;
    const requestOrdinal = frontierRequestOrdinalRef.current;
    const result = await apiInvoke<ConversationInputFrontier>('list_queued_chat_inputs', {
      workspaceId,
      conversationId,
    });
    const next =
      result && typeof result.queue_revision === 'number' && Array.isArray(result.items)
        ? result
        : EMPTY_FRONTIER;
    if (
      identityGenerationRef.current !== generation ||
      currentWorkspaceIdRef.current !== workspaceId ||
      useConversationStore.getState().activeId !== conversationId ||
      requestOrdinal < frontierAppliedOrdinalRef.current ||
      next.queue_revision < frontierRef.current.queue_revision
    ) {
      return next;
    }
    frontierAppliedOrdinalRef.current = requestOrdinal;
    frontierRef.current = next;
    setFrontier(next);
    return next;
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
    frontierAppliedOrdinalRef.current = frontierRequestOrdinalRef.current;

    const adoptsCurrentTurn =
      !startsNewConversation &&
      previous === null &&
      activeConversationId !== null &&
      activeTurnIdRef.current !== null;
    currentConversationIdRef.current = activeConversationId;
    frontierRef.current = EMPTY_FRONTIER;
    setFrontier(EMPTY_FRONTIER);
    if (adoptsCurrentTurn) return;

    activeTurnIdRef.current = null;
    currentMessageKeyRef.current = null;
    assistantIdRef.current = null;
    thinkingIdRef.current = null;
    isCancelledRef.current = false;
    pendingAdmissionRef.current = null;
  }, [activeConversationId, currentWorkspaceId, newConversationEpoch]);

  const dispatchNextQueued = useCallback((workspaceId: string, conversationId: string) => {
    if (
      currentWorkspaceIdRef.current !== workspaceId ||
      useConversationStore.getState().activeId !== conversationId
    ) {
      return;
    }
    const snapshot = frontierRef.current;
    const next = snapshot.items[0];
    if (!next) return;
    const inputId = next.receipt.identity.input_id;
    if (dispatchingInputIdRef.current === inputId) return;
    dispatchingInputIdRef.current = inputId;
    queueMicrotask(() => {
      const dispatch = dispatchMessageRef.current;
      if (!dispatch) {
        dispatchingInputIdRef.current = null;
        return;
      }
      void dispatch(next.payload.text, next.payload.attachments, {
        identity: next.receipt.identity,
        queueRevision: snapshot.queue_revision,
      }).finally(() => {
        if (dispatchingInputIdRef.current === inputId) {
          dispatchingInputIdRef.current = null;
        }
      });
    });
  }, []);

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
      if (event.payload.source === 'input_lifecycle') {
        handleChatEventEnvelope(event, {
          assistantIdRef,
          currentMessageKeyRef,
          currentMessageIdRef: currentMessageKeyRef,
          isCancelledRef,
          currentThinkingIdRef: thinkingIdRef,
          onInputLifecycle: (workspaceId, conversationId) => {
            void refreshFrontier(workspaceId, conversationId);
          },
        });
        return;
      }
      if (!isCurrentRunEvent(event)) return;
      rebindEventRefs(event);
      handleChatEventEnvelope(event, {
        assistantIdRef,
        currentMessageKeyRef,
        currentMessageIdRef: currentMessageKeyRef,
        isCancelledRef,
        currentThinkingIdRef: thinkingIdRef,
        onInputLifecycle: (workspaceId, conversationId) => {
          void refreshFrontier(workspaceId, conversationId);
        },
      });
      const terminalStatus =
        event.payload.source === 'turn_status' &&
        ['completed', 'failed', 'cancelled'].includes(event.payload.event.status);
      if (terminalStatus) {
        activeTurnIdRef.current = null;
        currentMessageKeyRef.current = null;
        assistantIdRef.current = null;
        thinkingIdRef.current = null;
        if (terminalStatus && event.conversation_id) {
          const conversationId = event.conversation_id;
          void refreshFrontier(event.workspace_id, conversationId).then(() => {
            dispatchNextQueued(event.workspace_id, conversationId);
          });
        }
      }
    },
    [dispatchNextQueued, isCurrentRunEvent, rebindEventRefs, refreshFrontier]
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
      // kind="subagent" -> lifecycle, usage, and complete terminal outcome;
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
              if (!snapshot && useChatStore.getState().isStreaming) {
                useChatStore.getState().clearInactiveTurnProjection();
              }
            }
          }
          if (activeConversation) {
            await refreshFrontier(currentWorkspaceId, activeConversation);
            if (!aborted && setupIdentityGeneration === identityGenerationRef.current) {
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
    refreshFrontier,
    restoreActiveTurnRefs,
  ]);

  const dispatchMessage = useCallback(
    async (text: string, attachments?: Attachment[], queued?: QueuedDispatch) => {
      const store = useChatStore.getState();
      const displayAttachments = attachments?.map((a) => ({
        name: a.name,
        mime_type: a.mime_type,
        url: `data:${a.mime_type};base64,${a.data}`,
        size: a.size,
        source: a.source,
      }));
      const messageKey =
        typeof crypto !== 'undefined' && 'randomUUID' in crypto
          ? crypto.randomUUID()
          : `chat-${Date.now()}-${Math.random().toString(36).slice(2)}`;
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
          request: {
            workspaceId,
            message: text,
            // Multimodal: forward attachments (base64-encoded) so the backend can
            // persist them and build a multimodal Message for the LLM.
            attachments: attachments && attachments.length > 0 ? attachments : undefined,
            conversationId,
            messageKey,
            inputIdentity: queued?.identity,
            expectedQueueRevision: queued?.queueRevision,
          },
        });
        const pendingEvents = pendingAdmissionRef.current?.events ?? [];
        pendingAdmissionRef.current = null;
        const outcome = chatResult.kind ?? (chatResult.success === false ? undefined : 'started');

        if (
          outcome === 'queued' ||
          outcome === 'task_run_conflict' ||
          outcome === 'interrupt_prompt'
        ) {
          await refreshFrontier(workspaceId, conversationId);
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
                const current = await refreshFrontier(workspaceId, conversationId);
                const selected = current.items.find(
                  (item) => item.receipt.identity.input_id === chatResult.input_id
                );
                if (selected) {
                  await dispatchMessageRef.current?.(text, attachments, {
                    identity: selected.receipt.identity,
                    queueRevision: current.queue_revision,
                  });
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
        const conversationId = useConversationStore.getState().activeId;
        if (conversationId) {
          void refreshFrontier(workspaceId, conversationId);
        }
        return false;
      }
    },
    [applyEvent, refreshFrontier]
  );

  dispatchMessageRef.current = dispatchMessage;

  const steerConversationInput = useCallback(
    async (queued: ConversationInputProjection, expectedQueueRevision: number) => {
      const conversationId = useConversationStore.getState().activeId;
      if (
        !conversationId ||
        conversationId !== queued.receipt.identity.address.conversation_id ||
        currentWorkspaceIdRef.current !== queued.receipt.identity.address.workspace_id
      ) {
        return false;
      }
      try {
        const result = await apiInvoke<ConversationInputReceipt>('steer_chat_message', {
          workspaceId: queued.receipt.identity.address.workspace_id,
          conversationId,
          expectedActiveTurnId: activeTurnIdRef.current,
          identity: queued.receipt.identity,
          expectedQueueRevision,
        });
        await refreshFrontier(
          queued.receipt.identity.address.workspace_id,
          queued.receipt.identity.address.conversation_id
        );
        if (!result.drained) {
          useToastStore.getState().addToast('info', '当前阶段不能插入，已保留在排队队列中');
          return false;
        }
        const displayAttachments = queued.payload.attachments.map((attachment) => ({
          name: attachment.name,
          mime_type: attachment.mime_type,
          url: `data:${attachment.mime_type};base64,${attachment.data}`,
          size: attachment.size,
          source: attachment.source,
        }));
        assistantIdRef.current = useChatStore
          .getState()
          .continueAfterSteer(
            assistantIdRef.current,
            queued.payload.text || '(附件)',
            displayAttachments
          );
        return true;
      } catch (error) {
        useToastStore.getState().addToast('error', `补充当前任务失败：${errorMessage(error)}`);
        return false;
      }
    },
    [refreshFrontier]
  );

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
        const receipt = await apiInvoke<ConversationInputReceipt>('queue_chat_input', {
          workspaceId,
          conversationId: activeConversation,
          externalId: id,
          text,
          attachments: attachments && attachments.length > 0 ? attachments : undefined,
        });
        const current = await refreshFrontier(workspaceId, activeConversation);
        const selected = current.items.find(
          (item) => item.receipt.identity.input_id === receipt.identity.input_id
        );
        if (selected) {
          await steerConversationInput(selected, current.queue_revision);
        }
        return true;
      }
      if (activeTurnIdRef.current) {
        activeTurnIdRef.current = null;
        currentMessageKeyRef.current = null;
        assistantIdRef.current = null;
      }
      return dispatchMessage(text, attachments);
    },
    [dispatchMessage, refreshFrontier, steerConversationInput]
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
          useChatStore.getState().clearInactiveTurnProjection();
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
          useChatStore.getState().clearInactiveTurnProjection();
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

  const queuedInputs: QueuedChatInput[] = frontier.items.map((item) => ({
    id: item.receipt.identity.input_id,
    text: item.payload.text,
    attachments: item.payload.attachments,
    workspaceId: item.receipt.identity.address.workspace_id,
    conversationId: item.receipt.identity.address.conversation_id,
    identity: item.receipt.identity,
  }));

  const clearQueuedMessages = useCallback(() => {
    const snapshot = frontierRef.current;
    if (snapshot.items.length === 0) return;
    void (async () => {
      for (const item of snapshot.items) {
        await apiInvoke('remove_queued_chat_input', {
          identity: item.receipt.identity,
        });
      }
      const address = snapshot.items[0]?.receipt.identity.address;
      if (address) await refreshFrontier(address.workspace_id, address.conversation_id);
    })().catch((error) => {
      useToastStore.getState().addToast('error', `清空排队消息失败：${errorMessage(error)}`);
    });
  }, [refreshFrontier]);

  const removeQueuedMessage = useCallback(
    (id: string) => {
      const item = frontierRef.current.items.find(
        (candidate) => candidate.receipt.identity.input_id === id
      );
      if (!item) return;
      void apiInvoke('remove_queued_chat_input', { identity: item.receipt.identity })
        .then(() =>
          refreshFrontier(
            item.receipt.identity.address.workspace_id,
            item.receipt.identity.address.conversation_id
          )
        )
        .catch((error) => {
          useToastStore.getState().addToast('error', `删除排队消息失败：${errorMessage(error)}`);
        });
    },
    [refreshFrontier]
  );

  const reorderQueuedMessage = useCallback(
    (sourceId: string, targetId: string) => {
      const snapshot = frontierRef.current;
      const current = snapshot.items.map((item) => ({
        id: item.receipt.identity.input_id,
        identity: item.receipt.identity,
      }));
      const next = reorderById(current, sourceId, targetId);
      const addressed = next[0]?.identity.address;
      if (!addressed) return;
      void apiInvoke('reorder_queued_chat_inputs', {
        workspaceId: addressed.workspace_id,
        conversationId: addressed.conversation_id,
        expectedQueueRevision: snapshot.queue_revision,
        inputIds: next.map((item) => item.id),
      })
        .then(() => refreshFrontier(addressed.workspace_id, addressed.conversation_id))
        .catch((error) => {
          void refreshFrontier(addressed.workspace_id, addressed.conversation_id);
          useToastStore.getState().addToast('error', `调整排队顺序失败：${errorMessage(error)}`);
        });
    },
    [refreshFrontier]
  );

  const steerQueuedMessage = useCallback(
    async (id: string) => {
      const snapshot = frontierRef.current;
      const queued = snapshot.items.find((item) => item.receipt.identity.input_id === id);
      if (!queued) return false;
      return steerConversationInput(queued, snapshot.queue_revision);
    },
    [steerConversationInput]
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
