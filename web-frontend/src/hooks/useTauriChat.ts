import { useRef, useCallback, useEffect, useState } from 'react';
import { listen } from '@tauri-apps/api/event';
import { useChatStore } from '../stores/chatStore';
import { useConversationStore } from '../stores/conversationStore';
import { useTaskRuntimeStore, type TaskRuntimeState } from '../stores/taskRuntimeStore';
import { useToastStore } from '../stores/toastStore';
import { useToolExecutionStore } from '../stores/toolExecutionStore';
import { useWorkspaceStore } from '../stores/workspaceStore';
import { isTauri, apiInvoke, errorMessage } from '../lib/tauri-bridge';
import { workspaceIdForView } from '../lib/viewAddress';
import { productDataScope, sameProductDataScope } from '../lib/productDataScope';
import { handleChatEventEnvelope } from './chatEventHandler';
import { ChatEventSequencer } from './chatEventSequencer';
import { reorderById } from './queuedChat';
import { sameHitlIdentity } from '../types/api';
import type { ForegroundTurnSnapshot } from '../generated';
import type {
  Attachment,
  HitlRequestIdentity,
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
  phase: ConversationInputReceipt['phase'];
  drained: boolean;
  channelOwned?: boolean;
  steerOutcome?: 'retained' | 'rejected';
};

// Channel inputs keep their original pump and recovery owner even when their
// conversation is opened in the GUI. Do not dispatch or rewrite their payload.
function channelOwnsInput(input: ConversationInputProjection): boolean {
  return 'channel_binding' in input.payload && input.payload.channel_binding != null;
}

