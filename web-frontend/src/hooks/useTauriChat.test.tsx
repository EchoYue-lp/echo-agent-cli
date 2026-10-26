import type { HitlRequestIdentity } from '../types/api';
// @vitest-environment jsdom
import { act, cleanup, renderHook, waitFor } from '@testing-library/react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { useConversationStore } from '../stores/conversationStore';
import { useChatStore } from '../stores/chatStore';
import { useToastStore } from '../stores/toastStore';
import { useTaskRuntimeStore } from '../stores/taskRuntimeStore';
import { subagentRunStoreKey, useSubagentRunStore } from '../stores/subagentRunStore';
import type { ExecEvent, TaskRun, TaskRunResumeReceipt } from '../generated';
import type { Workspace } from '../api/endpoints';
import { useWorkspaceStore } from '../stores/workspaceStore';
import type {
  AgentEvent,
  ChatEventEnvelope,
  ChatEventReplay,
  ConversationInputFact,
  ConversationInputFrontier,
  ConversationInputProjection,
  ConversationInputReceipt,
  Attachment,
} from '../types/api';
import { resetChatEventCursorsForTest } from './chatEventSequencer';
import { useTauriChat, type ChatSendSubmission } from './useTauriChat';

const mocks = vi.hoisted(() => ({
  apiInvoke: vi.fn(),
  listen: vi.fn(),
  listeners: new Map<string, (event: { payload: unknown }) => void>(),
}));

const emptyReplay = (): ChatEventReplay => ({
  events: [],
  retained_earliest_cursor: null,
  returned_earliest_cursor: null,
  latest_cursor: 0,
  truncated: false,
});

function deferred<T>() {
  let resolve: (value: T) => void = () => undefined;
  let reject: (reason: Error) => void = () => undefined;
  const promise = new Promise<T>((resolvePromise, rejectPromise) => {
    resolve = resolvePromise;
    reject = rejectPromise;
  });
  return { promise, resolve, reject };
}

function draftSubmission(
  text: string,
  onPersisted?: ChatSendSubmission['onPersisted'],
  attachments?: Attachment[],
  domainPlugin?: string
): ChatSendSubmission {
  const workspace = useWorkspaceStore.getState().current;
  const conversation = useConversationStore.getState();
  return {
    externalId: 'fixed-draft-submission',
    workspaceId: workspace?.id ?? 'global',
    workspaceGeneration: workspace?.product_data_generation ?? 'global',
    conversationId: conversation.activeId,
    newConversationEpoch: conversation.newConversationEpoch,
    payload: { text, attachments, domainPlugin },
    onPersisted,
  };
}

const inputProjection = (
  inputId: string,
  conversationId: string,
  text: string,
  drained = false
): ConversationInputProjection => ({
  receipt: {
    identity: {
      address: { workspace_id: 'global', conversation_id: conversationId },
      input_id: inputId,
      revision: 1,
      payload_sha256: `sha-${inputId}`,
    },
    phase: drained ? 'drained' : 'persisted',
    attempt: null,
    attempt_id: null,
    turn_id: null,
    outcome: null,
    drained,
    reason: null,
    duplicate: false,
    queue_revision: 1,
  },
  payload: {
    text,
    attachments: [],
    submitted_at_ms: 1,
    payload_sha256: `sha-${inputId}`,
  },
});

let queuedFrontier: ConversationInputFrontier = { queue_revision: 0, items: [] };

const agentEnvelope = (
  turnId: string,
  sequence: number,
  payload: AgentEvent,
  conversationId: string | null,
  messageId: string
): ChatEventEnvelope => ({
  schema_version: 2,
  workspace_id: 'global',
  event_id: `chat-event-${sequence}`,
  content_hash: `content-hash-${sequence}`,
  sequence,
  stream_id: JSON.stringify(['global', conversationId ?? messageId]),
  conversation_id: conversationId,
  root_turn_id: messageId,
  turn_id: turnId,
  message_id: messageId,
  timestamp: '2026-08-18T00:00:00Z',
  payload: {
    source: 'agent',
    event: {
      schema_version: 4,
      event_id: `agent-event-${sequence}`,
      content_hash: `agent-hash-${sequence}`,
      sequence,
      stream_id: `chat:${turnId}`,
      conversation_id: conversationId,
      run_id: null,
      turn_id: turnId,
      message_id: messageId,
      execution_id: null,
      parent_event_id: null,
      timestamp: '2026-08-18T00:00:00Z',
      payload,
    },
  },
});

const turnStatusEnvelope = (
  turnId: string,
  messageId: string,
  sequence: number,
  status: 'running' | 'completed' | 'failed' | 'cancelled',
  conversationId: string
): ChatEventEnvelope => ({
  schema_version: 2,
  workspace_id: 'global',
  event_id: `chat-status-${sequence}`,
  content_hash: `status-hash-${sequence}`,
  sequence,
  stream_id: JSON.stringify(['global', conversationId]),
  conversation_id: conversationId,
  root_turn_id: messageId,
  turn_id: turnId,
  message_id: messageId,
  timestamp: '2026-08-18T00:00:00Z',
  payload: { source: 'turn_status', event: { status } },
});

const executionEnvelope = (
  messageId: string,
  sequence: number,
  conversationId: string,
  event: ExecEvent
): ChatEventEnvelope => ({
  schema_version: 2,
  workspace_id: event.workspace_id,
  event_id: `chat-execution-${sequence}`,
  content_hash: `execution-hash-${sequence}`,
  sequence,
  stream_id: JSON.stringify([event.workspace_id, conversationId]),
  conversation_id: conversationId,
  root_turn_id: messageId,
  turn_id: messageId,
  message_id: messageId,
  timestamp: '2026-08-18T00:00:00Z',
  payload: { source: 'execution', event },
});

const inputLifecycleEnvelope = (
  sequence: number,
  phase: 'persisted' | 'attempt_started',
  conversationId: string,
  inputId: string
): ChatEventEnvelope => {
  const identity = {
    address: { workspace_id: 'global', conversation_id: conversationId },
    input_id: inputId,
    revision: 1,
    payload_sha256: `sha-${inputId}`,
  };
  const event: ConversationInputFact =
    phase === 'persisted'
      ? {
          phase,
          identity,
          payload: {
            text: inputId,
            attachments: [],
            submitted_at_ms: 1,
            payload_sha256: `sha-${inputId}`,
          },
        }
      : {
          phase,
          attempt: {
            identity,
            attempt: 1,
            attempt_id: `attempt-${inputId}`,
            turn_id: inputId,
          },
          started_at_ms: 1,
        };
  return {
    schema_version: 2,
    workspace_id: 'global',
    event_id: `input-${phase}-${sequence}`,
    content_hash: `input-hash-${sequence}`,
    sequence,
    stream_id: JSON.stringify(['global', conversationId]),
    conversation_id: conversationId,
    root_turn_id: inputId,
    turn_id: inputId,
    message_id: inputId,
    timestamp: '2026-08-18T00:00:00Z',
    payload: { source: 'input_lifecycle', event },
  };
};

vi.mock('../lib/tauri-bridge', () => ({
  apiInvoke: mocks.apiInvoke,
  errorMessage: (error: unknown) => String(error),
  isTauri: () => true,
}));

vi.mock('@tauri-apps/api/event', () => ({
  listen: mocks.listen,
}));

const hitlIdentity = (request_id: string): HitlRequestIdentity => ({
  request_id,
  response_target: `target-${request_id}`,
  kind: 'approval',
  scope: {
    workspace_id: 'original-workspace',
    host_generation: 'original-generation',
    conversation_id: 'original-conversation',
    root_turn_id: 'original-root',
    turn_id: 'original-turn',
  },
});

