import { beforeEach, describe, expect, it, vi } from 'vitest';

const mocks = vi.hoisted(() => ({
  getConversation: vi.fn(),
  listToolExecutions: vi.fn(),
  restoreConversation: vi.fn(),
  branchConversation: vi.fn(),
  listConversations: vi.fn(),
  resetSession: vi.fn(),
}));

vi.mock('../api/endpoints', () => ({
  sessionApi: { reset: mocks.resetSession },
  conversationApi: {
    list: mocks.listConversations,
    get: mocks.getConversation,
    save: vi.fn(),
    update: vi.fn(),
    delete: vi.fn(),
    restore: mocks.restoreConversation,
    branch: mocks.branchConversation,
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
  mocks.listConversations.mockResolvedValue([]);
  mocks.branchConversation.mockResolvedValue({
    success: true,
    id: 'branch-1',
    source_id: 'conversation-1',
    message_count: 2,
    target_content: 'canonical user prompt',
  });
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

  it('does not duplicate pasted or oversized attachment bodies in UI persistence', () => {
    const largeUrl = `data:text/plain;base64,${'A'.repeat(70 * 1024)}`;
    const messages: ChatMessage[] = [
      {
        id: 'user-turn-1',
        role: 'user',
        content: '(附件)',
        timestamp: 1,
        attachments: [
          {
            name: 'pasted-text-1.txt',
            mime_type: 'text/plain',
            url: 'data:text/plain;base64,cGFzdGU=',
            size: 5,
            source: 'paste',
          },
          {
            name: 'large.log',
            mime_type: 'text/plain',
            url: largeUrl,
            size: 70 * 1024,
            source: 'upload',
          },
          {
            name: 'small.txt',
            mime_type: 'text/plain',
            url: 'data:text/plain;base64,c21hbGw=',
            size: 5,
            source: 'upload',
          },
          {
            name: 'screenshot.png',
            mime_type: 'image/png',
            url: 'data:image/png;base64,aW1hZ2U=',
            size: 5,
            source: 'paste',
          },
        ],
      },
    ];

    const attachments = chatMessagesToSaved(messages)[0]?.attachments;
    expect(attachments?.map((attachment) => attachment.url)).toEqual([
      '',
      '',
      'data:text/plain;base64,c21hbGw=',
      'data:image/png;base64,aW1hZ2U=',
    ]);
    expect(attachments?.map((attachment) => attachment.source)).toEqual([
      'paste',
      'upload',
      'upload',
      'paste',
    ]);
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

  it('activates only the canonical branch returned by the backend', async () => {
    useConversationStore.setState({
      activeId: 'conversation-1',
      conversations: [],
      isLoading: false,
    });

    const result = await useConversationStore.getState().branchCurrent(2);

    expect(mocks.branchConversation).toHaveBeenCalledWith('conversation-1', 2);
    expect(result).toEqual({ id: 'branch-1', targetContent: 'canonical user prompt' });
    expect(useConversationStore.getState().activeId).toBe('branch-1');
  });
});
