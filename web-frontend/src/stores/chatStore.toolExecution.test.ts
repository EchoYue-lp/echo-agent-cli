import { beforeEach, describe, expect, it } from 'vitest';
import { useChatStore } from './chatStore';

describe('chat tool execution projection', () => {
  beforeEach(() => {
    useChatStore.getState().clearMessages();
    useChatStore.setState({ runStatus: 'idle' });
  });

  it('stores canonical execution order as stable execution IDs', () => {
    const store = useChatStore.getState();
    store.startAssistantMessage('assistant-test');
    store.recordToolStart('assistant-test', 'detail-a');
    store.recordToolStart('assistant-test', 'detail-b');

    const message = useChatStore.getState().messages[0];
    expect(message?.executionSteps).toEqual([
      { type: 'tool', callId: 'detail-a' },
      { type: 'tool', callId: 'detail-b' },
    ]);
  });

  it('ignores duplicate start events for one execution ID', () => {
    const store = useChatStore.getState();
    store.startAssistantMessage('assistant-dedup');
    store.recordToolStart('assistant-dedup', 'detail-1');
    store.recordToolStart('assistant-dedup', 'detail-1');

    const message = useChatStore.getState().messages[0];
    expect(message?.executionSteps).toEqual([{ type: 'tool', callId: 'detail-1' }]);
  });
});
