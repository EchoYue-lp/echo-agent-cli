import { beforeEach, describe, expect, it } from 'vitest';
import type { ChatEventEnvelope, HitlRequestIdentity } from '../types/api';
import { useChatStore } from '../stores/chatStore';
import { handleChatEventEnvelope } from './chatEventHandler';
import fixtureText from '../fixtures/chat-event-envelope-v4.json?raw';

const original: HitlRequestIdentity = {
  request_id: 'reusable-question',
  response_target: 'original-answer-target',
  kind: 'input',
  scope: {
    workspace_id: 'workspace-original',
    host_generation: 'host-original',
    conversation_id: 'conversation-original',
    root_turn_id: 'root-original',
    turn_id: 'turn-original',
  },
};

function envelope(identity: HitlRequestIdentity): ChatEventEnvelope {
  const base = (JSON.parse(fixtureText) as ChatEventEnvelope[]).at(0);
  if (!base) throw new Error('missing canonical envelope fixture');
  return {
    ...base,
    workspace_id: 'workspace-original',
    conversation_id: 'conversation-original',
    root_turn_id: 'root-original',
    turn_id: 'turn-original',
    payload: { source: 'hitl_request_released', event: { identity } },
  };
}

function context() {
  return {
    assistantIdRef: { current: null as string | null },
    currentMessageKeyRef: { current: 'root-original' as string | null },
    currentMessageIdRef: { current: 'message-original' as string | null },
    isCancelledRef: { current: false },
    currentThinkingIdRef: { current: null as string | null },
  };
}

function enqueue(identity: HitlRequestIdentity) {
  useChatStore.getState().enqueueHitlRequest({
    kind: 'input',
    requestId: identity.request_id,
    identity,
    prompt: '恢复后仍待回答的中文问题🙂',
  });
}

beforeEach(() => {
  useChatStore.setState({
    messages: [],
    pendingHitlRequests: [],
    runStatus: 'idle',
    activity: null,
  });
  useChatStore.getState().setRunStatus('running');
});

describe('HITL release replay ownership', () => {
  it('does not count a rejected request envelope as progress of the active operation', () => {
    enqueue(original);
    useChatStore.getState().observeActivity('original-operation', '原操作仍在等待', 100);
    const invalid: ChatEventEnvelope = {
      ...envelope(original),
      workspace_id: 'foreign-workspace',
      payload: {
        source: 'input_request',
        event: {
          identity: original,
          request_id: original.request_id,
          prompt: '错误来源的问题',
        },
      },
    };
    handleChatEventEnvelope(invalid, context());
    expect(useChatStore.getState().pendingHitlRequests).toHaveLength(1);
    expect(useChatStore.getState().activity?.lastEventAt).toBe(100);
  });

  for (const coordinate of [
    'workspace_id',
    'conversation_id',
    'root_turn_id',
    'turn_id',
  ] as const) {
    it(`keeps the pending request when the release envelope has a foreign ${coordinate}`, () => {
      enqueue(original);
      useChatStore.getState().observeActivity('original-operation', '原操作仍在等待', 100);
      const release = envelope(original);
      release[coordinate] = 'foreign-coordinate';
      handleChatEventEnvelope(release, context());
      expect(useChatStore.getState().pendingHitlRequests).toHaveLength(1);
      expect(useChatStore.getState().pendingHitlRequests.at(0)?.identity).toEqual(original);
      expect(useChatStore.getState().runStatus).toBe('waiting_input');
      expect(useChatStore.getState().activity?.lastEventAt).toBe(100);
    });
  }

  it('releases the matching request and preserves the next request and its waiting state', () => {
    const next = { ...original, request_id: 'next-question', response_target: 'next-target' };
    enqueue(original);
    enqueue(next);
    useChatStore.getState().observeActivity('original-operation', '原操作仍在等待', 100);
    handleChatEventEnvelope(envelope(original), context());
    expect(useChatStore.getState().pendingHitlRequests.map((request) => request.identity)).toEqual([
      next,
    ]);
    expect(useChatStore.getState().runStatus).toBe('waiting_input');
    expect(useChatStore.getState().activity?.lastEventAt).toBe(100);
  });

  it('ignores a duplicate old release when the request id is reused after recovery', () => {
    const replacement = { ...original, response_target: 'recovered-answer-target' };
    enqueue(replacement);
    handleChatEventEnvelope(envelope(original), context());
    expect(useChatStore.getState().pendingHitlRequests.at(0)?.identity).toEqual(replacement);
    handleChatEventEnvelope(envelope(replacement), context());
    expect(useChatStore.getState().pendingHitlRequests).toEqual([]);
  });

  it('still consumes exact cleanup after cancellation and does not manufacture a terminal outcome', () => {
    enqueue(original);
    const ctx = context();
    ctx.isCancelledRef.current = true;
    handleChatEventEnvelope(envelope(original), ctx);
    expect(useChatStore.getState().pendingHitlRequests).toEqual([]);
    expect(useChatStore.getState().runStatus).toBe('running');
    expect(ctx.isCancelledRef.current).toBe(true);
  });
});
