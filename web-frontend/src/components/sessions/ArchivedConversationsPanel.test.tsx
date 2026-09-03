// @vitest-environment jsdom
import { cleanup, fireEvent, render, waitFor } from '@testing-library/react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

const mocks = vi.hoisted(() => ({
  list: vi.fn(),
  setArchived: vi.fn(),
  delete: vi.fn(),
}));

vi.mock('../../api/endpoints', () => ({
  sessionApi: {},
  conversationApi: {
    list: mocks.list,
    setArchived: mocks.setArchived,
    delete: mocks.delete,
  },
  toolExecutionApi: {},
}));

import { useConversationStore } from '../../stores/conversationStore';
import { ArchivedConversationsPanel } from './ArchivedConversationsPanel';

describe('ArchivedConversationsPanel', () => {
  afterEach(() => {
    cleanup();
    vi.unstubAllGlobals();
  });

  beforeEach(() => {
    vi.clearAllMocks();
    mocks.list.mockResolvedValue([
      {
        id: 1,
        conversation_id: 'conversation-1',
        title: 'Archived task',
        message_count: 3,
        created_at: '2026-09-03T00:00:00Z',
        updated_at: '2026-09-03T01:00:00Z',
        archived: true,
      },
    ]);
    mocks.setArchived.mockResolvedValue({
      success: true,
      conversation_id: 'conversation-1',
      archived: false,
    });
    mocks.delete.mockResolvedValue({ cleanup_pending: false });
    useConversationStore.setState({
      workspaceId: 'workspace-1',
      conversations: [],
      archivedConversationIds: [],
      activeId: null,
      isLoading: false,
    });
  });

  it('lists archived conversations and exposes restore and permanent delete actions', async () => {
    const { findByText, getByRole } = render(<ArchivedConversationsPanel />);

    expect(await findByText('Archived task')).toBeTruthy();
    expect(getByRole('button', { name: '恢复' })).toBeTruthy();
    expect(getByRole('button', { name: '永久删除' })).toBeTruthy();

    fireEvent.click(getByRole('button', { name: '恢复' }));
    await waitFor(() =>
      expect(mocks.setArchived).toHaveBeenCalledWith('workspace-1', 'conversation-1', false)
    );
  });

  it('requires confirmation before permanently deleting a conversation', async () => {
    vi.stubGlobal(
      'confirm',
      vi.fn(() => false)
    );
    const { findByText, getByRole } = render(<ArchivedConversationsPanel />);
    await findByText('Archived task');

    fireEvent.click(getByRole('button', { name: '永久删除' }));
    expect(mocks.delete).not.toHaveBeenCalled();
  });
});