/** Capture identity/target before encoding; retain the prepared payload for retries. */
export type ChatSendSubmission = {
  readonly externalId: string;
  readonly workspaceId: string;
  readonly workspaceGeneration: string;
  readonly conversationId: string | null;
  readonly newConversationEpoch: number;
  readonly payload: {
    readonly text: string;
    readonly attachments?: readonly Readonly<Attachment>[];
    readonly domainPlugin?: string;
  };
  /** The original durable receipt; canProject also fences the private turn identity. */
  readonly onPersisted?: (
    receipt: ConversationInputReceipt,
    canProject: () => boolean
  ) => void | Promise<void>;
  /** Ordinary admission proof, deliberately distinct from a durable input receipt. */
  readonly onDirectAccepted?: (canProject: () => boolean) => void | Promise<void>;
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

type PendingChatAdmission = {
  messageKey: string;
  workspaceId: string;
  conversationId: string | null;
  identityGeneration: number;
  events: ChatEventEnvelope[];
};

function chatStreamId(workspaceId: string, conversationId?: string, messageKey?: string): string {
  return JSON.stringify([workspaceId, conversationId ?? messageKey ?? '']);
}

function sameAttachments(
  left: readonly Attachment[] | undefined,
  right: readonly Attachment[] | undefined
): boolean {
  const expected = left ?? [];
  const actual = right ?? [];
  return (
    expected.length === actual.length &&
    expected.every((attachment, index) => {
      const other = actual.at(index);
      return (
        other !== undefined &&
        attachment.name === other.name &&
        attachment.mime_type === other.mime_type &&
        attachment.data === other.data &&
        attachment.size === other.size &&
        attachment.source === other.source
      );
    })
  );
}

export function useTauriChat() {
  const activeConversationId = useConversationStore((state) => state.activeId);
  const newConversationEpoch = useConversationStore((state) => state.newConversationEpoch);
  const currentWorkspaceId = useWorkspaceStore((state) => workspaceIdForView(state.current?.id));
  const currentWorkspaceGeneration = useWorkspaceStore(
    (state) => productDataScope(state.current).workspaceGeneration
  );
  const assistantIdRef = useRef<string | null>(null);
  const isCancelledRef = useRef(false);
  const activeTurnIdRef = useRef<string | null>(null);
  const terminalProjectionRevisionRef = useRef(0);
  const currentMessageKeyRef = useRef<string | null>(null);
  const currentConversationIdRef = useRef<string | null>(activeConversationId);
  const currentWorkspaceIdRef = useRef(currentWorkspaceId);
  const currentWorkspaceGenerationRef = useRef(currentWorkspaceGeneration);
  const identityGenerationRef = useRef(0);
  const previousActiveConversationIdRef = useRef(activeConversationId);
  const previousNewConversationEpochRef = useRef(newConversationEpoch);
  const thinkingIdRef = useRef<string | null>(null);
  const eventSequencerRef = useRef(new ChatEventSequencer());
  const replayingGapStreamsRef = useRef(new Set<string>());
  const frontierRef = useRef<ConversationInputFrontier>(EMPTY_FRONTIER);
  const frontierRequestOrdinalRef = useRef(0);
  const frontierAppliedOrdinalRef = useRef(0);
  const dispatchingInputIdRef = useRef<string | null>(null);
  const pendingAdmissionRef = useRef<PendingChatAdmission | null>(null);
  const pendingSteersRef = useRef(new Map<string, Promise<boolean>>());
  const steerProjectionOwnersRef = useRef(new Map<string, symbol>());
  const dispatchMessageRef = useRef<
    | ((
        text: string,
        attachments: Attachment[] | undefined,
        queued?: QueuedDispatch
      ) => Promise<boolean>)
    | null
  >(null);
  const [frontier, setFrontier] = useState<ConversationInputFrontier>(EMPTY_FRONTIER);
  const [steerOutcomes, setSteerOutcomes] = useState<
    Record<string, QueuedChatInput['steerOutcome']>
  >({});

  const captureInputProjection = useCallback(() => {
    const generation = identityGenerationRef.current;
    const workspaceScope = productDataScope(useWorkspaceStore.getState().current);
    const conversationId = useConversationStore.getState().activeId;
    const conversationEpoch = useConversationStore.getState().newConversationEpoch;
    const activeTurnId = activeTurnIdRef.current;
    const rootTurnId = currentMessageKeyRef.current;
    return () => {
      const conversation = useConversationStore.getState();
      return (
        identityGenerationRef.current === generation &&
        sameProductDataScope(
          workspaceScope,
          productDataScope(useWorkspaceStore.getState().current)
        ) &&
        currentWorkspaceIdRef.current === workspaceScope.workspaceId &&
        conversation.activeId === conversationId &&
        conversation.newConversationEpoch === conversationEpoch &&
        activeTurnIdRef.current === activeTurnId &&
        currentMessageKeyRef.current === rootTurnId
      );
    };
  }, []);

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
    setSteerOutcomes((previous) => {
      const activeIds = new Set(next.items.map((item) => item.receipt.identity.input_id));
      const retained = Object.fromEntries(
        Object.entries(previous).filter(([inputId]) => activeIds.has(inputId))
      );
      return Object.keys(retained).length === Object.keys(previous).length ? previous : retained;
    });
    return next;
  }, []);

  useEffect(() => {
    const previous = previousActiveConversationIdRef.current;
    const startsNewConversation = previousNewConversationEpochRef.current !== newConversationEpoch;
    const workspaceChanged =
      currentWorkspaceIdRef.current !== currentWorkspaceId ||
      currentWorkspaceGenerationRef.current !== currentWorkspaceGeneration;
    if (previous === activeConversationId && !startsNewConversation && !workspaceChanged) return;
    currentWorkspaceIdRef.current = currentWorkspaceId;
    currentWorkspaceGenerationRef.current = currentWorkspaceGeneration;
    previousActiveConversationIdRef.current = activeConversationId;
    previousNewConversationEpochRef.current = newConversationEpoch;
    const previousIdentityGeneration = identityGenerationRef.current;
    identityGenerationRef.current += 1;
    frontierAppliedOrdinalRef.current = frontierRequestOrdinalRef.current;

    const adoptsPendingAdmission =
      pendingAdmissionRef.current !== null &&
      pendingAdmissionRef.current.workspaceId === currentWorkspaceId &&
      pendingAdmissionRef.current.conversationId === activeConversationId &&
      pendingAdmissionRef.current.identityGeneration === previousIdentityGeneration;
    const adoptsCurrentTurn =
      !startsNewConversation &&
      !workspaceChanged &&
      previous === null &&
      activeConversationId !== null &&
      (activeTurnIdRef.current !== null || adoptsPendingAdmission);
    currentConversationIdRef.current = activeConversationId;
    frontierRef.current = EMPTY_FRONTIER;
    setFrontier(EMPTY_FRONTIER);
    setSteerOutcomes({});
    if (adoptsCurrentTurn) {
      if (adoptsPendingAdmission && pendingAdmissionRef.current) {
        pendingAdmissionRef.current.identityGeneration = identityGenerationRef.current;
      }
      return;
    }

    activeTurnIdRef.current = null;
    currentMessageKeyRef.current = null;
    assistantIdRef.current = null;
    thinkingIdRef.current = null;
    isCancelledRef.current = false;
    pendingAdmissionRef.current = null;
  }, [activeConversationId, currentWorkspaceId, currentWorkspaceGeneration, newConversationEpoch]);

  useEffect(
    () => () => {
      identityGenerationRef.current += 1;
      pendingAdmissionRef.current = null;
    },
    []
  );

  const dispatchNextQueued = useCallback((workspaceId: string, conversationId: string) => {
    if (
      currentWorkspaceIdRef.current !== workspaceId ||
      useConversationStore.getState().activeId !== conversationId
    ) {
      return;
    }
    const snapshot = frontierRef.current;
    const next = snapshot.items[0];
    if (!next || channelOwnsInput(next)) return;
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
      if (event.payload.source === 'execution') {
        if (!isCurrentStreamEvent(event)) return;
        handleChatEventEnvelope(event, {
          assistantIdRef,
          currentMessageKeyRef,
          currentMessageIdRef: currentMessageKeyRef,
          isCancelledRef,
          currentThinkingIdRef: thinkingIdRef,
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
        // 实时终态比此前发出的活跃快照更新，迟到快照不能复活这个执行槽。
        terminalProjectionRevisionRef.current += 1;
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
    [dispatchNextQueued, isCurrentRunEvent, isCurrentStreamEvent, rebindEventRefs, refreshFrontier]
  );

  const handleEvent = useCallback(
    (event: ChatEventEnvelope) => {
      const pendingAdmission = pendingAdmissionRef.current;
      if (
        pendingAdmission &&
        pendingAdmission.messageKey === event.message_id &&
        pendingAdmission.workspaceId === event.workspace_id &&
        pendingAdmission.conversationId === event.conversation_id
      ) {
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
  // 才生效。若组件在两次 `await listen()` 之间卸载, cleanup 执行时
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
      if (aborted) return;
      const unlisten = await listen<ChatEventEnvelope>('chat://event', (event) => {
        if (aborted || !isCurrentStreamEvent(event.payload)) return;
        const envelope = event.payload;
        const sequencer = eventSequencerRef.current;
        const cursor = sequencer.cursor(envelope.stream_id);
        sequencer.ingest(envelope, handleEvent);
        if (
          envelope.sequence > cursor + 1 &&
          !replayingGapStreamsRef.current.has(envelope.stream_id)
        ) {
          replayingGapStreamsRef.current.add(envelope.stream_id);
          void apiInvoke<ChatEventReplay>('replay_chat_events', {
            workspaceId: envelope.workspace_id,
            conversationId: envelope.conversation_id ?? undefined,
            messageKey: envelope.root_turn_id,
            afterCursor: cursor,
          })
            .then((replay) => {
              if (!aborted) sequencer.ingestReplay(replay, handleEvent);
            })
            .catch((error) => {
              if (!aborted) console.warn('[TauriChat] Failed to recover live event gap:', error);
            })
            .finally(() => replayingGapStreamsRef.current.delete(envelope.stream_id));
        }
      });
      // 卸载发生在两个 listen 之间: 立即注销刚注册的第一个, 不再注册第二个。
      if (aborted) {
        unlisten();
        return;
      }
      pendingCleanup.push(unlisten);
      // `execution://event` now carries only secondary committed tool summaries.
      // Run/Subagent lifecycle arrives on the canonical journaled chat stream.
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
        if (kind === 'tool') {
          const tool = payload as unknown as ToolExecution;
          useToolExecutionStore.getState().ingest(tool);
          if (payload.event === 'started' && tool.owner.kind === 'chat') {
            useChatStore.getState().recordToolStart(tool.owner.message_id, tool.id);
          }
        }
      });
      // 卸载发生在第二个 listen 之后、push 之前: 立即注销。
      if (aborted) {
        unlistenExec();
        return;
      }
      pendingCleanup.push(unlistenExec);
      try {
        const recoveryTerminalRevision = terminalProjectionRevisionRef.current;
        const snapshot = await getActiveTurnSnapshot();
        if (
          !aborted &&
          setupIdentityGeneration === identityGenerationRef.current &&
          recoveryTerminalRevision === terminalProjectionRevisionRef.current
        ) {
          restoreActiveTurnRefs(snapshot);
          const activeConversation = useConversationStore.getState().activeId;
          const snapshotIsMessageScope = snapshot?.conversation_id.startsWith('message:') ?? false;
          const conversationId = snapshotIsMessageScope
            ? undefined
            : (activeConversation ?? snapshot?.conversation_id);
          const messageKey = snapshot?.root_turn_id;
          if (conversationId || messageKey) {
            const streamId = chatStreamId(currentWorkspaceId, conversationId, messageKey);
            const cursor = eventSequencerRef.current.cursor(streamId);
            const replay = await apiInvoke<ChatEventReplay>('replay_chat_events', {
              workspaceId: currentWorkspaceId,
              conversationId,
              messageKey,
              afterCursor: cursor,
            });
            if (!aborted && setupIdentityGeneration === identityGenerationRef.current) {
              eventSequencerRef.current.ingestReplay(replay, handleEvent);
              if (cursor > 0) {
                const projectionReplay = await apiInvoke<ChatEventReplay>('replay_chat_events', {
                  workspaceId: currentWorkspaceId,
                  conversationId,
                  messageKey,
                  afterCursor: 0,
                });
                if (!aborted && setupIdentityGeneration === identityGenerationRef.current) {
                  for (const event of projectionReplay.events) {
                    if (event.payload.source === 'execution') applyEvent(event);
                  }
                }
              }
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
    currentWorkspaceGeneration,
    newConversationEpoch,
    applyEvent,
    dispatchNextQueued,
    getActiveTurnSnapshot,
    handleEvent,
    isCurrentStreamEvent,
    refreshFrontier,
    restoreActiveTurnRefs,
  ]);

  const dispatchMessage = useCallback(
    async (
      text: string,
      attachments?: Attachment[],
      queued?: QueuedDispatch,
      domainPlugin?: string,
      onDirectAccepted?: ChatSendSubmission['onDirectAccepted']
    ) => {
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
      const workspaceScope = productDataScope(useWorkspaceStore.getState().current);
      const conversationEpoch = useConversationStore.getState().newConversationEpoch;
      let conversationId = useConversationStore.getState().activeId;
      if (
        queued &&
        (queued.identity.address.workspace_id !== workspaceId ||
          queued.identity.address.conversation_id !== conversationId)
      )
        return false;
      let admission: PendingChatAdmission | null = null;
      const isOriginalView = () => {
        const conversation = useConversationStore.getState();
        return (
          sameProductDataScope(
            workspaceScope,
            productDataScope(useWorkspaceStore.getState().current)
          ) &&
          currentWorkspaceIdRef.current === workspaceId &&
          conversation.activeId === conversationId &&
          conversation.newConversationEpoch === conversationEpoch &&
          !conversation.isLoading
        );
      };
      const ownsAdmissionProjection = () =>
        admission !== null &&
        pendingAdmissionRef.current === admission &&
        admission.identityGeneration === identityGenerationRef.current &&
        isOriginalView();
      const notifyDirectAccepted = (canProject: () => boolean) => {
        const report = (error: unknown) => {
          if (canProject())
            useToastStore
              .getState()
              .addToast('warning', `消息已接受，界面状态同步失败：${errorMessage(error)}`);
        };
        try {
          void Promise.resolve(onDirectAccepted?.(canProject)).catch(report);
        } catch (error) {
          report(error);
        }
      };

      try {
        identityGenerationRef.current += 1;
        isCancelledRef.current = false;
        thinkingIdRef.current = null;
        // Capture the owner before the first await. Stop, unmount or another
        // submission may invalidate it while canonical creation is pending.
        admission = {
          messageKey,
          workspaceId,
          conversationId,
          identityGeneration: identityGenerationRef.current,
          events: [],
        };
        if (isOriginalView()) pendingAdmissionRef.current = admission;

        // TaskRuntime runs are keyed by conversation_id. On the first turn there
        // is no active conversation yet, so create it before routing; otherwise
        // the backend falls back to a message-scoped id and the right rail loses
        // the run as soon as the conversation is later saved as conv-*.
        if (!conversationId) {
          const savedAddress = await useConversationStore.getState().saveCurrent(
            [
              {
                id: messageKey,
                role: 'user',
                content: text || '(附件)',
                timestamp: Date.now(),
                attachments: displayAttachments,
              },
            ],
            (address) => {
              if (
                admission &&
                pendingAdmissionRef.current === admission &&
                admission.identityGeneration === identityGenerationRef.current &&
                isOriginalView()
              ) {
                admission.conversationId = address.conversation_id;
              }
            }
          );
          if (!savedAddress || savedAddress.workspace_id !== workspaceId) {
            throw new Error('创建会话失败，无法启动 TaskRuntime。');
          }
          conversationId = savedAddress.conversation_id;
        }

        if (
          !conversationId ||
          !sameProductDataScope(
            workspaceScope,
            productDataScope(useWorkspaceStore.getState().current)
          )
        ) {
          throw new Error('创建会话失败，无法启动 TaskRuntime。');
        }
        admission.conversationId = conversationId;
        if (ownsAdmissionProjection()) {
          currentConversationIdRef.current = conversationId;
        }
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
            ...(domainPlugin === undefined ? {} : { domainPlugin }),
          },
        });
        const pendingEvents = admission.events;
        const outcome = chatResult.kind ?? (chatResult.success === false ? undefined : 'started');

        // Backend admission remains bound to its original address. A departed
        // view must not consume its buffered events or overwrite the new view.
        if (!ownsAdmissionProjection()) {
          if (pendingAdmissionRef.current === admission) pendingAdmissionRef.current = null;
          if (outcome === 'started') notifyDirectAccepted(() => false);
          return (
            outcome === 'started' ||
            outcome === 'queued' ||
            outcome === 'task_run_conflict' ||
            outcome === 'interrupt_prompt'
          );
        }

        if (
          outcome === 'queued' ||
          outcome === 'task_run_conflict' ||
          outcome === 'interrupt_prompt'
        ) {
          await refreshFrontier(workspaceId, conversationId);
          if (!ownsAdmissionProjection()) {
            if (pendingAdmissionRef.current === admission) pendingAdmissionRef.current = null;
            return true;
          }
          pendingAdmissionRef.current = null;
          for (const event of pendingEvents) applyEvent(event);
          if (outcome !== 'queued' && chatResult.run_id && chatResult.goal) {
            const admittedConversationId = conversationId;
            const admittedRunId = chatResult.run_id;
            const prompt: NonNullable<TaskRuntimeState['interruptPrompt']> = {
              runId: admittedRunId,
              goal: chatResult.goal,
              newMessage: chatResult.new_message ?? text,
              resolve: async (action) => {
                const ownsPrompt = () =>
                  isOriginalView() && useTaskRuntimeStore.getState().interruptPrompt === prompt;
                if (!ownsPrompt()) return;
                if (action === 'continue') {
                  // The conflict result owns this run; a historical inspector never supplies its identity.
                  const receipt = await useTaskRuntimeStore.getState().resumeTaskRun({
                    workspace_id: workspaceId,
                    workspace_generation: workspaceScope.workspaceGeneration,
                    run_id: admittedRunId,
                  });
                  if ((!receipt || receipt.run_id !== admittedRunId) && ownsPrompt()) {
                    throw new Error(
                      useTaskRuntimeStore.getState().error ?? '后端未确认此运行的恢复请求'
                    );
                  }
                  if (receipt?.run_id === admittedRunId && ownsPrompt()) {
                    useTaskRuntimeStore.getState().dismissInterruptPrompt();
                  }
                  return;
                }
                if (action === 'edit') {
                  useTaskRuntimeStore.getState().dismissInterruptPrompt();
                  return;
                }
                await apiInvoke('cancel_task_run', {
                  workspaceId,
                  workspaceGeneration: workspaceScope.workspaceGeneration,
                  runId: admittedRunId,
                });
                if (!ownsPrompt()) return;
                useTaskRuntimeStore.getState().dismissInterruptPrompt();
                const current = await refreshFrontier(workspaceId, admittedConversationId);
                if (!isOriginalView() || useTaskRuntimeStore.getState().interruptPrompt !== null)
                  return;
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
            };
            useTaskRuntimeStore.getState().openInterruptPrompt(prompt);
          }
          currentMessageKeyRef.current = null;
          activeTurnIdRef.current = null;
          assistantIdRef.current = null;
          return true;
        }

        if (outcome !== 'started') {
          throw new Error('后端未接受本次消息');
        }

        pendingAdmissionRef.current = null;
        store.addUserMessage(text || '(附件)', displayAttachments);
        currentMessageKeyRef.current = chatResult.root_turn_id ?? messageKey;
        activeTurnIdRef.current = chatResult.active_turn_id ?? messageKey;
        assistantIdRef.current = store.startAssistantMessage(currentMessageKeyRef.current);
        notifyDirectAccepted(captureInputProjection());
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
        const mayProject = admission ? ownsAdmissionProjection() : isOriginalView();
        if (pendingAdmissionRef.current === admission) pendingAdmissionRef.current = null;
        console.error('[TauriChat] Failed to send message:', e);
        if (mayProject) {
          useToastStore.getState().addToast('error', `发送失败：${errorMessage(e)}`);
          assistantIdRef.current = null;
          currentMessageKeyRef.current = null;
          activeTurnIdRef.current = null;
          if (conversationId) {
            void refreshFrontier(workspaceId, conversationId).catch((error) => {
              if (isOriginalView())
                console.warn('[TauriChat] Failed to refresh input state:', error);
            });
          }
        }
        return false;
      }
    },
    [applyEvent, captureInputProjection, refreshFrontier]
  );

  dispatchMessageRef.current = dispatchMessage;

  const steerConversationInput = useCallback(
    async (queued: ConversationInputProjection, expectedQueueRevision: number) => {
      if (channelOwnsInput(queued)) return false;
      const conversationId = useConversationStore.getState().activeId;
      const workspaceScope = productDataScope(useWorkspaceStore.getState().current);
      if (
        !conversationId ||
        conversationId !== queued.receipt.identity.address.conversation_id ||
        currentWorkspaceIdRef.current !== queued.receipt.identity.address.workspace_id ||
        workspaceScope.workspaceId !== queued.receipt.identity.address.workspace_id ||
        currentWorkspaceGenerationRef.current !== workspaceScope.workspaceGeneration
      ) {
        return false;
      }
      const activeTurnId = activeTurnIdRef.current;
      const ownsTarget = captureInputProjection();
      // Enter自动投递与队列按钮可同时观察同一输入。复用准确版本/原turn的
      // 在途回执，避免第二次CAS拒绝覆盖第一次仍待drain的真实状态。
      const projectionKey = JSON.stringify([
        identityGenerationRef.current,
        workspaceScope.workspaceGeneration,
        queued.receipt.identity.address.workspace_id,
        queued.receipt.identity.address.conversation_id,
        queued.receipt.identity.input_id,
        activeTurnId,
        currentMessageKeyRef.current,
      ]);
      const pendingKey = JSON.stringify([
        projectionKey,
        queued.receipt.identity.revision,
        queued.receipt.identity.payload_sha256,
      ]);
      const pending = pendingSteersRef.current.get(pendingKey);
      if (pending) return pending;
      const projectionOwner = Symbol('steer-projection');
      steerProjectionOwnersRef.current.set(projectionKey, projectionOwner);
      // 新版本开始观察后，旧版本的结果只返回旧调用方，不覆盖当前输入。
      const isCurrentSteer = () => {
        const current = frontierRef.current.items.find(
          (item) => item.receipt.identity.input_id === queued.receipt.identity.input_id
        );
        return (
          ownsTarget() &&
          steerProjectionOwnersRef.current.get(projectionKey) === projectionOwner &&
          (!current ||
            (current.receipt.identity.revision === queued.receipt.identity.revision &&
              current.receipt.identity.payload_sha256 === queued.receipt.identity.payload_sha256))
        );
      };
      setSteerOutcomes((previous) => {
        if (!(queued.receipt.identity.input_id in previous)) return previous;
        const next = { ...previous };
        delete next[queued.receipt.identity.input_id];
        return next;
      });
      const operation = (async () => {
        try {
          const result = await apiInvoke<ConversationInputReceipt>('steer_chat_message', {
            workspaceId: queued.receipt.identity.address.workspace_id,
            conversationId,
            expectedActiveTurnId: activeTurnId,
            identity: queued.receipt.identity,
            expectedQueueRevision,
          });
          // The original journal owns delivery even after focus changes. A late
          // receipt must not project into another turn or a reopened workspace.
          if (!isCurrentSteer()) return result.drained;
          await refreshFrontier(
            queued.receipt.identity.address.workspace_id,
            queued.receipt.identity.address.conversation_id
          );
          if (!isCurrentSteer()) return result.drained;
          if (!result.drained) {
            setSteerOutcomes((previous) => ({
              ...previous,
              [queued.receipt.identity.input_id]: 'retained',
            }));
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
          if (isCurrentSteer()) {
            setSteerOutcomes((previous) => ({
              ...previous,
              [queued.receipt.identity.input_id]: 'rejected',
            }));
            useToastStore.getState().addToast('error', `补充当前任务失败：${errorMessage(error)}`);
          }
          return false;
        }
      })();
      pendingSteersRef.current.set(pendingKey, operation);
      try {
        return await operation;
      } finally {
        if (pendingSteersRef.current.get(pendingKey) === operation)
          pendingSteersRef.current.delete(pendingKey);
        if (steerProjectionOwnersRef.current.get(projectionKey) === projectionOwner)
          steerProjectionOwnersRef.current.delete(projectionKey);
      }
    },
    [captureInputProjection, refreshFrontier]
  );

  const sendMessage = useCallback(
    async (
      text: string,
      attachments?: Attachment[],
      domainPlugin?: string,
      submission?: ChatSendSubmission
    ) => {
      const conversation = useConversationStore.getState();
      const activeConversation = conversation.activeId;
      const workspaceScope = productDataScope(useWorkspaceStore.getState().current);
      if (
        submission &&
        (submission.externalId.trim().length === 0 ||
          useWorkspaceStore.getState().isLoading ||
          !sameProductDataScope(submission, workspaceScope) ||
          currentWorkspaceIdRef.current !== workspaceScope.workspaceId ||
          currentWorkspaceGenerationRef.current !== workspaceScope.workspaceGeneration ||
          submission.conversationId !== activeConversation ||
          submission.newConversationEpoch !== conversation.newConversationEpoch)
      )
        return false;
      if (
        submission &&
        (submission.payload.text !== text ||
          !sameAttachments(submission.payload.attachments, attachments) ||
          submission.payload.domainPlugin !== domainPlugin)
      ) {
        useToastStore
          .getState()
          .addToast('error', '本次提交的内容或领域已改变，请使用新的草稿提交身份');
        return false;
      }
      const belongsToActiveConversation = activeConversation
        ? currentConversationIdRef.current === activeConversation
        : currentConversationIdRef.current === null && currentMessageKeyRef.current !== null;
      if (activeTurnIdRef.current && belongsToActiveConversation) {
        const id =
          submission?.externalId ??
          (typeof crypto !== 'undefined' && 'randomUUID' in crypto
            ? crypto.randomUUID()
            : `queued-${Date.now()}-${Math.random().toString(36).slice(2)}`);
        if (!activeConversation) return false;
        const workspaceId = currentWorkspaceIdRef.current;
        const canProject = captureInputProjection();
        const onPersisted = submission?.onPersisted;
        const payloadAttachments = attachments?.map((attachment) => ({ ...attachment }));
        const reportObservationError = (error: unknown) => {
          if (canProject()) {
            useToastStore
              .getState()
              .addToast('warning', `输入已保存，后续状态同步失败：${errorMessage(error)}`);
          }
        };
        let receipt: ConversationInputReceipt;
        try {
          receipt = await apiInvoke<ConversationInputReceipt>('queue_chat_input', {
            workspaceId,
            conversationId: activeConversation,
            externalId: id,
            text,
            attachments:
              payloadAttachments && payloadAttachments.length > 0 ? payloadAttachments : undefined,
            ...(domainPlugin === undefined ? {} : { domainPlugin }),
          });
          if (
            !receipt?.identity ||
            receipt.identity.address.workspace_id !== workspaceId ||
            receipt.identity.address.conversation_id !== activeConversation ||
            !receipt.identity.input_id ||
            !receipt.identity.payload_sha256 ||
            !Number.isInteger(receipt.identity.revision) ||
            receipt.identity.revision < 1 ||
            !Number.isInteger(receipt.queue_revision) ||
            receipt.queue_revision < 0 ||
            typeof receipt.phase !== 'string'
          ) {
            throw new Error('后端没有返回本次输入的完整持久回执');
          }
        } catch (error) {
          if (canProject())
            useToastStore.getState().addToast('error', `发送失败：${errorMessage(error)}`);
          return false;
        }
        // A successful queue admission is the durable fact. Never delay it on
        // frontier/steer observation, or downgrade it if a view callback fails.
        try {
          void Promise.resolve(onPersisted?.(receipt, canProject)).catch(reportObservationError);
        } catch (error) {
          reportObservationError(error);
        }
        if (canProject()) {
          void refreshFrontier(workspaceId, activeConversation)
            .then((current) => {
              if (!canProject()) return;
              const selected = current.items.find(
                (item) => item.receipt.identity.input_id === receipt.identity.input_id
              );
              // Preserve the original explicit-domain admission path. A queued
              // selection cannot silently change the active TaskRun binding.
              if (selected && domainPlugin === undefined) {
                return steerConversationInput(selected, current.queue_revision);
              }
            })
            .catch(reportObservationError);
        }
        return true;
      }
      if (activeTurnIdRef.current) {
        activeTurnIdRef.current = null;
        currentMessageKeyRef.current = null;
        assistantIdRef.current = null;
      }
      return dispatchMessage(
        text,
        attachments,
        undefined,
        domainPlugin,
        submission?.onDirectAccepted
      );
    },
    [captureInputProjection, dispatchMessage, refreshFrontier, steerConversationInput]
  );

  const sendApproval = useCallback(
    async (identity: HitlRequestIdentity, approved: boolean, reason?: string, scope?: string) => {
      try {
        const receipt = await apiInvoke<{ submitted: boolean; response_target: string }>(
          'send_approval_response',
          {
            workspaceId: identity.scope.workspace_id,
            conversationId: identity.scope.conversation_id,
            expectedRootTurnId: identity.scope.root_turn_id,
            expectedActiveTurnId: identity.scope.turn_id,
            requestId: identity.request_id,
            responseTarget: identity.response_target,
            requestScope: identity.scope,
            requestKind: identity.kind,
            approved,
            reason,
            scope,
          }
        );
        if (receipt?.submitted !== true || receipt.response_target !== identity.response_target) {
          throw new Error('HITL response submission was not acknowledged');
        }
        useChatStore.getState().removeHitlRequest(identity);
      } catch (e) {
        console.error('[TauriChat] Failed to send approval:', e);
        throw e;
      }
    },
    []
  );

  const sendInput = useCallback(
    async (identity: HitlRequestIdentity, text: string) => {
      const ownsPresentation = captureInputProjection();
      try {
        const receipt = await apiInvoke<{ submitted: boolean; response_target: string }>(
          'send_input_response',
          {
            workspaceId: identity.scope.workspace_id,
            conversationId: identity.scope.conversation_id,
            expectedRootTurnId: identity.scope.root_turn_id,
            expectedActiveTurnId: identity.scope.turn_id,
            requestId: identity.request_id,
            responseTarget: identity.response_target,
            requestScope: identity.scope,
            requestKind: identity.kind,
            text,
          }
        );
        if (receipt?.submitted !== true || receipt.response_target !== identity.response_target) {
          throw new Error('HITL response submission was not acknowledged');
        }
        useChatStore.getState().removeHitlRequest(identity);
        return true;
      } catch (e) {
        console.error('[TauriChat] Failed to send input:', e);
        // 原调用仍返回真实拒绝；迟到失败不污染换代后的视图或已由其它入口释放的请求。
        if (
          ownsPresentation() &&
          useChatStore
            .getState()
            .pendingHitlRequests.some((request) => sameHitlIdentity(request.identity, identity))
        )
          useToastStore.getState().addToast('error', `回复提交失败：${errorMessage(e)}`);
        return false;
      }
    },
    [captureInputProjection]
  );

  const sendSelection = useCallback(
    async (identity: HitlRequestIdentity, selection: string, instructions?: string) => {
      const ownsPresentation = captureInputProjection();
      try {
        const receipt = await apiInvoke<{ submitted: boolean; response_target: string }>(
          'send_selection_response',
          {
            workspaceId: identity.scope.workspace_id,
            conversationId: identity.scope.conversation_id,
            expectedRootTurnId: identity.scope.root_turn_id,
            expectedActiveTurnId: identity.scope.turn_id,
            requestId: identity.request_id,
            responseTarget: identity.response_target,
            requestScope: identity.scope,
            requestKind: identity.kind,
            selection,
            instructions,
          }
        );
        if (receipt?.submitted !== true || receipt.response_target !== identity.response_target) {
          throw new Error('HITL response submission was not acknowledged');
        }
        useChatStore.getState().removeHitlRequest(identity);
        return true;
      } catch (e) {
        console.error('[TauriChat] Failed to send selection:', e);
        if (
          ownsPresentation() &&
          useChatStore
            .getState()
            .pendingHitlRequests.some((request) => sameHitlIdentity(request.identity, identity))
        )
          useToastStore.getState().addToast('error', `回复提交失败：${errorMessage(e)}`);
        return false;
      }
    },
    [captureInputProjection]
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
    phase: item.receipt.phase,
    drained: item.receipt.drained,
    channelOwned: channelOwnsInput(item),
    steerOutcome: steerOutcomes[item.receipt.identity.input_id],
  }));

  const clearQueuedMessages = useCallback(() => {
    const snapshot = frontierRef.current;
    if (snapshot.items.length === 0) return;
    void (async () => {
      for (const item of snapshot.items) {
        if (channelOwnsInput(item)) continue;
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
      if (!item || channelOwnsInput(item)) return;
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

  const replaceQueuedMessage = useCallback(
    async (id: string, text: string) => {
      const snapshot = frontierRef.current;
      const item = snapshot.items.find((candidate) => candidate.receipt.identity.input_id === id);
      if (
        !item ||
        channelOwnsInput(item) ||
        (!text.trim() && item.payload.attachments.length === 0)
      )
        return false;
      const isCurrentEdit = captureInputProjection();
      try {
        await apiInvoke<ConversationInputReceipt>('replace_queued_chat_input', {
          identity: item.receipt.identity,
          expectedQueueRevision: snapshot.queue_revision,
          text,
          attachments: item.payload.attachments,
          domainPlugin: item.payload.domain_plugin ?? null,
        });
        if (!isCurrentEdit()) return false;
        setSteerOutcomes((previous) => {
          if (!(id in previous)) return previous;
          const next = { ...previous };
          delete next[id];
          return next;
        });
        await refreshFrontier(
          item.receipt.identity.address.workspace_id,
          item.receipt.identity.address.conversation_id
        );
        return isCurrentEdit();
      } catch (error) {
        if (isCurrentEdit())
          useToastStore.getState().addToast('error', `编辑排队消息失败：${errorMessage(error)}`);
        return false;
      }
    },
    [captureInputProjection, refreshFrontier]
  );

  const reorderQueuedMessage = useCallback(
    (sourceId: string, targetId: string) => {
      const snapshot = frontierRef.current;
      if (
        snapshot.items.some(
          (item) =>
            [sourceId, targetId].includes(item.receipt.identity.input_id) && channelOwnsInput(item)
        )
      )
        return;
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
    replaceQueuedMessage,
    reorderQueuedMessage,
    steerQueuedMessage,
  };
}
