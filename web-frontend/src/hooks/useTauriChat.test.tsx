// @vitest-environment jsdom
import { act, renderHook, waitFor } from '@testing-library/react';
import { beforeEach, describe, expect, it, vi } from 'vitest';
import { useConversationStore } from '../stores/conversationStore';
import { useChatStore } from '../stores/chatStore';
import { useToastStore } from '../stores/toastStore';
import { useTaskRuntimeStore } from '../stores/taskRuntimeStore';
import type {
  AgentEvent,
  ChatEventEnvelope,
  ChatEventReplay,
  ConversationInputFact,
  ConversationInputFrontier,
  ConversationInputProjection,
} from '../types/api';
import { resetChatEventCursorsForTest } from './chatEventSequencer';
import { useTauriChat } from './useTauriChat';

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

describe('useTauriChat foreground turn recovery', () => {
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
    useConversationStore.setState({ activeId: null, newConversationEpoch: 0 });
    useChatStore.getState().clearMessages();
    useChatStore.setState({ pendingHitlRequests: [] });
    useChatStore.getState().setRunStatus('running');
    useToastStore.getState().clearAll();
    useTaskRuntimeStore.getState().reset();
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
      if (command === 'save_conversation') return { success: true, id: 'conversation-new' };
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
    await waitFor(() => expect(mocks.listeners.has('execution://event')).toBe(true));

    act(() => {
      mocks.listeners.get('execution://event')?.({
        payload: {
          kind: 'run',
          event: 'plan_revision_committed',
          workspace_id: 'global',
          conversation_id: conversationId,
          run_id: 'taskrun:conversation-plan-committed',
        },
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

  it('removes only the acknowledged HITL request and keeps the next one waiting', async () => {
    useChatStore.getState().enqueueHitlRequest({
      kind: 'approval',
      requestId: 'approval-first',
      toolName: 'write_file',
      args: { path: 'first.txt' },
    });
    useChatStore.getState().enqueueHitlRequest({
      kind: 'approval',
      requestId: 'approval-second',
      toolName: 'write_file',
      args: { path: 'second.txt' },
    });
    const hook = renderHook(() => useTauriChat());

    await act(async () => {
      await hook.result.current.sendApproval('approval-first', true);
    });

    expect(mocks.apiInvoke).toHaveBeenCalledWith('send_approval_response', {
      workspaceId: 'global',
      conversationId: null,
      expectedRootTurnId: null,
      expectedActiveTurnId: null,
      requestId: 'approval-first',
      approved: true,
      reason: undefined,
      scope: undefined,
    });
    expect(useChatStore.getState().pendingHitlRequests).toEqual([
      expect.objectContaining({ requestId: 'approval-second' }),
    ]);
    expect(useChatStore.getState().runStatus).toBe('waiting_approval');

    await act(async () => {
      await hook.result.current.sendApproval('approval-second', false, 'not now');
    });
    expect(useChatStore.getState().pendingHitlRequests).toEqual([]);
    expect(useChatStore.getState().runStatus).toBe('running');
    hook.unmount();
  });
});
