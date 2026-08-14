import { beforeEach, describe, expect, it, vi } from 'vitest';

const mocks = vi.hoisted(() => ({
  switchWorkspace: vi.fn(),
  resetSession: vi.fn(),
  clearMessages: vi.fn(),
  initConversations: vi.fn(),
  markWorkspaceChanged: vi.fn(),
  loadTree: vi.fn(),
  loadChanges: vi.fn(),
}));

vi.mock('../api/endpoints', () => ({
  workspaceApi: {
    switch: mocks.switchWorkspace,
  },
  sessionApi: {
    reset: mocks.resetSession,
  },
}));

vi.mock('./chatStore', () => ({
  useChatStore: {
    getState: () => ({ clearMessages: mocks.clearMessages }),
  },
}));

vi.mock('./conversationStore', () => ({
  useConversationStore: {
    setState: vi.fn(),
    getState: () => ({
      init: mocks.initConversations,
      conversations: [],
    }),
  },
}));

vi.mock('./fileStore', () => ({
  useFileStore: {
    getState: () => ({
      markWorkspaceChanged: mocks.markWorkspaceChanged,
      loadTree: mocks.loadTree,
      loadChanges: mocks.loadChanges,
    }),
  },
}));

import { useWorkspaceStore } from './workspaceStore';

describe('workspaceStore file identity integration', () => {
  beforeEach(() => {
    vi.clearAllMocks();
    useWorkspaceStore.setState({ current: null, workspaces: [], isLoading: false });
    mocks.switchWorkspace.mockResolvedValue({
      workspace: { id: 'workspace-b', name: 'Workspace B' },
    });
    mocks.resetSession.mockResolvedValue(undefined);
    mocks.initConversations.mockResolvedValue(undefined);
    mocks.loadTree.mockResolvedValue(undefined);
    mocks.loadChanges.mockResolvedValue(undefined);
  });

  it('invalidates open drafts before loading the selected workspace files', async () => {
    await useWorkspaceStore.getState().switchTo('workspace-b');

    expect(mocks.markWorkspaceChanged).toHaveBeenCalledOnce();
    expect(mocks.loadTree).toHaveBeenCalledWith(4);
    expect(mocks.loadChanges).toHaveBeenCalledOnce();
    expect(useWorkspaceStore.getState().current?.id).toBe('workspace-b');
  });
});
