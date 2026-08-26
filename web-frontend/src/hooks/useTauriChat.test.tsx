// @vitest-environment jsdom
import { act, renderHook, waitFor } from '@testing-library/react';
import { beforeEach, describe, expect, it, vi } from 'vitest';
import { useConversationStore } from '../stores/conversationStore';
import { useChatStore } from '../stores/chatStore';
import { useToastStore } from '../stores/toastStore';
import { useTaskRuntimeStore } from '../stores/taskRuntimeStore';
import type { AgentEvent, ChatEventEnvelope, ChatEventReplay } from '../types/api';
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
  status: 'completed' | 'failed' | 'cancelled',
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
    mocks.apiInvoke.mockImplementation(async (command: string, args?: Record<string, unknown>) => {
      if (command === 'get_active_chat_turn') {
        return activeSnapshot;
      }
      if (command === 'replay_chat_events') return emptyReplay();
      if (command === 'list_queued_chat_inputs') return [];
      if (command === 'queue_chat_input') {
        return {
          input_id: String(args?.inputId ?? 'queued-input'),
          workspace_id: String(args?.workspaceId ?? 'global'),
          conversation_id: String(args?.conversationId ?? ''),
          text: String(args?.text ?? ''),
          attachments: args?.attachments ?? [],
          submitted_at_ms: Date.now(),
        };
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
    expect(mocks.apiInvoke).toHaveBeenCalledWith(
      'send_chat_message',
      expect.objectContaining({ message: 'start an independent conversation' })
    );
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
      expect(mocks.apiInvoke).toHaveBeenCalledWith(
        'send_chat_message',
        expect.objectContaining({
          workspaceId: 'global',
          conversationId: 'conversation-admission',
        })
      );
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
    expect(hook.result.current.queuedInputs).toEqual([
      expect.objectContaining({ id: 'input-accepted', backendManaged: true }),
    ]);
    hook.unmount();
  });

  it('cancels the exact conflicting run before starting the new input once', async () => {
    useConversationStore.setState({ activeId: 'conversation-conflict' });
    let sendCount = 0;
    mocks.apiInvoke.mockImplementation(async (command: string) => {
      if (command === 'get_active_chat_turn') return null;
      if (command === 'replay_chat_events') return emptyReplay();
      if (command === 'send_chat_message') {
        sendCount += 1;
        return sendCount === 1
          ? {
              kind: 'task_run_conflict',
              run_id: 'run-existing',
              goal: 'old goal',
              new_message: 'replacement request',
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

    await act(async () => {
      await useConversationStore.getState().startNew();
    });
    expect(useConversationStore.getState().activeId).toBeNull();
    expect(hook.result.current.queuedInputs).toEqual([]);
    expect(mocks.apiInvoke).not.toHaveBeenCalledWith('reset_session');
    hook.unmount();
  });

  it('rehydrates a backend-managed queue for the exact workspace conversation', async () => {
    const conversationId = 'conversation-durable-queue';
    useConversationStore.setState({ activeId: conversationId });
    mocks.apiInvoke.mockImplementation(async (command: string) => {
      if (command === 'get_active_chat_turn') {
        return { ...activeSnapshot, conversation_id: conversationId };
      }
      if (command === 'replay_chat_events') return emptyReplay();
      if (command === 'list_queued_chat_inputs') {
        return [
          {
            input_id: 'durable-input',
            workspace_id: 'global',
            conversation_id: conversationId,
            text: 'continue after restart',
            attachments: [],
            submitted_at_ms: 1,
          },
        ];
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
          backendManaged: true,
        }),
      ]);
    });
    hook.unmount();
  });

  it('keeps the durable queue item when live steer is not accepted', async () => {
    const conversationId = 'conversation-not-steerable';
    useConversationStore.setState({ activeId: conversationId });
    mocks.apiInvoke.mockImplementation(async (command: string, args?: Record<string, unknown>) => {
      if (command === 'get_active_chat_turn') {
        return { ...activeSnapshot, conversation_id: conversationId };
      }
      if (command === 'replay_chat_events') return emptyReplay();
      if (command === 'list_queued_chat_inputs') return [];
      if (command === 'queue_chat_input') {
        return {
          input_id: String(args?.inputId),
          workspace_id: 'global',
          conversation_id: conversationId,
          text: String(args?.text),
          attachments: [],
          submitted_at_ms: 1,
        };
      }
      if (command === 'steer_chat_message') {
        return { kind: 'not_steerable', turn_id: activeSnapshot.active_turn_id };
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
    await act(async () => {
      await hook.result.current.sendMessage('keep this follow-up');
    });
    const queuedId = hook.result.current.queuedInputs.at(0)?.id;
    expect(queuedId).toBeTruthy();

    let accepted = true;
    await act(async () => {
      accepted = await hook.result.current.steerQueuedMessage(String(queuedId));
    });

    expect(accepted).toBe(false);
    expect(hook.result.current.queuedInputs).toEqual([
      expect.objectContaining({ id: queuedId, text: 'keep this follow-up', backendManaged: true }),
    ]);
    expect(
      mocks.apiInvoke.mock.calls.filter(([command]) => command === 'queue_chat_input')
    ).toHaveLength(1);
    expect(
      mocks.apiInvoke.mock.calls.filter(([command]) => command === 'steer_chat_message')
    ).toHaveLength(1);
    expect(
      mocks.apiInvoke.mock.calls.filter(([command]) => command === 'remove_queued_chat_input')
    ).toHaveLength(0);
    hook.unmount();
  });

  it('removes one accepted steer while the foreground turn remains unsettled', async () => {
    const conversationId = 'conversation-steer-accepted';
    useConversationStore.setState({ activeId: conversationId });
    mocks.apiInvoke.mockImplementation(async (command: string, args?: Record<string, unknown>) => {
      if (command === 'get_active_chat_turn') {
        return { ...activeSnapshot, conversation_id: conversationId };
      }
      if (command === 'replay_chat_events') return emptyReplay();
      if (command === 'list_queued_chat_inputs') return [];
      if (command === 'queue_chat_input') {
        return {
          input_id: String(args?.inputId),
          workspace_id: 'global',
          conversation_id: conversationId,
          text: String(args?.text),
          attachments: [],
          submitted_at_ms: 1,
        };
      }
      if (command === 'steer_chat_message') {
        return { kind: 'accepted', turn_id: activeSnapshot.active_turn_id };
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
    await act(async () => {
      await hook.result.current.sendMessage('inject this once');
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
      message: 'inject this once',
      attachments: [],
      conversationId,
      expectedRootTurnId: activeSnapshot.root_turn_id,
      expectedActiveTurnId: activeSnapshot.active_turn_id,
    });
    expect(
      mocks.apiInvoke.mock.calls.filter(([command]) => command === 'queue_chat_input')
    ).toHaveLength(1);
    expect(
      mocks.apiInvoke.mock.calls.filter(([command]) => command === 'steer_chat_message')
    ).toHaveLength(1);
    expect(
      mocks.apiInvoke.mock.calls.filter(([command]) => command === 'remove_queued_chat_input')
    ).toHaveLength(1);
    expect(useChatStore.getState().runStatus).toBe('running');
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
        payload: agentEnvelope(
          activeSnapshot.active_turn_id,
          1,
          { type: 'cancelled' },
          activeSnapshot.conversation_id,
          activeSnapshot.root_turn_id
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

  it('settles stale UI state when no active backend turn can be recovered', async () => {
    mocks.apiInvoke.mockResolvedValue(null);
    const hook = renderHook(() => useTauriChat());
    await act(async () => {
      await hook.result.current.cancel();
    });
    expect(useToastStore.getState().toasts).toEqual([]);
    expect(mocks.apiInvoke).not.toHaveBeenCalledWith('cancel_chat', expect.anything());
    expect(useChatStore.getState().runStatus).toBe('failed');
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

  it('settles stale UI without a Stop error when terminal replay is unavailable', async () => {
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
    expect(useChatStore.getState().runStatus).toBe('failed');
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
