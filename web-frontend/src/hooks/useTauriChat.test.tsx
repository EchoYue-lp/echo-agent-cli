// @vitest-environment jsdom
import { act, renderHook, waitFor } from '@testing-library/react';
import { beforeEach, describe, expect, it, vi } from 'vitest';
import { useConversationStore } from '../stores/conversationStore';
import { useChatStore } from '../stores/chatStore';
import { useToastStore } from '../stores/toastStore';
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
  schema_version: 1,
  event_id: `chat-event-${sequence}`,
  content_hash: `content-hash-${sequence}`,
  sequence,
  stream_id: conversationId ? `conversation:${conversationId}` : `message:${messageId}`,
  conversation_id: conversationId,
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
  schema_version: 1,
  event_id: `chat-status-${sequence}`,
  content_hash: `status-hash-${sequence}`,
  sequence,
  stream_id: `conversation:${conversationId}`,
  conversation_id: conversationId,
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
    useConversationStore.setState({ activeId: null });
    useChatStore.getState().clearMessages();
    useChatStore.setState({ pendingHitlRequests: [] });
    useChatStore.getState().setRunStatus('running');
    useToastStore.getState().clearAll();
    mocks.apiInvoke.mockImplementation(async (command: string) => {
      if (command === 'get_active_chat_turn') {
        return activeSnapshot;
      }
      if (command === 'replay_chat_events') return emptyReplay();
      return { success: true, turn_id: activeSnapshot.root_turn_id, status: 'cancelled' };
    });
  });

  it('restores the real message scope after remount and cancels that exact turn', async () => {
    const first = renderHook(() => useTauriChat());
    await waitFor(() => {
      expect(mocks.apiInvoke).toHaveBeenCalledWith('get_active_chat_turn', {
        conversationId: undefined,
        conversation_id: undefined,
      });
    });
    first.unmount();

    mocks.apiInvoke.mockClear();
    const remounted = renderHook(() => useTauriChat());
    await waitFor(() => {
      expect(mocks.apiInvoke).toHaveBeenCalledWith('get_active_chat_turn', {
        conversationId: undefined,
        conversation_id: undefined,
      });
    });
    await act(async () => {
      await remounted.result.current.cancel();
    });
    expect(mocks.apiInvoke).toHaveBeenCalledWith('cancel_chat', {
      conversationId: activeSnapshot.conversation_id,
      conversation_id: activeSnapshot.conversation_id,
      rootTurnId: activeSnapshot.root_turn_id,
      root_turn_id: activeSnapshot.root_turn_id,
    });
    remounted.unmount();
  });

  it('keeps a continuation turn separate from its root assistant message', async () => {
    const conversationId = 'conversation-continuation';
    const rootTurnId = 'root-message';
    const activeTurnId = 'continuation-turn';
    const snapshot = {
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
      conversationId,
      conversation_id: conversationId,
      rootTurnId,
      root_turn_id: rootTurnId,
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
      conversationId: activeSnapshot.conversation_id,
      conversation_id: activeSnapshot.conversation_id,
      rootTurnId: activeSnapshot.root_turn_id,
      root_turn_id: activeSnapshot.root_turn_id,
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

  it('keeps the running terminal projection when snapshot IPC fails', async () => {
    const consoleError = vi.spyOn(console, 'error').mockImplementation(() => {});
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
      requestId: 'approval-first',
      request_id: 'approval-first',
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
