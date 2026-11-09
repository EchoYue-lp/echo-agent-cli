import { beforeEach, describe, expect, it } from 'vitest';
import fixtureText from '../fixtures/chat-event-envelope-v4.json?raw';
import type { ChatEventEnvelope } from '../types/api';
import { useChatStore } from '../stores/chatStore';
import { handleChatEventEnvelope } from './chatEventHandler';
import { chatMessagesToSaved } from '../stores/conversationStore';

function envelope(executionId = 'execution'): ChatEventEnvelope {
  const event = (JSON.parse(fixtureText) as ChatEventEnvelope[]).at(0);
  if (!event || event.payload.source !== 'agent') throw new Error('agent fixture missing');
  return {
    ...event,
    payload: {
      source: 'agent',
      event: {
        ...event.payload.event,
        execution_id: executionId,
        payload: {
          type: 'assistant_message',
          data: {
            block: {
              block_id: `block:${executionId}`,
              request_id: 'request',
              sequence: 0,
              phase: 'commentary',
              delta: '原进度',
              complete: true,
              provider: null,
              reasoning: null,
            },
          },
        },
      },
    },
  };
}

function context() {
  return {
    assistantIdRef: { current: 'assistant' as string | null },
    currentMessageKeyRef: { current: 'fixture-message' as string | null },
    currentMessageIdRef: { current: 'fixture-message' as string | null },
    isCancelledRef: { current: false },
    currentThinkingIdRef: { current: null as string | null },
  };
}

beforeEach(() => {
  useChatStore.getState().clearMessages();
  useChatStore.getState().startAssistantMessage('assistant');
});

describe('原publication轮次坐标', () => {
  it('只由相同内外作用域和当前root关联，保存DTO保持原来源', () => {
    const event = envelope();
    handleChatEventEnvelope(event, context());
    const source = useChatStore.getState().messages.at(-1)?.turnSource;
    expect(source).toEqual({
      workspace_id: 'fixture-workspace',
      conversation_id: 'fixture-conversation',
      root_turn_id: 'fixture-message',
      turn_id: 'fixture-turn',
      execution_id: 'execution',
    });
    expect(chatMessagesToSaved(useChatStore.getState().messages).at(-1)?.turn_source).toEqual(
      source
    );
    expect(useChatStore.getState().messages.at(-1)?.finalAnswerConfirmed).not.toBe(true);
  });

  it('同一消息出现冲突execution后保留未关联，不被迟到旧owner覆盖', () => {
    const ctx = context();
    handleChatEventEnvelope(envelope(), ctx);
    handleChatEventEnvelope(envelope('foreign-execution'), ctx);
    expect(useChatStore.getState().messages.at(-1)?.turnSource).toBeNull();
    handleChatEventEnvelope(envelope(), ctx);
    expect(useChatStore.getState().messages.at(-1)?.turnSource).toBeNull();
  });

  it('原内外turn不一致或当前root不同，不从相邻正文推来源', () => {
    const event = envelope();
    if (event.payload.source !== 'agent') throw new Error('agent fixture missing');
    event.payload.event.turn_id = 'other-turn';
    handleChatEventEnvelope(event, context());
    expect(useChatStore.getState().messages.at(-1)?.turnSource).toBeUndefined();
    const ctx = context();
    ctx.currentMessageKeyRef.current = 'different-root';
    handleChatEventEnvelope(envelope(), ctx);
    expect(useChatStore.getState().messages.at(-1)?.turnSource).toBeUndefined();
  });
});
