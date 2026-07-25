import { beforeEach, describe, expect, it } from 'vitest';
import { useChatStore } from './chatStore';

describe('chat tool execution projection', () => {
  beforeEach(() => {
    useChatStore.getState().clearMessages();
    useChatStore.setState({ currentRound: null, runStatus: 'idle' });
  });

  it('stores execution order and rounds as stable execution IDs', () => {
    const store = useChatStore.getState();
    store.startAssistantMessage('assistant-test');
    store.startToolBatch(2);
    store.recordToolStart('assistant-test', 'detail-a');
    store.recordToolStart('assistant-test', 'detail-b');
    store.endToolBatch();

    const message = useChatStore.getState().messages[0];
    expect(message?.executionSteps).toEqual([
      { type: 'tool', callId: 'detail-a' },
      { type: 'tool', callId: 'detail-b' },
    ]);
    expect(message?.executionRounds).toEqual([{ toolCallIds: ['detail-a', 'detail-b'] }]);
  });

  it('ignores duplicate start events for one execution ID', () => {
    const store = useChatStore.getState();
    store.startAssistantMessage('assistant-dedup');
    store.startToolBatch(1);
    store.recordToolStart('assistant-dedup', 'detail-1');
    store.recordToolStart('assistant-dedup', 'detail-1');
    store.endToolBatch();

    const message = useChatStore.getState().messages[0];
    expect(message?.executionSteps).toEqual([{ type: 'tool', callId: 'detail-1' }]);
    expect(message?.executionRounds).toEqual([{ toolCallIds: ['detail-1'] }]);
  });
});
