import { beforeEach, describe, expect, it, vi } from 'vitest';

const mocks = vi.hoisted(() => ({
  switchWorkspace: vi.fn(),
  resetSession: vi.fn(),
  clearMessages: vi.fn(),
  initConversations: vi.fn(),
  markWorkspaceChanged: vi.fn(),
  loadTree: vi.fn(),
  loadChanges: vi.fn(),
  addToast: vi.fn(),
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

vi.mock('./toastStore', () => ({
  useToastStore: {
    getState: () => ({ addToast: mocks.addToast }),
  },
}));

import { useWorkspaceStore } from './workspaceStore';

describe('workspaceStore file identity integration', () => {
  beforeEach(() => {
    vi.clearAllMocks();
    useWorkspaceStore.setState({ current: null, workspaces: [], isLoading: false });
    mocks.switchWorkspace.mockResolvedValue({
      workspace: { id: 'workspace-b', name: 'Workspace B' },
      transition: {
        status: 'committed',
        previous_workspace_id: null,
        target_workspace_id: 'workspace-b',
        target_root: '/workspace-b',
        degraded_subsystems: [],
      },
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

  it('shows a warning without rolling back a degraded target', async () => {
    mocks.switchWorkspace.mockResolvedValueOnce({
      workspace: { id: 'workspace-b', name: 'Workspace B' },
      transition: {
        status: 'degraded',
        previous_workspace_id: 'workspace-a',
        target_workspace_id: 'workspace-b',
        target_root: '/workspace-b',
        degraded_subsystems: [
          {
            subsystem: 'config_watcher',
            target_root: '/workspace-b',
            stale_roots: ['/workspace-a'],
            error: 'old directory could not be unwatched',
          },
        ],
      },
    });

    await useWorkspaceStore.getState().switchTo('workspace-b');

    expect(useWorkspaceStore.getState().current?.id).toBe('workspace-b');
    expect(mocks.addToast).toHaveBeenCalledWith(
      'warning',
      expect.stringContaining('config_watcher'),
      8000
    );
  });
});
