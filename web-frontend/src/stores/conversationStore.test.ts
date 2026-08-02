import { beforeEach, describe, expect, it, vi } from 'vitest';

const mocks = vi.hoisted(() => ({
  getConversation: vi.fn(),
  listToolExecutions: vi.fn(),
  restoreConversation: vi.fn(),
  resetSession: vi.fn(),
}));

vi.mock('../api/endpoints', () => ({
  sessionApi: { reset: mocks.resetSession },
  conversationApi: {
    list: vi.fn(),
    get: mocks.getConversation,
    save: vi.fn(),
    update: vi.fn(),
    delete: vi.fn(),
    restore: mocks.restoreConversation,
  },
  toolExecutionApi: { list: mocks.listToolExecutions },
}));

import type { ChatMessage, SavedMessage } from '../types/api';
import { useChatStore } from './chatStore';
import { chatMessagesToSaved, restoredMessageId, useConversationStore } from './conversationStore';

function deferred<T>() {
  let resolve: (value: T) => void = () => undefined;
  const promise = new Promise<T>((next) => {
    resolve = next;
  });
  return { promise, resolve };
}

beforeEach(() => {
  vi.clearAllMocks();
  mocks.listToolExecutions.mockResolvedValue([]);
  mocks.restoreConversation.mockResolvedValue(undefined);
  mocks.resetSession.mockResolvedValue(undefined);
  useChatStore.getState().clearMessages();
  useConversationStore.setState({ activeId: null, isLoading: false, conversations: [] });
});

describe('conversation message identity', () => {
  it('persists the assistant message id used by TaskRuntime root_message_id', () => {
    const messages: ChatMessage[] = [
      {
        id: 'assistant-turn-1',
        role: 'assistant',
        content: 'result',
        timestamp: 1,
      },
    ];

    expect(chatMessagesToSaved(messages)[0]?.message_id).toBe('assistant-turn-1');
  });

  it('restores the persisted id and gives old records a deterministic fallback', () => {
    const persisted = { message_id: 'assistant-turn-1', role: 'assistant', content: '' };
    const legacy: SavedMessage = { role: 'assistant', content: '' };

    expect(restoredMessageId('conversation-1', 2, persisted)).toBe('assistant-turn-1');
    expect(restoredMessageId('conversation-1', 2, legacy)).toBe('loaded-conversation-1-2');
  });

  it('clears loading state when a pending conversation load is interrupted by a new chat', async () => {
    const pendingRecord = deferred<{ messages: SavedMessage[] }>();
    mocks.getConversation.mockReturnValueOnce(pendingRecord.promise);

    const load = useConversationStore.getState().loadConversation('conversation-1');
    expect(useConversationStore.getState().isLoading).toBe(true);

    await useConversationStore.getState().startNew();
    pendingRecord.resolve({ messages: [] });
    await load;

    expect(useConversationStore.getState()).toMatchObject({ activeId: null, isLoading: false });
    expect(mocks.restoreConversation).not.toHaveBeenCalled();
  });
});
