// @vitest-environment jsdom
import { act, cleanup, renderHook, waitFor } from '@testing-library/react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { useConversationUsage } from './useConversationUsage';
import { useConversationStore } from '../stores/conversationStore';
import { useChatStore } from '../stores/chatStore';
import { useWorkspaceStore } from '../stores/workspaceStore';
import { conversationApi } from '../api/endpoints';
import type { ConversationUsageSnapshot } from '../types/api';

vi.mock('../api/endpoints', () => ({ conversationApi: { usage: vi.fn() } }));

const snapshot = (conversation: string, sequence = 20) =>
  ({
    workspace_id: 'global',
    conversation_id: conversation,
    journal_identity: 'original-generation',
    through_sequence: sequence,
    latest_sequence: sequence,
  }) as ConversationUsageSnapshot;

describe('original journal usage read fences', () => {
  afterEach(cleanup);
  beforeEach(() => {
    vi.clearAllMocks();
    useWorkspaceStore.setState({ current: null });
    useConversationStore.setState({ activeId: 'first' });
    useChatStore.setState({ isStreaming: false });
  });
  it('does not project a late response after a conversation switch', async () => {
    let finish: ((value: ConversationUsageSnapshot) => void) | undefined;
    vi.mocked(conversationApi.usage)
      .mockImplementationOnce(
        () =>
          new Promise((resolve) => {
            finish = resolve;
          })
      )
      .mockResolvedValueOnce(snapshot('second'));
    const { result } = renderHook(() => useConversationUsage());
    act(() => useConversationStore.setState({ activeId: 'second' }));
    await waitFor(() => expect(result.current.value?.conversation_id).toBe('second'));
    await act(async () => {
      finish?.(snapshot('first', 999));
    });
    expect(result.current.value?.conversation_id).toBe('second');
  });
  it('keeps the same durable total when visible messages are cleared and reads again at stream end', async () => {
    vi.mocked(conversationApi.usage).mockResolvedValue(snapshot('first'));
    const { result } = renderHook(() => useConversationUsage());
    await waitFor(() => expect(result.current.value?.through_sequence).toBe(20));
    act(() => useChatStore.getState().clearMessages());
    expect(result.current.value?.through_sequence).toBe(20);
    act(() => useChatStore.setState({ isStreaming: true }));
    await waitFor(() => expect(conversationApi.usage).toHaveBeenCalledTimes(2));
    act(() => useChatStore.setState({ isStreaming: false }));
    await waitFor(() => expect(conversationApi.usage).toHaveBeenCalledTimes(3));
  });
  it('keeps the last confirmed value with an explicit read failure', async () => {
    vi.mocked(conversationApi.usage)
      .mockResolvedValueOnce(snapshot('first'))
      .mockRejectedValueOnce(new Error('original generation unavailable'));
    const { result } = renderHook(() => useConversationUsage());
    await waitFor(() => expect(result.current.value?.through_sequence).toBe(20));
    act(() => useChatStore.setState({ isStreaming: true }));
    await waitFor(() => expect(result.current.error).toContain('unavailable'));
    expect(result.current.value?.through_sequence).toBe(20);
  });
});