describe('useTauriChat foreground turn recovery', () => {
  afterEach(cleanup);
  const activeSnapshot = {
    workspace_id: 'global',
    surface: 'gui',
    conversation_id: 'conversation-before-remount',
    root_turn_id: 'root-turn-before-remount',
    active_turn_id: 'continuation-turn-before-remount',
    run_id: null,
    cancellation_requested: false,
  };

  beforeEach(() => {
    resetChatEventCursorsForTest();
    mocks.apiInvoke.mockReset();
    mocks.listen.mockReset();
    mocks.listeners.clear();
    mocks.listen.mockImplementation(
      async (eventName: string, callback: (event: { payload: unknown }) => void) => {
        mocks.listeners.set(eventName, callback);
        return () => mocks.listeners.delete(eventName);
      }
    );
    useWorkspaceStore.setState({ current: null, isLoading: false });
    useConversationStore.setState({ activeId: null, newConversationEpoch: 0 });
    useChatStore.getState().clearMessages();
    useChatStore.setState({ pendingHitlRequests: [] });
    useChatStore.getState().setRunStatus('running');
    useToastStore.getState().clearAll();
    useTaskRuntimeStore.getState().reset();
    useSubagentRunStore.getState().clear();
    queuedFrontier = { queue_revision: 0, items: [] };
    mocks.apiInvoke.mockImplementation(async (command: string, args?: Record<string, unknown>) => {
      if (command === 'get_active_chat_turn') {
        return activeSnapshot;
      }
      if (command === 'replay_chat_events') return emptyReplay();
      if (command === 'list_queued_chat_inputs') return queuedFrontier;
      if (command === 'queue_chat_input') {
        const projection = inputProjection(
          String(args?.externalId ?? 'queued-input'),
          String(args?.conversationId ?? ''),
          String(args?.text ?? '')
        );
        queuedFrontier = {
          queue_revision: queuedFrontier.queue_revision + 1,
          items: [...queuedFrontier.items, projection],
        };
        return projection.receipt;
      }
      return { success: true, turn_id: activeSnapshot.root_turn_id, status: 'cancelled' };
    });
  });

  it('restores the real message scope after remount and cancels that exact turn', async () => {
    useConversationStore.setState({ activeId: activeSnapshot.conversation_id });
    const first = renderHook(() => useTauriChat());
    await waitFor(() => {
      expect(mocks.apiInvoke).toHaveBeenCalledWith('get_active_chat_turn', {
        workspaceId: 'global',
        conversationId: activeSnapshot.conversation_id,
      });
    });
    first.unmount();

    mocks.apiInvoke.mockClear();
    const remounted = renderHook(() => useTauriChat());
    await waitFor(() => {
      expect(mocks.apiInvoke).toHaveBeenCalledWith('get_active_chat_turn', {
        workspaceId: 'global',
        conversationId: activeSnapshot.conversation_id,
      });
    });
    await act(async () => {
      await remounted.result.current.cancel();
    });
    expect(mocks.apiInvoke).toHaveBeenCalledWith('cancel_chat', {
      workspaceId: 'global',
      conversationId: activeSnapshot.conversation_id,
      expectedRootTurnId: activeSnapshot.root_turn_id,
      expectedActiveTurnId: activeSnapshot.active_turn_id,
    });
    remounted.unmount();
  });

  it('does not adopt another conversation turn from a blank new-chat view', async () => {
    mocks.apiInvoke.mockImplementation(async (command: string, args?: Record<string, unknown>) => {
      if (command === 'get_active_chat_turn') {
        return args?.conversationId ? null : activeSnapshot;
      }
      if (command === 'create_conversation') return { kind: 'primary', id: 'conversation-new' };
      if (command === 'list_conversations') return [];
      if (command === 'send_chat_message') return { success: true };
      if (command === 'replay_chat_events') return emptyReplay();
      return null;
    });

    const hook = renderHook(() => useTauriChat());
    await waitFor(() => {
      expect(mocks.listeners.has('chat://event')).toBe(true);
    });
    await act(async () => {
      await hook.result.current.sendMessage('start an independent conversation');
    });

    expect(mocks.apiInvoke).not.toHaveBeenCalledWith('get_active_chat_turn', {
      workspaceId: 'global',
      conversationId: undefined,
    });
    expect(mocks.apiInvoke).toHaveBeenCalledWith('send_chat_message', {
      request: expect.objectContaining({ message: 'start an independent conversation' }),
    });
    expect(hook.result.current.queuedInputs).toEqual([]);
    hook.unmount();
  });

  it('does not create a streaming placeholder before backend admission', async () => {
    let resolveSend: (value: unknown) => void = () => undefined;
    const sendResult = new Promise((resolve) => {
      resolveSend = resolve;
    });
    useConversationStore.setState({ activeId: 'conversation-admission' });
    useChatStore.getState().setRunStatus('idle');
    mocks.apiInvoke.mockImplementation(async (command: string) => {
      if (command === 'get_active_chat_turn') return null;
      if (command === 'replay_chat_events') return emptyReplay();
      if (command === 'send_chat_message') return sendResult;
      if (command === 'list_conversations') return [];
      return { success: true };
    });

    const hook = renderHook(() => useTauriChat());
    await waitFor(() => {
      expect(mocks.apiInvoke).toHaveBeenCalledWith(
        'replay_chat_events',
        expect.objectContaining({ conversationId: 'conversation-admission' })
      );
    });
    let sending: Promise<boolean> = Promise.resolve(false);
    await act(async () => {
      sending = hook.result.current.sendMessage('new request');
      await Promise.resolve();
    });
    await waitFor(() => {
      expect(mocks.apiInvoke).toHaveBeenCalledWith('send_chat_message', {
        request: expect.objectContaining({
          workspaceId: 'global',
          conversationId: 'conversation-admission',
        }),
      });
    });
    expect(useChatStore.getState().messages).toEqual([]);
    expect(useChatStore.getState().isStreaming).toBe(false);

    await act(async () => {
      resolveSend({
        kind: 'task_run_conflict',
        run_id: 'run-existing',
        input_id: 'input-accepted',
      });
      await sending;
    });
    expect(useChatStore.getState().messages).toEqual([]);
    expect(useChatStore.getState().isStreaming).toBe(false);
    expect(hook.result.current.queuedInputs).toEqual([]);
    hook.unmount();
  });

  function conflictRun(
    id: string,
    status: TaskRun['status'] = 'paused',
    workspaceId = 'global'
  ): TaskRun {
    return {
      run_id: id,
      workspace_id: workspaceId,
      conversation_id: 'conversation-conflict',
      root_message_id: 'conflict-message',
      domain_profile: 'general',
      domain_binding: null,
      status,
      goal: id,
      goal_revision: 1,
      goal_sha256: 'goal',
      plan_id: null,
      route: 'diagnostic-only',
      attended_mode: 'attended',
      created_at: '2026-10-25T00:00:00Z',
      updated_at: '2026-10-25T00:00:00Z',
    };
  }

  function conflictBackend(
    resume: () => Promise<TaskRunResumeReceipt>,
    workspace: Workspace | null = null
  ) {
    const workspaceId = workspace?.id ?? 'global';
    useWorkspaceStore.setState({ current: workspace });
    mocks.apiInvoke.mockImplementation(async (command: string, args?: Record<string, unknown>) => {
      if (command === 'get_active_chat_turn') return null;
      if (command === 'replay_chat_events') return emptyReplay();
      if (command === 'list_queued_chat_inputs') return queuedFrontier;
      if (command === 'send_chat_message')
        return {
          kind: 'task_run_conflict',
          run_id: 'run-live',
          goal: 'current live goal',
          input_id: 'conflict-input',
        };
      if (command === 'resume_task_run') return resume();
      if (command === 'get_task_run')
        return conflictRun(
          String(args?.runId),
          args?.runId === 'run-old' ? 'failed' : 'paused',
          workspaceId
        );
      if (command === 'list_task_events')
        return { events: [], next_after_sequence: args?.sinceSeq ?? '0', has_more: false };
      if (command === 'get_task_plan' || command === 'get_task_continuation') return null;
      if (
        command === 'list_task_todos' ||
        command === 'list_task_artifacts' ||
        command === 'list_recovery_blockers' ||
        command === 'list_task_background_cells'
      )
        return [];
      if (command === 'get_task_completion_gate')
        return {
          run_id: args?.runId,
          goal_revision: 1,
          plan_revision: 0,
          ready: false,
          requirements: [],
          blockers: [],
        };
      return { success: true };
    });
    useConversationStore.setState({ activeId: 'conversation-conflict' });
    useTaskRuntimeStore.setState({
      activeRun: conflictRun('run-live', 'paused', workspaceId),
      taskScope: {
        workspaceId,
        workspaceGeneration: workspace?.product_data_generation ?? 'global',
      },
    });
  }

  it('continues the conflict run while the real task inspector selects a different historical run', async () => {
    conflictBackend(async () => ({ kind: 'resumed', run_id: 'run-live', turn_id: null }));
    const hook = renderHook(() => useTauriChat());
    await act(async () => {
      await hook.result.current.sendMessage('continue the live task');
    });
    const prompt = useTaskRuntimeStore.getState().interruptPrompt;
    expect(prompt?.runId).toBe('run-live');
    await act(async () => {
      await useTaskRuntimeStore
        .getState()
        .loadByRun({ workspaceId: 'global', workspaceGeneration: 'global' }, 'run-old');
    });
    await act(async () => {
      await prompt?.resolve('continue');
    });
    expect(mocks.apiInvoke).toHaveBeenCalledWith('resume_task_run', {
      workspaceGeneration: 'global',
      workspaceId: 'global',
      runId: 'run-live',
    });
    expect(mocks.apiInvoke).not.toHaveBeenCalledWith('resume_task_run', {
      workspaceGeneration: 'global',
      workspaceId: 'global',
      runId: 'run-old',
    });
    expect(useTaskRuntimeStore.getState().activeRun?.run_id).toBe('run-live');
    expect(useTaskRuntimeStore.getState().historySnapshot?.activeRun?.run_id).toBe('run-old');
    hook.unmount();
  });

  it('does not dismiss a successor prompt when the previous Continue receipt arrives late', async () => {
    const resumed = deferred<TaskRunResumeReceipt>();
    conflictBackend(() => resumed.promise);
    const hook = renderHook(() => useTauriChat());
    await act(async () => {
      await hook.result.current.sendMessage('continue the live task');
    });
    const original = useTaskRuntimeStore.getState().interruptPrompt;
    let continuing: Promise<void> | undefined;
    act(() => {
      continuing = original?.resolve('continue');
    });
    await waitFor(() =>
      expect(mocks.apiInvoke).toHaveBeenCalledWith('resume_task_run', {
        workspaceGeneration: 'global',
        workspaceId: 'global',
        runId: 'run-live',
      })
    );
    const successor = {
      runId: 'run-next',
      goal: 'new prompt',
      newMessage: 'new input',
      resolve: vi.fn(async () => {}),
    };
    act(() => useTaskRuntimeStore.getState().openInterruptPrompt(successor));
    await act(async () => {
      resumed.resolve({ kind: 'resumed', run_id: 'run-live', turn_id: null });
      await continuing;
    });
    expect(useTaskRuntimeStore.getState().interruptPrompt).toBe(successor);
    hook.unmount();
  });

  it.each(['conversation', 'workspace', 'generation'] as const)(
    'does not project a late Continue into a changed %s',
    async (change) => {
      const resumed = deferred<TaskRunResumeReceipt>();
      const workspace: Workspace = {
        id: 'workspace-1',
        name: 'original',
        root: '/original',
        kind: { type: 'coding' },
        metadata: { tags: [] },
        product_data_generation: 'generation-a',
        created_at: '2026-10-25T00:00:00Z',
        last_active: '2026-10-25T00:00:00Z',
      };
      conflictBackend(() => resumed.promise, workspace);
      const hook = renderHook(() => useTauriChat());
      await act(async () => {
        await hook.result.current.sendMessage('continue the live task');
      });
      const original = useTaskRuntimeStore.getState().interruptPrompt;
      let continuing: Promise<void> | undefined;
      act(() => {
        continuing = original?.resolve('continue');
      });
      await waitFor(() =>
        expect(mocks.apiInvoke).toHaveBeenCalledWith('resume_task_run', {
          workspaceGeneration: 'generation-a',
          workspaceId: 'workspace-1',
          runId: 'run-live',
        })
      );
      const successor = {
        runId: 'new-target',
        goal: 'new target',
        newMessage: 'new input',
        resolve: vi.fn(async () => {}),
      };
      act(() => {
        if (change === 'conversation')
          useConversationStore.setState({ activeId: 'new-conversation' });
        else
          useWorkspaceStore.setState({
            current: {
              id: change === 'workspace' ? 'new-workspace' : 'workspace-1',
              name: 'new',
              root: '/new',
              kind: { type: 'coding' },
              metadata: { tags: [] },
              product_data_generation: 'new-generation',
              created_at: '2026-10-25T00:00:00Z',
              last_active: '2026-10-25T00:00:00Z',
            },
          });
        useTaskRuntimeStore.getState().reset();
        useTaskRuntimeStore.setState({
          activeRun: {
            ...conflictRun(
              'new-target',
              'paused',
              change === 'workspace' ? 'new-workspace' : 'workspace-1'
            ),
            conversation_id: 'new-conversation',
          },
        });
        useTaskRuntimeStore.getState().openInterruptPrompt(successor);
      });
      await act(async () => {
        resumed.resolve({ kind: 'resumed', run_id: 'run-live', turn_id: null });
        await continuing;
      });
      expect(useTaskRuntimeStore.getState().interruptPrompt).toBe(successor);
      expect(useTaskRuntimeStore.getState().activeRun?.run_id).toBe('new-target');
      hook.unmount();
    }
  );

  it('cancels the exact conflicting run before starting the new input once', async () => {
    const conversationId = 'conversation-conflict';
    useConversationStore.setState({ activeId: conversationId });
    queuedFrontier = {
      queue_revision: 4,
      items: [inputProjection('conflicting-input', conversationId, 'replacement request')],
    };
    let sendCount = 0;
    mocks.apiInvoke.mockImplementation(async (command: string) => {
      if (command === 'get_active_chat_turn') return null;
      if (command === 'replay_chat_events') return emptyReplay();
      if (command === 'list_queued_chat_inputs') return queuedFrontier;
      if (command === 'send_chat_message') {
        sendCount += 1;
        return sendCount === 1
          ? {
              kind: 'task_run_conflict',
              run_id: 'run-existing',
              goal: 'old goal',
              new_message: 'replacement request',
              input_id: 'conflicting-input',
            }
          : {
              kind: 'started',
              success: true,
              root_turn_id: 'replacement-root',
              active_turn_id: 'replacement-root',
            };
      }
      if (command === 'cancel_task_run') return { success: true, run_id: 'run-existing' };
      return { success: true };
    });

    const hook = renderHook(() => useTauriChat());
    await act(async () => {
      await hook.result.current.sendMessage('replacement request');
    });
    const prompt = useTaskRuntimeStore.getState().interruptPrompt;
    expect(prompt?.runId).toBe('run-existing');

    await act(async () => {
      await prompt?.resolve('cancel_and_start');
    });

    expect(mocks.apiInvoke).toHaveBeenCalledWith('cancel_task_run', {
      workspaceGeneration: 'global',
      workspaceId: 'global',
      runId: 'run-existing',
    });
    expect(sendCount).toBe(2);
    expect(
      mocks.apiInvoke.mock.calls.filter(([command]) => command === 'send_chat_message').at(-1)
    ).toEqual([
      'send_chat_message',
      {
        request: expect.objectContaining({
          workspaceId: 'global',
          conversationId,
          inputIdentity: queuedFrontier.items[0]?.receipt.identity,
          expectedQueueRevision: 4,
        }),
      },
    ]);
    expect(
      useChatStore.getState().messages.filter((message) => message.role === 'user')
    ).toHaveLength(1);
    hook.unmount();
  });

  it('clears the previous conversation queue when starting another chat', async () => {
    useConversationStore.setState({ activeId: activeSnapshot.conversation_id });
    const hook = renderHook(() => useTauriChat());
    await waitFor(() => {
      expect(mocks.apiInvoke).toHaveBeenCalledWith('get_active_chat_turn', {
        workspaceId: 'global',
        conversationId: activeSnapshot.conversation_id,
      });
      expect(mocks.apiInvoke).toHaveBeenCalledWith(
        'replay_chat_events',
        expect.objectContaining({ conversationId: activeSnapshot.conversation_id })
      );
    });

    await act(async () => {
      await hook.result.current.sendMessage('queued for the running conversation');
    });
    expect(hook.result.current.queuedInputs).toHaveLength(1);
    expect(mocks.apiInvoke).toHaveBeenCalledWith('steer_chat_message', {
      workspaceId: 'global',
      conversationId: activeSnapshot.conversation_id,
      expectedActiveTurnId: activeSnapshot.active_turn_id,
      identity: expect.objectContaining({
        address: {
          workspace_id: 'global',
          conversation_id: activeSnapshot.conversation_id,
        },
      }),
      expectedQueueRevision: 1,
    });

    await act(async () => {
      await useConversationStore.getState().startNew();
    });
    expect(useConversationStore.getState().activeId).toBeNull();
    expect(hook.result.current.queuedInputs).toEqual([]);
    expect(mocks.apiInvoke).not.toHaveBeenCalledWith('reset_session');
    hook.unmount();
  });

  it('rehydrates the exact durable Frontier projection', async () => {
    const conversationId = 'conversation-durable-queue';
    useConversationStore.setState({ activeId: conversationId });
    mocks.apiInvoke.mockImplementation(async (command: string) => {
      if (command === 'get_active_chat_turn') {
        return { ...activeSnapshot, conversation_id: conversationId };
      }
      if (command === 'replay_chat_events') return emptyReplay();
      if (command === 'list_queued_chat_inputs') {
        return {
          queue_revision: 7,
          items: [inputProjection('durable-input', conversationId, 'continue after restart')],
        };
      }
      return { success: true };
    });

    const hook = renderHook(() => useTauriChat());
    await waitFor(() => {
      expect(hook.result.current.queuedInputs).toEqual([
        expect.objectContaining({
          id: 'durable-input',
          workspaceId: 'global',
          conversationId,
          identity: expect.objectContaining({ revision: 1 }),
        }),
      ]);
    });
    hook.unmount();
  });

  it.each(['another conversation', 'another new draft'] as const)(
    'keeps a deferred first input on its original saved conversation after %s is selected',
    async (selection) => {
      useConversationStore.setState({ activeId: null, workspaceId: 'global' });
      const create = deferred<{ kind: string; id: string }>();
      const admit = deferred<{ kind: string; root_turn_id: string; active_turn_id: string }>();
      let requestedId = '';
      let requestedRoot = '';
      const onDirectAccepted = vi.fn();
      const onPersisted = vi.fn();
      mocks.apiInvoke.mockImplementation(
        async (command: string, args?: Record<string, unknown>) => {
          if (command === 'get_active_chat_turn') return null;
          if (command === 'replay_chat_events') return emptyReplay();
          if (command === 'create_conversation') {
            requestedId = String((args?.request as { id: string }).id);
            return create.promise;
          }
          if (command === 'list_conversations' || command === 'list_tool_executions') return [];
          if (command === 'get_conversation')
            return { messages: [{ role: 'user', content: 'other conversation contents' }] };
          if (command === 'list_queued_chat_inputs') return { queue_revision: 0, items: [] };
          if (command === 'send_chat_message') {
            requestedRoot = String((args?.request as { messageKey: string }).messageKey);
            return admit.promise;
          }
          return { success: true };
        }
      );
      const hook = renderHook(() => useTauriChat());
      await waitFor(() => expect(mocks.listeners.has('chat://event')).toBe(true));
      const submission = {
        ...draftSubmission('original first input', onPersisted),
        onDirectAccepted,
      };
      let sending: Promise<boolean> | undefined;
      act(() => {
        sending = hook.result.current.sendMessage(
          'original first input',
          undefined,
          undefined,
          submission
        );
      });
      await waitFor(() => expect(requestedId).not.toBe(''));
      await act(async () => {
        if (selection === 'another conversation')
          await useConversationStore.getState().loadConversation('other-conversation');
        else await useConversationStore.getState().startNew();
      });
      const expectedActive = selection === 'another conversation' ? 'other-conversation' : null;
      const messages = useChatStore.getState().messages;
      await act(async () => {
        create.resolve({ kind: 'primary', id: requestedId });
      });
      await waitFor(() =>
        expect(
          mocks.apiInvoke.mock.calls.some(([command]) => command === 'send_chat_message')
        ).toBe(true)
      );
      expect(
        mocks.apiInvoke.mock.calls.find(([command]) => command === 'send_chat_message')?.[1]
      ).toMatchObject({
        request: {
          workspaceId: 'global',
          conversationId: requestedId,
          message: 'original first input',
        },
      });
      expect(useConversationStore.getState().activeId).toBe(expectedActive);
      expect(useChatStore.getState().messages).toEqual(messages);
      await act(async () => {
        admit.resolve({
          kind: 'started',
          root_turn_id: requestedRoot,
          active_turn_id: requestedRoot,
        });
        expect(await sending).toBe(true);
      });
      expect(useConversationStore.getState().activeId).toBe(expectedActive);
      expect(useChatStore.getState().messages).toEqual(messages);
      expect(onDirectAccepted).toHaveBeenCalledExactlyOnceWith(expect.any(Function));
      expect(onDirectAccepted.mock.calls[0]?.[0]()).toBe(false);
      expect(onPersisted).not.toHaveBeenCalled();
    }
  );

  it('publishes direct acceptance for its own ordinary first conversation without inventing persistence', async () => {
    useConversationStore.setState({ activeId: null, workspaceId: 'global' });
    let requestedId = '';
    let directWasCurrent = false;
    const onDirectAccepted = vi.fn((canProject: () => boolean) => {
      directWasCurrent = canProject();
    });
    const onPersisted = vi.fn();
    mocks.apiInvoke.mockImplementation(async (command: string, args?: Record<string, unknown>) => {
      if (command === 'get_active_chat_turn') return null;
      if (command === 'replay_chat_events') return emptyReplay();
      if (command === 'create_conversation') {
        requestedId = String((args?.request as { id: string }).id);
        return { kind: 'primary', id: requestedId };
      }
      if (command === 'list_conversations' || command === 'list_tool_executions') return [];
      if (command === 'list_queued_chat_inputs') return { queue_revision: 0, items: [] };
      if (command === 'send_chat_message') {
        const request = args?.request as { messageKey: string };
        return {
          kind: 'started',
          root_turn_id: request.messageKey,
          active_turn_id: request.messageKey,
        };
      }
      return { success: true };
    });
    const hook = renderHook(() => useTauriChat());
    await waitFor(() => expect(mocks.listeners.has('chat://event')).toBe(true));
    await act(async () => {
      expect(
        await hook.result.current.sendMessage('normal first input', undefined, undefined, {
          ...draftSubmission('normal first input', onPersisted),
          onDirectAccepted,
        })
      ).toBe(true);
    });
    expect(useConversationStore.getState().activeId).toBe(requestedId);
    expect(
      useChatStore
        .getState()
        .messages.filter((message) => message.role === 'user')
        .map((message) => message.content)
    ).toEqual(['normal first input']);
    expect(onDirectAccepted).toHaveBeenCalledTimes(1);
    expect(directWasCurrent).toBe(true);
    expect(onPersisted).not.toHaveBeenCalled();
  });

  it('keeps a late ordinary admission from resetting the view after Stop', async () => {
    const conversationId = 'ordinary-admission-after-stop';
    useConversationStore.setState({ activeId: conversationId, workspaceId: 'global' });
    const admission = deferred<{ kind: string; root_turn_id: string; active_turn_id: string }>();
    const onDirectAccepted = vi.fn();
    mocks.apiInvoke.mockImplementation(async (command: string) => {
      if (command === 'get_active_chat_turn') return null;
      if (command === 'replay_chat_events') return emptyReplay();
      if (command === 'list_queued_chat_inputs') return { queue_revision: 0, items: [] };
      if (command === 'send_chat_message') return admission.promise;
      return { success: true };
    });
    const hook = renderHook(() => useTauriChat());
    await waitFor(() =>
      expect(mocks.apiInvoke).toHaveBeenCalledWith('list_queued_chat_inputs', expect.anything())
    );
    let sending: Promise<boolean> | undefined;
    act(() => {
      sending = hook.result.current.sendMessage('ordinary input', undefined, undefined, {
        ...draftSubmission('ordinary input'),
        onDirectAccepted,
      });
    });
    await waitFor(() =>
      expect(mocks.apiInvoke.mock.calls.some(([command]) => command === 'send_chat_message')).toBe(
        true
      )
    );
    await act(async () => {
      await hook.result.current.cancel();
    });
    const messages = useChatStore.getState().messages;
    await act(async () => {
      admission.resolve({
        kind: 'started',
        root_turn_id: 'late-root',
        active_turn_id: 'late-root',
      });
      expect(await sending).toBe(true);
    });
    expect(onDirectAccepted).toHaveBeenCalledExactlyOnceWith(expect.any(Function));
    expect(onDirectAccepted.mock.calls[0]?.[0]()).toBe(false);
    expect(useChatStore.getState().messages).toEqual(messages);
  });

  it.each([
    ['create', 'Stop'],
    ['create', 'unmount'],
    ['list', 'Stop'],
    ['list', 'unmount'],
  ] as const)(
    'does not reacquire a first-input projection when %s finishes after %s',
    async (stage, interruption) => {
      useConversationStore.setState({ activeId: null, workspaceId: 'global' });
      const create = deferred<{ kind: string; id: string }>();
      const list = deferred<never[]>();
      let requestedId = '';
      let listRequested = false;
      const onDirectAccepted = vi.fn();
      const onPersisted = vi.fn();
      mocks.apiInvoke.mockImplementation(
        async (command: string, args?: Record<string, unknown>) => {
          if (command === 'get_active_chat_turn') return null;
          if (command === 'replay_chat_events') return emptyReplay();
          if (command === 'create_conversation') {
            requestedId = String((args?.request as { id: string }).id);
            return stage === 'create' ? create.promise : { kind: 'primary', id: requestedId };
          }
          if (command === 'list_conversations') {
            listRequested = true;
            return stage === 'list' ? list.promise : [];
          }
          if (command === 'list_queued_chat_inputs') return { queue_revision: 0, items: [] };
          if (command === 'send_chat_message') {
            const request = args?.request as { messageKey: string };
            return {
              kind: 'started',
              root_turn_id: request.messageKey,
              active_turn_id: request.messageKey,
            };
          }
          return { success: true };
        }
      );
      const hook = renderHook(() => useTauriChat());
      await waitFor(() => expect(mocks.listeners.has('chat://event')).toBe(true));
      let sending: Promise<boolean> | undefined;
      act(() => {
        sending = hook.result.current.sendMessage('bootstrap input', undefined, undefined, {
          ...draftSubmission('bootstrap input', onPersisted),
          onDirectAccepted,
        });
      });
      await waitFor(() =>
        expect(stage === 'create' ? requestedId !== '' : listRequested).toBe(true)
      );
      if (interruption === 'Stop')
        await act(async () => {
          await hook.result.current.cancel();
        });
      else hook.unmount();
      const messages = useChatStore.getState().messages;
      const toasts = useToastStore.getState().toasts;

      await act(async () => {
        if (stage === 'create') create.resolve({ kind: 'primary', id: requestedId });
        else list.resolve([]);
        expect(await sending).toBe(true);
      });

      expect(
        mocks.apiInvoke.mock.calls.find(([command]) => command === 'send_chat_message')?.[1]
      ).toMatchObject({
        request: { conversationId: requestedId, message: 'bootstrap input' },
      });
      expect(useChatStore.getState().messages).toBe(messages);
      expect(useToastStore.getState().toasts).toBe(toasts);
      expect(onDirectAccepted).toHaveBeenCalledExactlyOnceWith(expect.any(Function));
      expect(onDirectAccepted.mock.calls[0]?.[0]()).toBe(false);
      expect(onPersisted).not.toHaveBeenCalled();
    }
  );

  it('keeps its original first-input owner across a delayed metadata list normally', async () => {
    useConversationStore.setState({ activeId: null, workspaceId: 'global' });
    const list = deferred<never[]>();
    let requestedId = '';
    let listRequested = false;
    let directWasCurrent = false;
    const onDirectAccepted = vi.fn((canProject: () => boolean) => {
      directWasCurrent = canProject();
    });
    mocks.apiInvoke.mockImplementation(async (command: string, args?: Record<string, unknown>) => {
      if (command === 'get_active_chat_turn') return null;
      if (command === 'replay_chat_events') return emptyReplay();
      if (command === 'create_conversation') {
        requestedId = String((args?.request as { id: string }).id);
        return { kind: 'primary', id: requestedId };
      }
      if (command === 'list_conversations') {
        listRequested = true;
        return list.promise;
      }
      if (command === 'list_queued_chat_inputs') return { queue_revision: 0, items: [] };
      if (command === 'send_chat_message') {
        const request = args?.request as { messageKey: string };
        return {
          kind: 'started',
          root_turn_id: request.messageKey,
          active_turn_id: request.messageKey,
        };
      }
      return { success: true };
    });
    const hook = renderHook(() => useTauriChat());
    await waitFor(() => expect(mocks.listeners.has('chat://event')).toBe(true));
    let sending: Promise<boolean> | undefined;
    act(() => {
      sending = hook.result.current.sendMessage('original delayed list', undefined, undefined, {
        ...draftSubmission('original delayed list'),
        onDirectAccepted,
      });
    });
    await waitFor(() => expect(listRequested).toBe(true));
    expect(useConversationStore.getState().activeId).toBe(requestedId);
    await act(async () => {
      list.resolve([]);
      expect(await sending).toBe(true);
    });
    expect(directWasCurrent).toBe(true);
    expect(onDirectAccepted).toHaveBeenCalledTimes(1);
    expect(
      useChatStore
        .getState()
        .messages.filter((message) => message.role === 'user')
        .map((message) => message.content)
    ).toEqual(['original delayed list']);
  });

  it('acknowledges the real durable input before Frontier and Steer can finish', async () => {
    const conversationId = 'durable-before-observation';
    useConversationStore.setState({ activeId: conversationId });
    const persisted = inputProjection('opaque-backend-input', conversationId, '追加约束');
    const queue = deferred<ConversationInputReceipt>();
    const frontier = deferred<ConversationInputFrontier>();
    const steer = deferred<ConversationInputReceipt>();
    const onPersisted = vi.fn();
    let queueStarted = false;
    let frontierReleased = false;
    let current: ConversationInputFrontier = { queue_revision: 0, items: [] };
    mocks.apiInvoke.mockImplementation(async (command: string) => {
      if (command === 'get_active_chat_turn')
        return { ...activeSnapshot, conversation_id: conversationId };
      if (command === 'replay_chat_events') return emptyReplay();
      if (command === 'queue_chat_input') {
        queueStarted = true;
        return queue.promise;
      }
      if (command === 'list_queued_chat_inputs') {
        return queueStarted && !frontierReleased ? frontier.promise : current;
      }
      if (command === 'steer_chat_message') return steer.promise;
      return { success: true };
    });
    const hook = renderHook(() => useTauriChat());
    await waitFor(() =>
      expect(mocks.apiInvoke).toHaveBeenCalledWith('list_queued_chat_inputs', {
        workspaceId: 'global',
        conversationId,
      })
    );
    const submission = draftSubmission('追加约束', onPersisted);
    let sending: Promise<boolean> | undefined;
    let resolved = false;
    act(() => {
      sending = hook.result.current
        .sendMessage('追加约束', undefined, undefined, submission)
        .then((accepted) => {
          resolved = true;
          return accepted;
        });
    });
    expect(onPersisted).not.toHaveBeenCalled();
    await act(async () => {
      queue.resolve(persisted.receipt);
      await Promise.resolve();
    });
    expect(resolved).toBe(true);
    expect(onPersisted).toHaveBeenCalledExactlyOnceWith(persisted.receipt, expect.any(Function));
    expect(onPersisted.mock.calls[0]?.[1]()).toBe(true);
    expect(mocks.apiInvoke).not.toHaveBeenCalledWith('steer_chat_message', expect.anything());
    expect(await sending).toBe(true);
    await act(async () => {
      frontierReleased = true;
      current = { queue_revision: 3, items: [persisted] };
      frontier.resolve(current);
    });
    await waitFor(() =>
      expect(mocks.apiInvoke).toHaveBeenCalledWith(
        'steer_chat_message',
        expect.objectContaining({ identity: persisted.receipt.identity })
      )
    );
    expect(useChatStore.getState().messages.filter((message) => message.role === 'user')).toEqual(
      []
    );
    await act(async () => {
      current = { queue_revision: 5, items: [] };
      steer.resolve({ ...persisted.receipt, phase: 'drained', drained: true, queue_revision: 5 });
    });
    await waitFor(() =>
      expect(
        useChatStore
          .getState()
          .messages.filter((message) => message.role === 'user')
          .map((message) => message.content)
      ).toEqual(['追加约束'])
    );
    expect(onPersisted).toHaveBeenCalledTimes(1);
  });

  it('retries a lost durable acknowledgement with the same identity and frozen domain payload', async () => {
    const conversationId = 'durable-lost-ack';
    useConversationStore.setState({ activeId: conversationId });
    const receipt = {
      ...inputProjection('original-opaque-input', conversationId, '原始内容').receipt,
      duplicate: true,
    };
    const onPersisted = vi.fn();
    const attachment: Attachment = {
      name: '中文.txt',
      mime_type: 'text/plain',
      data: 'YWJj',
      size: 3,
      source: 'upload',
    };
    let calls = 0;
    mocks.apiInvoke.mockImplementation(async (command: string) => {
      if (command === 'get_active_chat_turn')
        return { ...activeSnapshot, conversation_id: conversationId };
      if (command === 'replay_chat_events') return emptyReplay();
      if (command === 'list_queued_chat_inputs') return { queue_revision: 0, items: [] };
      if (command === 'queue_chat_input') {
        calls += 1;
        if (calls === 1) throw new Error('response lost after admission');
        return receipt;
      }
      return { success: true };
    });
    const hook = renderHook(() => useTauriChat());
    await waitFor(() =>
      expect(mocks.apiInvoke).toHaveBeenCalledWith('list_queued_chat_inputs', {
        workspaceId: 'global',
        conversationId,
      })
    );
    const submission = draftSubmission('原始内容', onPersisted, [attachment], 'evidence-method');
    await act(async () => {
      expect(
        await hook.result.current.sendMessage(
          '原始内容',
          [attachment],
          'evidence-method',
          submission
        )
      ).toBe(false);
    });
    expect(onPersisted).not.toHaveBeenCalled();
    await act(async () => {
      expect(
        await hook.result.current.sendMessage(
          '原始内容',
          [attachment],
          'evidence-method',
          submission
        )
      ).toBe(true);
    });
    const attempts = mocks.apiInvoke.mock.calls.filter(
      ([command]) => command === 'queue_chat_input'
    );
    expect(attempts).toHaveLength(2);
    expect(attempts[0]?.[1]).toEqual(attempts[1]?.[1]);
    expect(attempts[1]?.[1]).toMatchObject({
      externalId: submission.externalId,
      text: '原始内容',
      domainPlugin: 'evidence-method',
      attachments: [attachment],
    });
    expect(onPersisted).toHaveBeenCalledExactlyOnceWith(receipt, expect.any(Function));
    expect(mocks.apiInvoke).not.toHaveBeenCalledWith('steer_chat_message', expect.anything());
  });

  it.each(['text', 'attachment', 'domain'] as const)(
    'rejects reuse of a draft identity with changed %s before transport',
    async (changed) => {
      useConversationStore.setState({ activeId: activeSnapshot.conversation_id });
      const hook = renderHook(() => useTauriChat());
      await waitFor(() =>
        expect(mocks.apiInvoke).toHaveBeenCalledWith('list_queued_chat_inputs', expect.anything())
      );
      const original: Attachment = {
        name: 'original.txt',
        mime_type: 'text/plain',
        data: 'YWJj',
        size: 3,
      };
      const onPersisted = vi.fn();
      const submission = draftSubmission('original', onPersisted, [original], 'original-domain');
      await act(async () => {
        expect(
          await hook.result.current.sendMessage(
            changed === 'text' ? 'replacement' : 'original',
            [changed === 'attachment' ? { ...original, data: 'eHl6' } : original],
            changed === 'domain' ? 'replacement-domain' : 'original-domain',
            submission
          )
        ).toBe(false);
      });
      expect(mocks.apiInvoke).not.toHaveBeenCalledWith('queue_chat_input', expect.anything());
      expect(onPersisted).not.toHaveBeenCalled();
    }
  );

  it.each([
    'conversation',
    'workspace',
    'generation',
    'new conversation epoch',
    'workspace transition',
  ] as const)('does not retarget a prepared draft after a %s change', async (changed) => {
    useConversationStore.setState({ activeId: activeSnapshot.conversation_id });
    const hook = renderHook(() => useTauriChat());
    await waitFor(() =>
      expect(mocks.apiInvoke).toHaveBeenCalledWith('list_queued_chat_inputs', expect.anything())
    );
    const submission = draftSubmission('original', vi.fn());
    await act(async () => {
      if (changed === 'conversation')
        useConversationStore.setState({ activeId: 'another-conversation' });
      if (changed === 'new conversation epoch')
        useConversationStore.setState({ newConversationEpoch: 1 });
      if (changed === 'workspace transition') useWorkspaceStore.setState({ isLoading: true });
      if (changed === 'workspace' || changed === 'generation')
        useWorkspaceStore.setState({
          current: {
            id: changed === 'workspace' ? 'another-workspace' : 'global',
            name: 'replacement',
            root: '/replacement',
            kind: { type: 'coding' },
            metadata: { tags: [] },
            product_data_generation: 'reopened',
            created_at: '',
            last_active: '',
          },
        });
      expect(
        await hook.result.current.sendMessage('original', undefined, undefined, submission)
      ).toBe(false);
    });
    expect(mocks.apiInvoke).not.toHaveBeenCalledWith('queue_chat_input', expect.anything());
    expect(mocks.apiInvoke).not.toHaveBeenCalledWith('send_chat_message', expect.anything());
  });

  it.each([
    'throwing callback',
    'rejected callback',
    'Frontier failure',
    'Steer rejection',
  ] as const)('keeps durable acceptance when the %s cannot finish', async (failure) => {
    const conversationId = 'durable-observation-failure';
    useConversationStore.setState({ activeId: conversationId });
    const receipt = inputProjection('persisted-input', conversationId, 'saved').receipt;
    let submitted = false;
    const onPersisted = vi.fn(() => {
      if (failure === 'throwing callback') throw new Error('view update failed');
      if (failure === 'rejected callback')
        return Promise.reject(new Error('async view update failed'));
    });
    mocks.apiInvoke.mockImplementation(async (command: string) => {
      if (command === 'get_active_chat_turn')
        return { ...activeSnapshot, conversation_id: conversationId };
      if (command === 'replay_chat_events') return emptyReplay();
      if (command === 'queue_chat_input') {
        submitted = true;
        return receipt;
      }
      if (command === 'list_queued_chat_inputs') {
        if (submitted && failure === 'Frontier failure') throw new Error('observation unavailable');
        if (submitted && failure === 'Steer rejection')
          return {
            queue_revision: 1,
            items: [inputProjection('persisted-input', conversationId, 'saved')],
          };
        return { queue_revision: 0, items: [] };
      }
      if (command === 'steer_chat_message')
        throw new Error('original turn no longer accepts Steer');
      return { success: true };
    });
    const hook = renderHook(() => useTauriChat());
    await waitFor(() =>
      expect(mocks.apiInvoke).toHaveBeenCalledWith('list_queued_chat_inputs', expect.anything())
    );
    await act(async () => {
      expect(
        await hook.result.current.sendMessage(
          'saved',
          undefined,
          undefined,
          draftSubmission('saved', onPersisted)
        )
      ).toBe(true);
    });
    await waitFor(() =>
      expect(useToastStore.getState().toasts).toEqual([
        expect.objectContaining({
          type: failure === 'Steer rejection' ? 'error' : 'warning',
          message: expect.stringContaining(
            failure === 'Steer rejection' ? '补充当前任务失败' : '输入已保存'
          ),
        }),
      ])
    );
    if (failure === 'Steer rejection') {
      await waitFor(() =>
        expect(hook.result.current.queuedInputs.at(0)?.steerOutcome).toBe('rejected')
      );
    }
    expect(onPersisted).toHaveBeenCalledExactlyOnceWith(receipt, expect.any(Function));
    expect(
      mocks.apiInvoke.mock.calls.filter(([command]) => command === 'queue_chat_input')
    ).toHaveLength(1);
  });

  it('reports the original late durable fact while fencing a different conversation', async () => {
    const conversationId = 'late-persisted-original';
    useConversationStore.setState({ activeId: conversationId });
    const queue = deferred<ConversationInputReceipt>();
    const receipt = inputProjection(
      'late-original-input',
      conversationId,
      'saved original'
    ).receipt;
    const onPersisted = vi.fn();
    mocks.apiInvoke.mockImplementation(async (command: string) => {
      if (command === 'get_active_chat_turn')
        return { ...activeSnapshot, conversation_id: useConversationStore.getState().activeId };
      if (command === 'replay_chat_events') return emptyReplay();
      if (command === 'list_queued_chat_inputs') return { queue_revision: 0, items: [] };
      if (command === 'queue_chat_input') return queue.promise;
      return { success: true };
    });
    const hook = renderHook(() => useTauriChat());
    await waitFor(() =>
      expect(mocks.apiInvoke).toHaveBeenCalledWith('list_queued_chat_inputs', expect.anything())
    );
    const submission = draftSubmission('saved original', onPersisted);
    let sending: Promise<boolean> | undefined;
    act(() => {
      sending = hook.result.current.sendMessage('saved original', undefined, undefined, submission);
    });
    await act(async () => {
      useConversationStore.setState({ activeId: 'new-visible-conversation' });
      useChatStore.getState().clearMessages();
      useChatStore.getState().addUserMessage('new draft belongs here');
    });
    const visible = useChatStore.getState().messages;
    await act(async () => {
      queue.resolve(receipt);
      expect(await sending).toBe(true);
    });
    expect(onPersisted).toHaveBeenCalledExactlyOnceWith(receipt, expect.any(Function));
    expect(onPersisted.mock.calls[0]?.[1]()).toBe(false);
    expect(useChatStore.getState().messages).toEqual(visible);
    expect(useToastStore.getState().toasts).toEqual([]);
    expect(mocks.apiInvoke).not.toHaveBeenCalledWith('steer_chat_message', expect.anything());
  });

  it('rechecks the live turn fence after early acknowledgement and a delayed Frontier', async () => {
    const conversationId = 'persisted-before-turn-settlement';
    useConversationStore.setState({ activeId: conversationId });
    const projection = inputProjection('settled-before-observation', conversationId, 'saved');
    const frontier = deferred<ConversationInputFrontier>();
    const onPersisted = vi.fn();
    let submitted = false;
    mocks.apiInvoke.mockImplementation(async (command: string) => {
      if (command === 'get_active_chat_turn')
        return { ...activeSnapshot, conversation_id: conversationId };
      if (command === 'replay_chat_events') return emptyReplay();
      if (command === 'queue_chat_input') {
        submitted = true;
        return projection.receipt;
      }
      if (command === 'list_queued_chat_inputs')
        return submitted ? frontier.promise : { queue_revision: 0, items: [] };
      return { success: true };
    });
    const hook = renderHook(() => useTauriChat());
    await waitFor(() => expect(mocks.listeners.has('chat://event')).toBe(true));
    await waitFor(() =>
      expect(mocks.apiInvoke).toHaveBeenCalledWith('list_queued_chat_inputs', expect.anything())
    );
    await act(async () => {
      expect(
        await hook.result.current.sendMessage(
          'saved',
          undefined,
          undefined,
          draftSubmission('saved', onPersisted)
        )
      ).toBe(true);
    });
    const canProject = onPersisted.mock.calls[0]?.[1] as (() => boolean) | undefined;
    expect(canProject?.()).toBe(true);
    act(() => {
      mocks.listeners.get('chat://event')?.({
        payload: turnStatusEnvelope(
          activeSnapshot.active_turn_id,
          activeSnapshot.root_turn_id,
          1,
          'completed',
          conversationId
        ),
      });
    });
    expect(canProject?.()).toBe(false);
    await act(async () => {
      frontier.resolve({ queue_revision: 4, items: [] });
    });
    expect(mocks.apiInvoke).not.toHaveBeenCalledWith('steer_chat_message', expect.anything());
  });

  it('does not fabricate a persisted notification from a malformed admission response', async () => {
    useConversationStore.setState({ activeId: activeSnapshot.conversation_id });
    const previous = mocks.apiInvoke.getMockImplementation();
    mocks.apiInvoke.mockImplementation(async (command, args) =>
      command === 'queue_chat_input' ? { success: true } : previous?.(command, args)
    );
    const hook = renderHook(() => useTauriChat());
    await waitFor(() =>
      expect(mocks.apiInvoke).toHaveBeenCalledWith('list_queued_chat_inputs', expect.anything())
    );
    const onPersisted = vi.fn();
    await act(async () => {
      expect(
        await hook.result.current.sendMessage(
          'saved',
          undefined,
          undefined,
          draftSubmission('saved', onPersisted)
        )
      ).toBe(false);
    });
    expect(onPersisted).not.toHaveBeenCalled();
    expect(mocks.apiInvoke).not.toHaveBeenCalledWith('steer_chat_message', expect.anything());
  });

  it('forwards an explicit domain on the unchanged ordinary send without fabricating persistence', async () => {
    const conversationId = 'ordinary-explicit-domain';
    useConversationStore.setState({ activeId: conversationId, workspaceId: 'global' });
    const onPersisted = vi.fn();
    mocks.apiInvoke.mockImplementation(async (command: string) => {
      if (command === 'get_active_chat_turn') return null;
      if (command === 'replay_chat_events') return emptyReplay();
      if (command === 'list_queued_chat_inputs') return { queue_revision: 0, items: [] };
      if (command === 'send_chat_message')
        return { kind: 'started', root_turn_id: 'ordinary-root', active_turn_id: 'ordinary-root' };
      return { success: true };
    });
    const hook = renderHook(() => useTauriChat());
    await waitFor(() =>
      expect(mocks.apiInvoke).toHaveBeenCalledWith('list_queued_chat_inputs', expect.anything())
    );
    await act(async () => {
      expect(
        await hook.result.current.sendMessage(
          'inspect evidence',
          undefined,
          'evidence-method',
          draftSubmission('inspect evidence', onPersisted, undefined, 'evidence-method')
        )
      ).toBe(true);
    });
    expect(mocks.apiInvoke).toHaveBeenCalledWith('send_chat_message', {
      request: expect.objectContaining({ domainPlugin: 'evidence-method' }),
    });
    expect(onPersisted).not.toHaveBeenCalled();
  });

  it('replaces a queued input with its stable identity and queue revision', async () => {
    const conversationId = 'conversation-edit';
    useConversationStore.setState({ activeId: conversationId });
    const pending = inputProjection('edited-input', conversationId, 'original');
    queuedFrontier = { queue_revision: 11, items: [pending] };
    const previous = mocks.apiInvoke.getMockImplementation();
    mocks.apiInvoke.mockImplementation(async (command, args) => {
      if (command === 'get_active_chat_turn') return null;
      if (command === 'replace_queued_chat_input') {
        const edited = {
          ...pending,
          receipt: {
            ...pending.receipt,
            identity: { ...pending.receipt.identity, revision: 2, payload_sha256: 'edited-sha' },
          },
          payload: { ...pending.payload, text: 'edited 中文🙂', payload_sha256: 'edited-sha' },
        };
        queuedFrontier = { queue_revision: 12, items: [edited] };
        return edited.receipt;
      }
      return previous?.(command, args);
    });
    const hook = renderHook(() => useTauriChat());
    await waitFor(() => expect(hook.result.current.queuedInputs).toHaveLength(1));
    await act(async () => {
      expect(await hook.result.current.replaceQueuedMessage('edited-input', 'edited 中文🙂')).toBe(
        true
      );
    });
    expect(mocks.apiInvoke).toHaveBeenCalledWith('replace_queued_chat_input', {
      identity: pending.receipt.identity,
      expectedQueueRevision: 11,
      text: 'edited 中文🙂',
      attachments: [],
      domainPlugin: null,
    });
    expect(hook.result.current.queuedInputs.at(0)?.identity.revision).toBe(2);
    expect(hook.result.current.queuedInputs.at(0)?.text).toBe('edited 中文🙂');
    expect(mocks.apiInvoke).not.toHaveBeenCalledWith('remove_queued_chat_input', expect.anything());
    expect(mocks.apiInvoke).not.toHaveBeenCalledWith('queue_chat_input', expect.anything());
    hook.unmount();
  });

  it('retains the original queued projection when replacement CAS is rejected', async () => {
    const conversationId = 'conversation-stale-edit';
    useConversationStore.setState({ activeId: conversationId });
    const pending = inputProjection('stale-input', conversationId, 'original');
    queuedFrontier = { queue_revision: 11, items: [pending] };
    const previous = mocks.apiInvoke.getMockImplementation();
    mocks.apiInvoke.mockImplementation(async (command, args) => {
      if (command === 'get_active_chat_turn') return null;
      if (command === 'replace_queued_chat_input') throw new Error('queue revision changed');
      return previous?.(command, args);
    });
    const hook = renderHook(() => useTauriChat());
    await waitFor(() => expect(hook.result.current.queuedInputs).toHaveLength(1));
    await act(async () => {
      expect(await hook.result.current.replaceQueuedMessage('stale-input', 'late edit')).toBe(
        false
      );
    });
    expect(hook.result.current.queuedInputs.at(0)?.text).toBe('original');
    expect(
      useToastStore
        .getState()
        .toasts.some((toast) => toast.message.includes('queue revision changed'))
    ).toBe(true);
    hook.unmount();
  });

  it('does not project a queued edit receipt into a successor conversation', async () => {
    const conversationId = 'conversation-late-edit';
    useConversationStore.setState({ activeId: conversationId });
    const pending = inputProjection('late-edit', conversationId, 'original');
    queuedFrontier = { queue_revision: 11, items: [pending] };
    const replacement = deferred<ConversationInputReceipt>();
    const previous = mocks.apiInvoke.getMockImplementation();
    mocks.apiInvoke.mockImplementation(async (command, args) => {
      if (command === 'get_active_chat_turn') return null;
      if (command === 'replace_queued_chat_input') return replacement.promise;
      return previous?.(command, args);
    });
    const hook = renderHook(() => useTauriChat());
    await waitFor(() => expect(hook.result.current.queuedInputs).toHaveLength(1));
    let result: Promise<boolean> | undefined;
    act(() => {
      result = hook.result.current.replaceQueuedMessage('late-edit', 'edited');
    });
    await act(async () => {
      queuedFrontier = { queue_revision: 0, items: [] };
      useConversationStore.setState({ activeId: 'successor-conversation' });
    });
    await act(async () => {
      replacement.resolve({
        ...pending.receipt,
        identity: { ...pending.receipt.identity, revision: 2 },
      });
      expect(await result).toBe(false);
    });
    expect(hook.result.current.queuedInputs).toEqual([]);
    hook.unmount();
  });

  it('keeps channel-owned queue inputs with their original runtime', async () => {
    const conversationId = 'channel-original';
    useConversationStore.setState({ activeId: conversationId });
    const pending = inputProjection('channel-owned', conversationId, 'provider message');
    Object.assign(pending.payload, { channel_binding: { channel_id: 'weixin' } });
    queuedFrontier = { queue_revision: 11, items: [pending] };
    const previous = mocks.apiInvoke.getMockImplementation();
    mocks.apiInvoke.mockImplementation(async (command, args) =>
      command === 'get_active_chat_turn' ? null : previous?.(command, args)
    );
    const hook = renderHook(() => useTauriChat());
    await waitFor(() => expect(hook.result.current.queuedInputs.at(0)?.channelOwned).toBe(true));
    await act(async () => {
      expect(await hook.result.current.replaceQueuedMessage('channel-owned', 'edited')).toBe(false);
      expect(await hook.result.current.steerQueuedMessage('channel-owned')).toBe(false);
      hook.result.current.removeQueuedMessage('channel-owned');
      hook.result.current.clearQueuedMessages();
    });
    for (const command of [
      'replace_queued_chat_input',
      'steer_chat_message',
      'remove_queued_chat_input',
    ]) {
      expect(mocks.apiInvoke).not.toHaveBeenCalledWith(command, expect.anything());
    }
    hook.unmount();
  });

  it('steers an exact selected identity and refreshes instead of removing locally', async () => {
    const conversationId = 'conversation-steer-accepted';
    useConversationStore.setState({ activeId: conversationId });
    const pending = inputProjection('selected-input', conversationId, 'inject this once');
    queuedFrontier = { queue_revision: 11, items: [pending] };
    mocks.apiInvoke.mockImplementation(async (command: string) => {
      if (command === 'get_active_chat_turn') {
        return { ...activeSnapshot, conversation_id: conversationId };
      }
      if (command === 'replay_chat_events') return emptyReplay();
      if (command === 'list_queued_chat_inputs') return queuedFrontier;
      if (command === 'steer_chat_message') {
        queuedFrontier = { queue_revision: 13, items: [] };
        return { ...pending.receipt, phase: 'drained', drained: true, queue_revision: 13 };
      }
      return { success: true };
    });

    const hook = renderHook(() => useTauriChat());
    await waitFor(() => {
      expect(mocks.apiInvoke).toHaveBeenCalledWith('list_queued_chat_inputs', {
        workspaceId: 'global',
        conversationId,
      });
    });
    const queuedId = hook.result.current.queuedInputs.at(0)?.id;
    expect(queuedId).toBeTruthy();

    let accepted = false;
    await act(async () => {
      accepted = await hook.result.current.steerQueuedMessage(String(queuedId));
    });

    expect(accepted).toBe(true);
    await waitFor(() => expect(hook.result.current.queuedInputs).toEqual([]));
    expect(mocks.apiInvoke).toHaveBeenCalledWith('steer_chat_message', {
      workspaceId: 'global',
      conversationId,
      expectedActiveTurnId: activeSnapshot.active_turn_id,
      identity: pending.receipt.identity,
      expectedQueueRevision: 11,
    });
    expect(
      mocks.apiInvoke.mock.calls.filter(([command]) => command === 'remove_queued_chat_input')
    ).toHaveLength(0);
    expect(useChatStore.getState().runStatus).toBe('running');
    hook.unmount();
  });

  it.each(
    (
      [
        'conversation',
        'workspace',
        'workspace generation',
        'conversation reopen',
        'hook remount',
      ] as const
    ).flatMap((change) =>
      (['Steer IPC', 'Frontier IPC'] as const).map((boundary) => [change, boundary] as const)
    )
  )('drops late Steer success after %s while awaiting %s', async (change, boundary) => {
    const workspace: Workspace = {
      id: 'steer-workspace-a',
      name: 'Steer workspace',
      root: '/steer-workspace',
      kind: { type: 'coding' },
      metadata: { tags: [] },
      product_data_generation: 'generation-a',
      created_at: '2026-10-25T00:00:00Z',
      last_active: '2026-10-25T00:00:00Z',
    };
    let visibleSnapshot = {
      ...activeSnapshot,
      workspace_id: workspace.id,
      conversation_id: 'steer-conversation-a',
      root_turn_id: 'steer-root-a',
      active_turn_id: 'steer-turn-a',
    };
    const pending = inputProjection(
      'late-steer-a',
      visibleSnapshot.conversation_id,
      'original input'
    );
    pending.receipt.identity.address.workspace_id = workspace.id;
    const current = inputProjection(
      'current-steer-b',
      visibleSnapshot.conversation_id,
      'current input'
    );
    current.receipt.identity.address.workspace_id = workspace.id;
    let visibleFrontier: ConversationInputFrontier = { queue_revision: 11, items: [pending] };
    let resolveSteer: ((receipt: ConversationInputReceipt) => void) | undefined;
    let resolveFrontier: ((frontier: ConversationInputFrontier) => void) | undefined;
    let waitForFrontier = false;
    useWorkspaceStore.setState({ current: workspace });
    useConversationStore.setState({ activeId: visibleSnapshot.conversation_id });
    mocks.apiInvoke.mockImplementation(async (command: string, args?: Record<string, unknown>) => {
      if (command === 'get_active_chat_turn') return visibleSnapshot;
      if (command === 'replay_chat_events') return emptyReplay();
      if (command === 'list_queued_chat_inputs') {
        if (waitForFrontier) {
          waitForFrontier = false;
          return new Promise<ConversationInputFrontier>((resolve) => {
            resolveFrontier = resolve;
          });
        }
        return visibleFrontier;
      }
      if (command === 'steer_chat_message') {
        if (
          (args?.identity as ConversationInputReceipt['identity']).input_id ===
          pending.receipt.identity.input_id
        ) {
          if (boundary === 'Steer IPC') {
            return new Promise<ConversationInputReceipt>((resolve) => {
              resolveSteer = resolve;
            });
          }
          waitForFrontier = true;
          return { ...pending.receipt, phase: 'drained', drained: true, queue_revision: 12 };
        }
        visibleFrontier = { queue_revision: 22, items: [] };
        return { ...current.receipt, phase: 'drained', drained: true, queue_revision: 22 };
      }
      return { success: true };
    });
    let hook = renderHook(() => useTauriChat());
    await waitFor(() => expect(hook.result.current.queuedInputs.at(0)?.id).toBe('late-steer-a'));
    let result: Promise<boolean> | undefined;
    act(() => {
      result = hook.result.current.steerQueuedMessage('late-steer-a');
    });
    await waitFor(() =>
      expect(boundary === 'Steer IPC' ? resolveSteer : resolveFrontier).toBeDefined()
    );

    visibleSnapshot = {
      ...visibleSnapshot,
      root_turn_id: 'steer-root-b',
      active_turn_id: 'steer-turn-b',
    };
    if (change === 'conversation') {
      visibleSnapshot = { ...visibleSnapshot, conversation_id: 'steer-conversation-b' };
      current.receipt.identity.address.conversation_id = visibleSnapshot.conversation_id;
    }
    if (change === 'workspace') {
      visibleSnapshot = { ...visibleSnapshot, workspace_id: 'steer-workspace-b' };
      current.receipt.identity.address.workspace_id = visibleSnapshot.workspace_id;
    }
    visibleFrontier = { queue_revision: 21, items: [current] };
    await act(async () => {
      useChatStore.getState().clearMessages();
      useChatStore.getState().startAssistantMessage('steer-root-b');
      useChatStore.getState().appendToken('steer-root-b', 'current reply');
      if (change === 'workspace' || change === 'workspace generation') {
        useWorkspaceStore.setState({
          current: {
            ...workspace,
            id: visibleSnapshot.workspace_id,
            product_data_generation: 'generation-b',
          },
        });
      }
      if (change === 'conversation')
        useConversationStore.setState({ activeId: visibleSnapshot.conversation_id });
      if (change === 'conversation reopen')
        useConversationStore.setState({ newConversationEpoch: 1 });
      if (change === 'hook remount') {
        hook.unmount();
        hook = renderHook(() => useTauriChat());
      }
    });
    const visibleMessages = useChatStore.getState().messages;
    await act(async () => {
      resolveSteer?.({ ...pending.receipt, phase: 'drained', drained: true, queue_revision: 12 });
      resolveFrontier?.({ queue_revision: 12, items: [] });
      expect(await result).toBe(true);
    });

    expect(useChatStore.getState().messages).toEqual(visibleMessages);
    expect(useToastStore.getState().toasts).toEqual([]);
    expect(
      mocks.apiInvoke.mock.calls.filter(([command]) => command === 'steer_chat_message')
    ).toHaveLength(1);
    expect(
      mocks.apiInvoke.mock.calls.filter(([command]) =>
        ['queue_chat_input', 'send_chat_message', 'remove_queued_chat_input'].includes(command)
      )
    ).toHaveLength(0);
    await waitFor(() => expect(hook.result.current.queuedInputs.at(0)?.id).toBe('current-steer-b'));
    await act(async () => {
      expect(await hook.result.current.steerQueuedMessage('current-steer-b')).toBe(true);
    });
    expect(
      useChatStore.getState().messages.find((message) => message.id === 'steer-root-b')
    ).toMatchObject({ content: 'current reply', isStreaming: false });
    expect(
      useChatStore
        .getState()
        .messages.filter((message) => message.role === 'user')
        .map((message) => message.content)
    ).toEqual(['current input']);
    hook.unmount();
  });

  it('does not show a late Steer rejection in another conversation', async () => {
    const conversationId = 'late-rejected-steer-a';
    useConversationStore.setState({ activeId: conversationId });
    const pending = inputProjection('late-rejection', conversationId, 'original input');
    let rejectSteer: ((reason: Error) => void) | undefined;
    mocks.apiInvoke.mockImplementation(async (command: string) => {
      if (command === 'get_active_chat_turn')
        return { ...activeSnapshot, conversation_id: useConversationStore.getState().activeId };
      if (command === 'replay_chat_events') return emptyReplay();
      if (command === 'list_queued_chat_inputs') return { queue_revision: 1, items: [pending] };
      if (command === 'steer_chat_message')
        return new Promise<ConversationInputReceipt>((_resolve, reject) => {
          rejectSteer = reject;
        });
      return { success: true };
    });
    const hook = renderHook(() => useTauriChat());
    await waitFor(() => expect(hook.result.current.queuedInputs.at(0)?.id).toBe('late-rejection'));
    let result: Promise<boolean> | undefined;
    act(() => {
      result = hook.result.current.steerQueuedMessage('late-rejection');
    });
    await waitFor(() => expect(rejectSteer).toBeDefined());
    act(() => {
      useConversationStore.setState({ activeId: 'late-rejected-steer-b' });
    });
    await act(async () => {
      rejectSteer?.(new Error('old turn settled'));
      expect(await result).toBe(false);
    });
    expect(useToastStore.getState().toasts).toEqual([]);
    hook.unmount();
  });

  it('shows a retained outcome when the safe point keeps Steer queued', async () => {
    const conversationId = 'retained-steer';
    const pending = inputProjection('retained-input', conversationId, 'keep queued');
    useConversationStore.setState({ activeId: conversationId });
    mocks.apiInvoke.mockImplementation(async (command: string) => {
      if (command === 'get_active_chat_turn') return null;
      if (command === 'replay_chat_events') return emptyReplay();
      if (command === 'list_queued_chat_inputs') return { queue_revision: 1, items: [pending] };
      if (command === 'steer_chat_message') {
        return { ...pending.receipt, phase: 'persisted', drained: false, queue_revision: 2 };
      }
      return { success: true };
    });
    const hook = renderHook(() => useTauriChat());
    await waitFor(() => expect(hook.result.current.queuedInputs.at(0)?.id).toBe('retained-input'));
    await act(async () => {
      expect(await hook.result.current.steerQueuedMessage('retained-input')).toBe(false);
    });
    await waitFor(() =>
      expect(hook.result.current.queuedInputs.at(0)?.steerOutcome).toBe('retained')
    );
    hook.unmount();
  });

  it('does not dispatch the next conversation input from a TaskRun terminal event', async () => {
    const conversationId = 'conversation-taskrun-terminal';
    useConversationStore.setState({ activeId: conversationId });
    queuedFrontier = {
      queue_revision: 4,
      items: [inputProjection('wait-for-turn-terminal', conversationId, 'next input')],
    };
    mocks.apiInvoke.mockImplementation(async (command: string) => {
      if (command === 'get_active_chat_turn') {
        return { ...activeSnapshot, conversation_id: conversationId };
      }
      if (command === 'replay_chat_events') return emptyReplay();
      if (command === 'list_queued_chat_inputs') return queuedFrontier;
      if (command === 'send_chat_message') {
        throw new Error('TaskRun terminal must not dispatch a conversation input');
      }
      return { success: true };
    });

    const hook = renderHook(() => useTauriChat());
    await waitFor(() => expect(mocks.listeners.has('execution://event')).toBe(true));
    mocks.apiInvoke.mockClear();

    act(() => {
      mocks.listeners.get('execution://event')?.({
        payload: {
          kind: 'run',
          event: 'run_completed',
          workspace_id: 'global',
          conversation_id: conversationId,
          run_id: 'task-run-finished-first',
        },
      });
    });
    await act(async () => Promise.resolve());

    expect(mocks.apiInvoke.mock.calls.some(([command]) => command === 'send_chat_message')).toBe(
      false
    );
    expect(hook.result.current.queuedInputs.map((item) => item.id)).toEqual([
      'wait-for-turn-terminal',
    ]);
    hook.unmount();
  });

  it('loads the task UI when an eager conversation run commits a plan', async () => {
    const conversationId = 'conversation-plan-committed';
    useConversationStore.setState({ activeId: conversationId });
    mocks.apiInvoke.mockImplementation(async (command: string) => {
      if (command === 'get_active_chat_turn') return null;
      if (command === 'replay_chat_events') return emptyReplay();
      if (command === 'list_queued_chat_inputs') return queuedFrontier;
      return { success: true };
    });
    const loadByConversation = vi
      .spyOn(useTaskRuntimeStore.getState(), 'loadByConversation')
      .mockResolvedValue();

    const hook = renderHook(() => useTauriChat());
    await waitFor(() => expect(mocks.listeners.has('chat://event')).toBe(true));

    act(() => {
      mocks.listeners.get('chat://event')?.({
        payload: executionEnvelope('plan-root', 1, conversationId, {
          scope: 'run',
          event: 'plan_revision_committed',
          workspace_id: 'global',
          conversation_id: conversationId,
          run_id: 'taskrun:conversation-plan-committed',
          task_id: null,
          subagent_run_id: null,
          agent: null,
          payload: {},
          framework_event: null,
        }),
      });
    });

    await waitFor(() => {
      expect(loadByConversation).toHaveBeenCalledWith('global', conversationId);
    });
    loadByConversation.mockRestore();
    hook.unmount();
  });

  it('delivers lifecycle and turn events continuously through the live sequencer', async () => {
    const conversationId = 'conversation-sequenced-input';
    const snapshot = {
      ...activeSnapshot,
      conversation_id: conversationId,
      root_turn_id: 'sequenced-root',
      active_turn_id: 'sequenced-turn',
    };
    useConversationStore.setState({ activeId: conversationId });
    mocks.apiInvoke.mockImplementation(async (command: string) => {
      if (command === 'get_active_chat_turn') return snapshot;
      if (command === 'replay_chat_events') return emptyReplay();
      if (command === 'list_queued_chat_inputs') return { queue_revision: 5, items: [] };
      return { success: true };
    });
    const hook = renderHook(() => useTauriChat());
    await waitFor(() => expect(mocks.listeners.has('chat://event')).toBe(true));

    act(() => {
      const listener = mocks.listeners.get('chat://event');
      listener?.({
        payload: inputLifecycleEnvelope(1, 'persisted', conversationId, 'input-one'),
      });
      listener?.({
        payload: inputLifecycleEnvelope(2, 'attempt_started', conversationId, 'input-one'),
      });
      listener?.({
        payload: turnStatusEnvelope(
          snapshot.active_turn_id,
          snapshot.root_turn_id,
          3,
          'running',
          conversationId
        ),
      });
      listener?.({
        payload: agentEnvelope(
          snapshot.active_turn_id,
          4,
          { type: 'token', data: 'ordered token' },
          conversationId,
          snapshot.root_turn_id
        ),
      });
      listener?.({
        payload: turnStatusEnvelope(
          snapshot.active_turn_id,
          snapshot.root_turn_id,
          5,
          'completed',
          conversationId
        ),
      });
    });

    await waitFor(() => expect(useChatStore.getState().runStatus).toBe('completed'));
    expect(
      useChatStore.getState().messages.find((message) => message.id === snapshot.root_turn_id)
        ?.content
    ).toContain('ordered token');
    hook.unmount();
  });

  it('原生 schema5 保存检查点不阻塞 completed，下一条输入正常启动新回合', async () => {
    const conversationId = 'native-schema5-conversation';
    const snapshot = {
      ...activeSnapshot,
      conversation_id: conversationId,
      root_turn_id: 'native-schema5-root',
      active_turn_id: 'native-schema5-turn',
    };
    useConversationStore.setState({ activeId: conversationId });
    let completed = false;
    mocks.apiInvoke.mockImplementation(async (command: string, args?: Record<string, unknown>) => {
      if (command === 'get_active_chat_turn') return completed ? null : snapshot;
      if (command === 'replay_chat_events') return emptyReplay();
      if (command === 'list_queued_chat_inputs') return { queue_revision: 0, items: [] };
      if (command === 'send_chat_message') {
        const request = args?.request as { messageKey: string };
        return {
          kind: 'started',
          root_turn_id: request.messageKey,
          active_turn_id: request.messageKey,
        };
      }
      return { success: true };
    });
    const hook = renderHook(() => useTauriChat());
    await waitFor(() =>
      expect(mocks.apiInvoke).toHaveBeenCalledWith('list_queued_chat_inputs', {
        workspaceId: 'global',
        conversationId,
      })
    );
    act(() => {
      const listener = mocks.listeners.get('chat://event');
      const agent = (sequence: number, event: AgentEvent) =>
        agentEnvelope(
          snapshot.active_turn_id,
          sequence,
          event,
          conversationId,
          snapshot.root_turn_id
        );
      listener?.({
        payload: turnStatusEnvelope(
          snapshot.active_turn_id,
          snapshot.root_turn_id,
          1,
          'running',
          conversationId
        ),
      });
      listener?.({
        payload: agent(2, {
          type: 'transcript_projection_settlement',
          data: {
            status: 'settled',
            operation_id: 'transcript-projection-v1:original',
            conversation_id: conversationId,
            generation_id: conversationId,
            attempt: 1,
            error_class: null,
            detail: null,
          },
        }),
      });
      listener?.({ payload: agent(3, { type: 'token', data: '本机原回复' }) });
      listener?.({ payload: agent(4, { type: 'final_answer', data: '本机原回复' }) });
      completed = true;
      listener?.({
        payload: turnStatusEnvelope(
          snapshot.active_turn_id,
          snapshot.root_turn_id,
          5,
          'completed',
          conversationId
        ),
      });
    });
    await waitFor(() => expect(useChatStore.getState().isStreaming).toBe(false));
    expect(useChatStore.getState().runStatus).toBe('completed');
    expect(useChatStore.getState().messages.at(-1)?.content).toBe('本机原回复');

    mocks.apiInvoke.mockClear();
    await act(async () => {
      expect(await hook.result.current.sendMessage('NATIVE_HITL')).toBe(true);
    });
    expect(mocks.apiInvoke).toHaveBeenCalledWith(
      'send_chat_message',
      expect.objectContaining({
        request: expect.objectContaining({ conversationId, message: 'NATIVE_HITL' }),
      })
    );
    expect(mocks.apiInvoke).not.toHaveBeenCalledWith('queue_chat_input', expect.anything());
    hook.unmount();
  });

  it('replays immediately when a live chat envelope exposes an outer sequence gap', async () => {
    const conversationId = 'conversation-live-gap';
    const snapshot = {
      ...activeSnapshot,
      conversation_id: conversationId,
      root_turn_id: 'gap-root',
      active_turn_id: 'gap-turn',
    };
    useConversationStore.setState({ activeId: conversationId });
    let replayCalls = 0;
    const first = agentEnvelope(
      snapshot.active_turn_id,
      1,
      { type: 'token', data: 'first ' },
      conversationId,
      snapshot.root_turn_id
    );
    const second = agentEnvelope(
      snapshot.active_turn_id,
      2,
      { type: 'token', data: 'second' },
      conversationId,
      snapshot.root_turn_id
    );
    mocks.apiInvoke.mockImplementation(async (command: string) => {
      if (command === 'get_active_chat_turn') return snapshot;
      if (command === 'list_queued_chat_inputs') return { queue_revision: 0, items: [] };
      if (command === 'replay_chat_events') {
        replayCalls += 1;
        return replayCalls === 1
          ? emptyReplay()
          : {
              events: [first, second],
              retained_earliest_cursor: 1,
              returned_earliest_cursor: 1,
              latest_cursor: 2,
              truncated: false,
            };
      }
      return { success: true };
    });
    const hook = renderHook(() => useTauriChat());
    await waitFor(() => expect(mocks.listeners.has('chat://event')).toBe(true));

    act(() => {
      mocks.listeners.get('chat://event')?.({ payload: second });
    });

    await waitFor(() => {
      expect(
        useChatStore.getState().messages.find((message) => message.id === snapshot.root_turn_id)
          ?.content
      ).toContain('first second');
    });
    expect(mocks.apiInvoke).toHaveBeenCalledWith('replay_chat_events', {
      workspaceId: 'global',
      conversationId,
      messageKey: snapshot.root_turn_id,
      afterCursor: 0,
    });
    hook.unmount();
  });

  it('keeps a newer Frontier when list responses arrive in reverse order', async () => {
    const conversationId = 'conversation-frontier-race';
    useConversationStore.setState({ activeId: conversationId });
    const pending: Array<(frontier: ConversationInputFrontier) => void> = [];
    let deferLists = false;
    mocks.apiInvoke.mockImplementation(async (command: string) => {
      if (command === 'get_active_chat_turn') {
        return { ...activeSnapshot, conversation_id: conversationId };
      }
      if (command === 'replay_chat_events') return emptyReplay();
      if (command === 'list_queued_chat_inputs') {
        if (!deferLists) return { queue_revision: 9, items: [] };
        return new Promise<ConversationInputFrontier>((resolve) => pending.push(resolve));
      }
      return { success: true };
    });
    const hook = renderHook(() => useTauriChat());
    await waitFor(() => expect(mocks.listeners.has('chat://event')).toBe(true));
    deferLists = true;

    act(() => {
      const listener = mocks.listeners.get('chat://event');
      listener?.({
        payload: inputLifecycleEnvelope(1, 'persisted', conversationId, 'input-race'),
      });
      listener?.({
        payload: inputLifecycleEnvelope(2, 'attempt_started', conversationId, 'input-race'),
      });
    });
    await waitFor(() => expect(pending).toHaveLength(2));
    const newer = inputProjection('revision-11', conversationId, 'newer');
    const older = inputProjection('revision-10', conversationId, 'older');
    await act(async () => {
      pending[1]?.({ queue_revision: 11, items: [newer] });
      await Promise.resolve();
      pending[0]?.({ queue_revision: 10, items: [older] });
      await Promise.resolve();
    });

    expect(hook.result.current.queuedInputs.map((item) => item.id)).toEqual(['revision-11']);
    hook.unmount();
  });

  it('drops a late Frontier response after the conversation generation changes', async () => {
    const oldConversation = 'conversation-generation-old';
    const newConversation = 'conversation-generation-new';
    useConversationStore.setState({ activeId: oldConversation });
    let resolveOld: ((frontier: ConversationInputFrontier) => void) | null = null;
    let deferOld = false;
    const newProjection = inputProjection('generation-new', newConversation, 'new generation');
    mocks.apiInvoke.mockImplementation(async (command: string, args?: Record<string, unknown>) => {
      if (command === 'get_active_chat_turn') return null;
      if (command === 'replay_chat_events') return emptyReplay();
      if (command === 'list_queued_chat_inputs') {
        if (args?.conversationId === oldConversation && deferOld) {
          return new Promise<ConversationInputFrontier>((resolve) => {
            resolveOld = resolve;
          });
        }
        if (args?.conversationId === newConversation) {
          return { queue_revision: 2, items: [newProjection] };
        }
        return { queue_revision: 1, items: [] };
      }
      return { success: true };
    });
    const hook = renderHook(() => useTauriChat());
    await waitFor(() => expect(mocks.listeners.has('chat://event')).toBe(true));
    deferOld = true;
    act(() => {
      mocks.listeners.get('chat://event')?.({
        payload: inputLifecycleEnvelope(1, 'persisted', oldConversation, 'old-input'),
      });
    });
    await waitFor(() => expect(resolveOld).not.toBeNull());

    act(() => {
      useConversationStore.setState({ activeId: newConversation });
    });
    await waitFor(() =>
      expect(hook.result.current.queuedInputs.map((item) => item.id)).toEqual(['generation-new'])
    );
    await act(async () => {
      resolveOld?.({
        queue_revision: 99,
        items: [inputProjection('generation-old', oldConversation, 'late old generation')],
      });
      await Promise.resolve();
    });

    expect(hook.result.current.queuedInputs.map((item) => item.id)).toEqual(['generation-new']);
    hook.unmount();
  });

  it('keeps a continuation turn separate from its root assistant message', async () => {
    const conversationId = 'conversation-continuation';
    const rootTurnId = 'root-message';
    const activeTurnId = 'continuation-turn';
    const snapshot = {
      workspace_id: 'global',
      surface: 'gui',
      conversation_id: conversationId,
      root_turn_id: rootTurnId,
      active_turn_id: activeTurnId,
      cancellation_requested: false,
    };
    useConversationStore.setState({ activeId: conversationId });
    useChatStore.getState().startAssistantMessage(rootTurnId);
    const replay: ChatEventReplay = {
      events: [
        agentEnvelope(
          activeTurnId,
          1,
          { type: 'token', data: 'continued' },
          conversationId,
          rootTurnId
        ),
      ],
      retained_earliest_cursor: 1,
      returned_earliest_cursor: 1,
      latest_cursor: 1,
      truncated: false,
    };
    mocks.apiInvoke.mockImplementation(async (command: string) => {
      if (command === 'get_active_chat_turn') return snapshot;
      if (command === 'replay_chat_events') return replay;
      return { success: true, turn_id: rootTurnId, status: 'cancelled' };
    });

    const hook = renderHook(() => useTauriChat());
    await waitFor(() => {
      expect(
        useChatStore.getState().messages.find((message) => message.id === rootTurnId)?.content
      ).toBe('continued');
    });
    await act(async () => {
      await hook.result.current.cancel();
    });
    expect(mocks.apiInvoke).toHaveBeenCalledWith('cancel_chat', {
      workspaceId: 'global',
      conversationId,
      expectedRootTurnId: rootTurnId,
      expectedActiveTurnId: activeTurnId,
    });
    hook.unmount();
  });

  it('replays a finished turn when the backend no longer has an active snapshot', async () => {
    const conversationId = 'conversation-rebind';
    const rootTurnId = 'turn-finished-while-unmounted';
    useConversationStore.setState({ activeId: conversationId });
    useChatStore.getState().startAssistantMessage(rootTurnId);
    const replay: ChatEventReplay = {
      events: [
        agentEnvelope(
          rootTurnId,
          1,
          { type: 'token', data: 'partial' },
          conversationId,
          rootTurnId
        ),
        agentEnvelope(
          rootTurnId,
          2,
          { type: 'final_answer', data: 'replayed final' },
          conversationId,
          rootTurnId
        ),
        turnStatusEnvelope(rootTurnId, rootTurnId, 3, 'completed', conversationId),
      ],
      retained_earliest_cursor: 1,
      returned_earliest_cursor: 1,
      latest_cursor: 3,
      truncated: false,
    };
    mocks.apiInvoke.mockImplementation(async (command: string) => {
      if (command === 'get_active_chat_turn') return null;
      if (command === 'replay_chat_events') return replay;
      return null;
    });

    const hook = renderHook(() => useTauriChat());
    await waitFor(() => {
      const assistant = useChatStore
        .getState()
        .messages.find((message) => message.id === rootTurnId);
      expect(assistant?.content).toBe('replayed final');
      expect(assistant?.isStreaming).toBe(false);
      expect(useChatStore.getState().runStatus).toBe('completed');
    });
    hook.unmount();
  });

  it('rehydrates Subagent projection from zero after its store was cleared', async () => {
    const conversationId = 'conversation-subagent-rehydrate';
    const rootTurnId = 'subagent-root';
    const snapshot = {
      ...activeSnapshot,
      conversation_id: conversationId,
      root_turn_id: rootTurnId,
      active_turn_id: rootTurnId,
    };
    const event = executionEnvelope(rootTurnId, 1, conversationId, {
      workspace_id: 'global',
      conversation_id: conversationId,
      run_id: '',
      scope: 'subagent',
      task_id: null,
      subagent_run_id: 'execution-rehydrate',
      event: 'started',
      agent: 'explorer',
      payload: { task: 'rehydrate' },
      framework_event: null,
    });
    const replay: ChatEventReplay = {
      events: [event],
      retained_earliest_cursor: 1,
      returned_earliest_cursor: 1,
      latest_cursor: 1,
      truncated: false,
    };
    useConversationStore.setState({ activeId: conversationId });
    mocks.apiInvoke.mockImplementation(async (command: string, args?: Record<string, unknown>) => {
      if (command === 'get_active_chat_turn') return snapshot;
      if (command === 'list_queued_chat_inputs') return { queue_revision: 0, items: [] };
      if (command === 'replay_chat_events') {
        return args?.afterCursor === 0 ? replay : emptyReplay();
      }
      return { success: true };
    });

    const first = renderHook(() => useTauriChat());
    await waitFor(() =>
      expect(
        useSubagentRunStore.getState().runs[subagentRunStoreKey('', 'execution-rehydrate')]
      ).toBeDefined()
    );
    first.unmount();
    useSubagentRunStore.getState().clear();

    const second = renderHook(() => useTauriChat());
    await waitFor(() =>
      expect(
        useSubagentRunStore.getState().runs[subagentRunStoreKey('', 'execution-rehydrate')]
      ).toBeDefined()
    );
    expect(mocks.apiInvoke).toHaveBeenCalledWith('replay_chat_events', {
      workspaceId: 'global',
      conversationId,
      messageKey: rootTurnId,
      afterCursor: 0,
    });
    second.unmount();
  });

  it('does not cancel or fail a replay with no active snapshot and no terminal fact', async () => {
    const conversationId = 'conversation-inactive-partial-replay';
    const rootTurnId = 'inactive-partial-turn';
    useConversationStore.setState({ activeId: conversationId });
    useChatStore.getState().startAssistantMessage(rootTurnId);
    const replay: ChatEventReplay = {
      events: [
        agentEnvelope(
          rootTurnId,
          1,
          { type: 'token', data: 'durable partial output' },
          conversationId,
          rootTurnId
        ),
      ],
      retained_earliest_cursor: 1,
      returned_earliest_cursor: 1,
      latest_cursor: 1,
      truncated: false,
    };
    mocks.apiInvoke.mockImplementation(async (command: string) => {
      if (command === 'get_active_chat_turn') return null;
      if (command === 'replay_chat_events') return replay;
      return null;
    });

    const hook = renderHook(() => useTauriChat());
    await waitFor(() => expect(useChatStore.getState().runStatus).toBe('idle'));
    expect(useChatStore.getState().isStreaming).toBe(false);
    expect(useChatStore.getState().messages.at(-1)?.content).toBe('durable partial output');
    expect(mocks.apiInvoke).not.toHaveBeenCalledWith('cancel_chat', expect.anything());
    hook.unmount();
  });

  it('keeps exact control refs until turn_status follows a final answer', async () => {
    const conversationId = 'conversation-final-before-status';
    const snapshot = { ...activeSnapshot, conversation_id: conversationId };
    useConversationStore.setState({ activeId: conversationId });
    mocks.apiInvoke.mockImplementation(async (command: string) => {
      if (command === 'get_active_chat_turn') return snapshot;
      if (command === 'replay_chat_events') return emptyReplay();
      if (command === 'cancel_chat') {
        return { success: true, turn_id: snapshot.root_turn_id, status: 'cancelled' };
      }
      return null;
    });

    const hook = renderHook(() => useTauriChat());
    await waitFor(() => expect(mocks.listeners.has('chat://event')).toBe(true));
    act(() => {
      mocks.listeners.get('chat://event')?.({
        payload: agentEnvelope(
          snapshot.active_turn_id,
          1,
          { type: 'final_answer', data: 'answer before settlement' },
          conversationId,
          snapshot.root_turn_id
        ),
      });
    });
    expect(useChatStore.getState().runStatus).toBe('running');
    expect(useChatStore.getState().isStreaming).toBe(true);

    await act(async () => {
      await hook.result.current.cancel();
    });
    expect(mocks.apiInvoke).toHaveBeenCalledWith('cancel_chat', {
      workspaceId: 'global',
      conversationId,
      expectedRootTurnId: snapshot.root_turn_id,
      expectedActiveTurnId: snapshot.active_turn_id,
    });
    hook.unmount();
  });

  it('ignores a late remount replay after a newer turn starts in the same conversation', async () => {
    const conversationId = 'conversation-replay-race';
    const oldRootTurnId = 'old-turn';
    let resolveReplay: (value: ChatEventReplay) => void = () => {};
    const pendingReplay = new Promise<ChatEventReplay>((resolve) => {
      resolveReplay = resolve;
    });
    useConversationStore.setState({ activeId: conversationId });
    useChatStore.getState().startAssistantMessage(oldRootTurnId);
    mocks.apiInvoke.mockImplementation(async (command: string) => {
      if (command === 'get_active_chat_turn') return null;
      if (command === 'replay_chat_events') return pendingReplay;
      if (command === 'send_chat_message') return { success: true };
      return null;
    });

    const hook = renderHook(() => useTauriChat());
    await waitFor(() => {
      expect(mocks.apiInvoke).toHaveBeenCalledWith(
        'replay_chat_events',
        expect.objectContaining({ conversationId })
      );
    });
    await act(async () => {
      await hook.result.current.sendMessage('new turn');
    });
    await act(async () => {
      resolveReplay({
        events: [
          agentEnvelope(
            oldRootTurnId,
            1,
            { type: 'final_answer', data: 'stale replay' },
            conversationId,
            oldRootTurnId
          ),
        ],
        retained_earliest_cursor: 1,
        returned_earliest_cursor: 1,
        latest_cursor: 1,
        truncated: false,
      });
      await pendingReplay;
    });

    const oldAssistant = useChatStore
      .getState()
      .messages.find((message) => message.id === oldRootTurnId);
    expect(oldAssistant?.content).toBe('');
    expect(oldAssistant?.isStreaming).toBe(true);
    hook.unmount();
  });

  it('已收到 completed 后，迟到的活跃快照不能把下一条普通输入改成 Steer', async () => {
    const conversationId = 'completed-before-recovery-snapshot';
    const snapshot = { ...activeSnapshot, conversation_id: conversationId };
    const lookup = deferred<typeof snapshot>();
    useConversationStore.setState({ activeId: conversationId });
    mocks.apiInvoke.mockImplementation(async (command: string, args?: Record<string, unknown>) => {
      if (command === 'get_active_chat_turn') return lookup.promise;
      if (command === 'replay_chat_events') return emptyReplay();
      if (command === 'list_queued_chat_inputs') return { queue_revision: 0, items: [] };
      if (command === 'send_chat_message') {
        const request = args?.request as { messageKey: string };
        return {
          kind: 'started',
          root_turn_id: request.messageKey,
          active_turn_id: request.messageKey,
        };
      }
      return { success: true };
    });
    const hook = renderHook(() => useTauriChat());
    await waitFor(() =>
      expect(mocks.apiInvoke).toHaveBeenCalledWith('get_active_chat_turn', {
        workspaceId: 'global',
        conversationId,
      })
    );
    act(() => {
      mocks.listeners.get('chat://event')?.({
        payload: turnStatusEnvelope(
          snapshot.active_turn_id,
          snapshot.root_turn_id,
          1,
          'completed',
          conversationId
        ),
      });
    });
    await act(async () => {
      lookup.resolve(snapshot);
      await lookup.promise;
    });
    mocks.apiInvoke.mockClear();
    await act(async () => {
      expect(await hook.result.current.sendMessage('完成后新要求')).toBe(true);
    });
    expect(mocks.apiInvoke).toHaveBeenCalledWith('send_chat_message', expect.anything());
    expect(mocks.apiInvoke).not.toHaveBeenCalledWith('queue_chat_input', expect.anything());
    hook.unmount();
  });

  it('resolves exact identity when Stop wins the mount-recovery race', async () => {
    useConversationStore.setState({ activeId: activeSnapshot.conversation_id });
    let resolveMountLookup: (value: typeof activeSnapshot) => void = () => {};
    const pendingMountLookup = new Promise<typeof activeSnapshot>((resolve) => {
      resolveMountLookup = resolve;
    });
    let lookupCount = 0;
    mocks.apiInvoke.mockImplementation(async (command: string) => {
      if (command === 'get_active_chat_turn') {
        lookupCount += 1;
        if (lookupCount === 1) {
          return pendingMountLookup;
        }
        return activeSnapshot;
      }
      if (command === 'replay_chat_events') return emptyReplay();
      return { success: true, turn_id: activeSnapshot.root_turn_id, status: 'cancelled' };
    });

    const hook = renderHook(() => useTauriChat());
    await waitFor(() => {
      expect(lookupCount).toBe(1);
      expect(mocks.listeners.has('chat://event')).toBe(true);
    });
    await act(async () => {
      await hook.result.current.cancel();
    });
    expect(mocks.apiInvoke).toHaveBeenCalledWith('cancel_chat', {
      workspaceId: 'global',
      conversationId: activeSnapshot.conversation_id,
      expectedRootTurnId: activeSnapshot.root_turn_id,
      expectedActiveTurnId: activeSnapshot.active_turn_id,
    });
    expect(useChatStore.getState().runStatus).toBe('running');
    await act(async () => {
      resolveMountLookup(activeSnapshot);
      await pendingMountLookup;
    });
    act(() => {
      mocks.listeners.get('chat://event')?.({
        payload: turnStatusEnvelope(
          activeSnapshot.active_turn_id,
          activeSnapshot.root_turn_id,
          1,
          'cancelled',
          activeSnapshot.conversation_id
        ),
      });
    });
    expect(useChatStore.getState().runStatus).toBe('cancelled');

    // The late mount response carried a now-settled snapshot. It must not
    // restore stale refs that could target a later turn.
    mocks.apiInvoke.mockClear();
    mocks.apiInvoke.mockResolvedValue(null);
    await act(async () => {
      await hook.result.current.cancel();
    });
    expect(mocks.apiInvoke).not.toHaveBeenCalledWith('cancel_chat', expect.anything());
    hook.unmount();
  });

  it('clears transient UI without fabricating failure when no active turn is recovered', async () => {
    mocks.apiInvoke.mockResolvedValue(null);
    const hook = renderHook(() => useTauriChat());
    await act(async () => {
      await hook.result.current.cancel();
    });
    expect(useToastStore.getState().toasts).toEqual([]);
    expect(mocks.apiInvoke).not.toHaveBeenCalledWith('cancel_chat', expect.anything());
    expect(useChatStore.getState().runStatus).toBe('idle');
    expect(useChatStore.getState().isStreaming).toBe(false);
    expect(useChatStore.getState().isCancelled).toBe(false);
    hook.unmount();
  });

  it('replays terminal state when Stop races with backend turn settlement', async () => {
    const conversationId = 'conversation-stop-settlement-race';
    const rootTurnId = 'root-stop-settlement-race';
    const snapshot = {
      workspace_id: 'global',
      surface: 'gui',
      conversation_id: conversationId,
      root_turn_id: rootTurnId,
      active_turn_id: rootTurnId,
      cancellation_requested: false,
    };
    const replay: ChatEventReplay = {
      events: [turnStatusEnvelope(rootTurnId, rootTurnId, 1, 'completed', conversationId)],
      retained_earliest_cursor: 1,
      returned_earliest_cursor: 1,
      latest_cursor: 1,
      truncated: false,
    };
    useConversationStore.setState({ activeId: conversationId });
    useChatStore.getState().startAssistantMessage(rootTurnId);
    let replayCount = 0;
    mocks.apiInvoke.mockImplementation(async (command: string) => {
      if (command === 'get_active_chat_turn') return snapshot;
      if (command === 'cancel_chat') {
        return { success: true, turn_id: rootTurnId, status: 'already_settled' };
      }
      if (command === 'replay_chat_events') {
        replayCount += 1;
        return replayCount === 1 ? emptyReplay() : replay;
      }
      return null;
    });

    const hook = renderHook(() => useTauriChat());
    await waitFor(() => {
      expect(replayCount).toBe(1);
    });
    expect(useChatStore.getState().runStatus).toBe('running');
    expect(useChatStore.getState().isStreaming).toBe(true);
    await act(async () => {
      await hook.result.current.cancel();
    });

    expect(mocks.apiInvoke).toHaveBeenCalledWith('cancel_chat', {
      workspaceId: 'global',
      conversationId,
      expectedRootTurnId: rootTurnId,
      expectedActiveTurnId: rootTurnId,
    });
    expect(mocks.apiInvoke).toHaveBeenCalledWith('replay_chat_events', {
      workspaceId: 'global',
      conversationId,
      messageKey: rootTurnId,
      afterCursor: expect.any(Number),
    });
    expect(useToastStore.getState().toasts).toEqual([]);
    expect(useChatStore.getState().runStatus).toBe('completed');
    expect(useChatStore.getState().isStreaming).toBe(false);
    hook.unmount();
  });

  it('clears transient UI without inventing an outcome when terminal replay is unavailable', async () => {
    const consoleWarn = vi.spyOn(console, 'warn').mockImplementation(() => {});
    const conversationId = 'conversation-stop-replay-failure';
    const rootTurnId = 'root-stop-replay-failure';
    const snapshot = {
      workspace_id: 'global',
      surface: 'gui',
      conversation_id: conversationId,
      root_turn_id: rootTurnId,
      active_turn_id: rootTurnId,
      cancellation_requested: false,
    };
    useConversationStore.setState({ activeId: conversationId });
    useChatStore.getState().startAssistantMessage(rootTurnId);
    let replayCount = 0;
    mocks.apiInvoke.mockImplementation(async (command: string) => {
      if (command === 'get_active_chat_turn') return snapshot;
      if (command === 'cancel_chat') {
        return { success: true, turn_id: rootTurnId, status: 'already_settled' };
      }
      if (command === 'replay_chat_events') {
        replayCount += 1;
        if (replayCount === 1) return emptyReplay();
        throw new Error('journal unavailable');
      }
      return null;
    });

    const hook = renderHook(() => useTauriChat());
    await waitFor(() => {
      expect(replayCount).toBe(1);
    });
    await act(async () => {
      await hook.result.current.cancel();
    });

    expect(useToastStore.getState().toasts).toEqual([]);
    expect(useChatStore.getState().runStatus).toBe('idle');
    expect(useChatStore.getState().isStreaming).toBe(false);
    expect(consoleWarn).toHaveBeenCalledWith(
      '[TauriChat] Failed to reconcile an already-settled turn:',
      expect.any(Error)
    );
    consoleWarn.mockRestore();
    hook.unmount();
  });

  it('keeps the running terminal projection when snapshot IPC fails', async () => {
    const consoleError = vi.spyOn(console, 'error').mockImplementation(() => {});
    useConversationStore.setState({ activeId: activeSnapshot.conversation_id });
    mocks.apiInvoke.mockRejectedValue(new Error('snapshot unavailable'));
    const hook = renderHook(() => useTauriChat());
    await act(async () => {
      await hook.result.current.cancel();
    });
    expect(useToastStore.getState().toasts).toEqual(
      expect.arrayContaining([
        expect.objectContaining({
          type: 'error',
          message: '停止任务失败：Error: snapshot unavailable',
        }),
      ])
    );
    expect(useChatStore.getState().runStatus).toBe('running');
    expect(useChatStore.getState().isCancelled).toBe(false);
    consoleError.mockRestore();
    hook.unmount();
  });

  it.each(['input', 'selection'] as const)(
    'passes original scope and kind through the real %s reply call',
    async (kind) => {
      const identity = { ...hitlIdentity(`request-${kind}`), kind };
      const previous = mocks.apiInvoke.getMockImplementation();
      mocks.apiInvoke.mockImplementation(async (command, args) =>
        command === `send_${kind}_response`
          ? { submitted: true, response_target: args.responseTarget }
          : previous?.(command, args)
      );
      if (kind === 'input')
        useChatStore
          .getState()
          .enqueueHitlRequest({ kind, requestId: identity.request_id, identity, prompt: 'input' });
      else
        useChatStore.getState().enqueueHitlRequest({
          kind,
          requestId: identity.request_id,
          identity,
          prompt: 'choose',
          options: ['Continue'],
        });
      const hook = renderHook(() => useTauriChat());
      await act(async () => {
        if (kind === 'input') await hook.result.current.sendInput(identity, '中文\n🙂');
        else await hook.result.current.sendSelection(identity, 'Continue');
      });
      expect(mocks.apiInvoke).toHaveBeenCalledWith(
        `send_${kind}_response`,
        expect.objectContaining({
          workspaceId: 'original-workspace',
          conversationId: 'original-conversation',
          expectedRootTurnId: 'original-root',
          expectedActiveTurnId: 'original-turn',
          responseTarget: identity.response_target,
          requestScope: identity.scope,
          requestKind: kind,
        })
      );
      expect(useChatStore.getState().pendingHitlRequests).toEqual([]);
      hook.unmount();
    }
  );

  it('does not let an earlier submission acknowledgement remove a reused-id successor', async () => {
    const original = {
      ...hitlIdentity('reusable'),
      kind: 'input' as const,
      response_target: 'old-target',
    };
    const successor = { ...original, response_target: 'new-target' };
    let finish: ((receipt: { submitted: boolean; response_target: string }) => void) | undefined;
    const previous = mocks.apiInvoke.getMockImplementation();
    mocks.apiInvoke.mockImplementation((command, args) =>
      command === 'send_input_response'
        ? new Promise((resolve) => {
            finish = resolve;
          })
        : previous?.(command, args)
    );
    useChatStore
      .getState()
      .enqueueHitlRequest({ kind: 'input', requestId: 'reusable', identity: original });
    const hook = renderHook(() => useTauriChat());
    let pending: Promise<boolean> | undefined;
    await act(async () => {
      pending = hook.result.current.sendInput(original, 'answer');
    });
    expect(finish).toBeTypeOf('function');
    act(() => {
      useChatStore.getState().removeHitlRequest(original);
      useChatStore
        .getState()
        .enqueueHitlRequest({ kind: 'input', requestId: 'reusable', identity: successor });
    });
    await act(async () => {
      finish?.({ submitted: true, response_target: 'old-target' });
      await pending;
    });
    expect(useChatStore.getState().pendingHitlRequests).toEqual([
      expect.objectContaining({ identity: successor }),
    ]);
    hook.unmount();
  });

  it.each(['input', 'selection'] as const)(
    'keeps %s waiting when transport success is not a submission receipt',
    async (kind) => {
      const identity = { ...hitlIdentity('not-submitted'), kind };
      const previous = mocks.apiInvoke.getMockImplementation();
      mocks.apiInvoke.mockImplementation(async (command, args) =>
        command === `send_${kind}_response` ? { success: true } : previous?.(command, args)
      );
      if (kind === 'input')
        useChatStore
          .getState()
          .enqueueHitlRequest({ kind, requestId: identity.request_id, identity });
      else
        useChatStore.getState().enqueueHitlRequest({
          kind,
          requestId: identity.request_id,
          identity,
          prompt: 'choose',
          options: ['Continue'],
        });
      const hook = renderHook(() => useTauriChat());
      await act(async () => {
        const submitted =
          kind === 'input'
            ? await hook.result.current.sendInput(identity, 'answer')
            : await hook.result.current.sendSelection(identity, 'Continue');
        expect(submitted).toBe(false);
      });
      expect(useChatStore.getState().pendingHitlRequests).toEqual([
        expect.objectContaining({ identity }),
      ]);
      hook.unmount();
    }
  );

  it('removes only the acknowledged HITL request and keeps the next one waiting', async () => {
    mocks.apiInvoke.mockImplementation(async (command, args) =>
      command === 'send_approval_response'
        ? { submitted: true, response_target: args.responseTarget }
        : undefined
    );
    useChatStore.getState().enqueueHitlRequest({
      kind: 'approval',
      requestId: 'approval-first',
      identity: hitlIdentity('approval-first'),
      toolName: 'write_file',
      args: { path: 'first.txt' },
    });
    useChatStore.getState().enqueueHitlRequest({
      kind: 'approval',
      requestId: 'approval-second',
      identity: hitlIdentity('approval-second'),
      toolName: 'write_file',
      args: { path: 'second.txt' },
    });
    const hook = renderHook(() => useTauriChat());

    await act(async () => {
      await hook.result.current.sendApproval(hitlIdentity('approval-first'), true);
    });

    expect(mocks.apiInvoke).toHaveBeenCalledWith('send_approval_response', {
      workspaceId: 'original-workspace',
      conversationId: 'original-conversation',
      expectedRootTurnId: 'original-root',
      expectedActiveTurnId: 'original-turn',
      requestId: 'approval-first',
      responseTarget: 'target-approval-first',
      requestScope: hitlIdentity('approval-first').scope,
      requestKind: 'approval',
      approved: true,
      reason: undefined,
      scope: undefined,
    });
    expect(useChatStore.getState().pendingHitlRequests).toEqual([
      expect.objectContaining({ requestId: 'approval-second' }),
    ]);
    expect(useChatStore.getState().runStatus).toBe('waiting_approval');

    await act(async () => {
      await hook.result.current.sendApproval(hitlIdentity('approval-second'), false, 'not now');
    });
    expect(useChatStore.getState().pendingHitlRequests).toEqual([]);
    expect(useChatStore.getState().runStatus).toBe('running');
    hook.unmount();
  });
});
