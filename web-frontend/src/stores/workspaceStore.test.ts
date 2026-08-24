import { beforeEach, describe, expect, it, vi } from 'vitest';

const mocks = vi.hoisted(() => ({
  switchWorkspace: vi.fn(),
  createAndSwitchWorkspace: vi.fn(),
  listWorkspaces: vi.fn(),
  currentWorkspace: vi.fn(),
  deleteWorkspace: vi.fn(),
  resetSession: vi.fn(),
  clearMessages: vi.fn(),
  initConversations: vi.fn(),
  detachConversations: vi.fn(),
  bindScope: vi.fn(),
  loadTree: vi.fn(),
  loadChanges: vi.fn(),
  addToast: vi.fn(),
  clearBrowserWorkspace: vi.fn(),
}));

vi.mock('../api/endpoints', () => ({
  workspaceApi: {
    switch: mocks.switchWorkspace,
    createAndSwitch: mocks.createAndSwitchWorkspace,
    list: mocks.listWorkspaces,
    current: mocks.currentWorkspace,
    delete: mocks.deleteWorkspace,
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
      detachForWorkspace: mocks.detachConversations,
      conversations: [],
    }),
  },
}));

vi.mock('./fileStore', () => ({
  useFileStore: {
    getState: () => ({
      bindScope: mocks.bindScope,
      loadTree: mocks.loadTree,
      loadChanges: mocks.loadChanges,
    }),
  },
}));

vi.mock('./browserStore', () => ({
  useBrowserStore: {
    getState: () => ({ clearWorkspace: mocks.clearBrowserWorkspace }),
  },
}));

vi.mock('./toastStore', () => ({
  useToastStore: {
    getState: () => ({ addToast: mocks.addToast }),
  },
}));

import { useWorkspaceStore } from './workspaceStore';

function workspace(id: string, name: string, generation: string) {
  return { id, name, product_data_generation: generation };
}

function deferred<T>() {
  let resolve: (value: T) => void = () => undefined;
  const promise = new Promise<T>((next) => {
    resolve = next;
  });
  return { promise, resolve };
}

describe('workspaceStore file identity integration', () => {
  beforeEach(() => {
    vi.clearAllMocks();
    useWorkspaceStore.setState({ current: null, workspaces: [], isLoading: false });
    mocks.switchWorkspace.mockResolvedValue({
      workspace: workspace('workspace-b', 'Workspace B', 'generation-b'),
      transition: {
        status: 'committed',
        previous_workspace_id: null,
        target_workspace_id: 'workspace-b',
        target_root: '/workspace-b',
        degraded_subsystems: [],
      },
    });
    mocks.createAndSwitchWorkspace.mockResolvedValue({
      success: true,
      created: true,
      switched: true,
      workspace: workspace('workspace-b', 'Workspace B', 'generation-b'),
      transition: {
        status: 'committed',
        previous_workspace_id: null,
        target_workspace_id: 'workspace-b',
        target_root: '/workspace-b',
        degraded_subsystems: [],
      },
    });
    mocks.listWorkspaces.mockResolvedValue({
      workspaces: [workspace('workspace-b', 'Workspace B', 'generation-b')],
      count: 1,
    });
    mocks.currentWorkspace.mockResolvedValue({
      workspace: workspace('workspace-b', 'Workspace B', 'generation-b'),
      active: true,
    });
    mocks.resetSession.mockResolvedValue(undefined);
    mocks.deleteWorkspace.mockResolvedValue({ success: true });
    mocks.initConversations.mockResolvedValue(undefined);
    mocks.loadTree.mockResolvedValue(undefined);
    mocks.loadChanges.mockResolvedValue(undefined);
  });

  it('invalidates open drafts before loading the selected workspace files', async () => {
    await useWorkspaceStore.getState().switchTo('workspace-b');

    expect(mocks.bindScope).toHaveBeenCalledWith({
      workspaceId: 'workspace-b',
      workspaceGeneration: 'generation-b',
    });
    expect(mocks.loadTree).toHaveBeenCalledWith(4);
    expect(mocks.loadChanges).toHaveBeenCalledOnce();
    expect(useWorkspaceStore.getState().current?.id).toBe('workspace-b');
  });

  it('shows a warning without rolling back a degraded target', async () => {
    mocks.switchWorkspace.mockResolvedValueOnce({
      workspace: workspace('workspace-b', 'Workspace B', 'generation-b'),
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

  it('creates and switches through one backend transition', async () => {
    await useWorkspaceStore.getState().createAndSwitch('Workspace B', 'code', '/workspace-b');

    expect(mocks.createAndSwitchWorkspace).toHaveBeenCalledWith(
      'Workspace B',
      'code',
      '/workspace-b'
    );
    expect(mocks.switchWorkspace).not.toHaveBeenCalled();
    expect(mocks.bindScope).toHaveBeenCalledWith({
      workspaceId: 'workspace-b',
      workspaceGeneration: 'generation-b',
    });
    expect(useWorkspaceStore.getState().current?.id).toBe('workspace-b');
  });

  it('does not issue a second switch when backend switching fails', async () => {
    mocks.createAndSwitchWorkspace.mockResolvedValueOnce({
      success: false,
      created: true,
      switched: false,
      workspace: workspace('workspace-b', 'Workspace B', 'generation-b'),
      error: 'runtime transition failed',
    });

    await expect(
      useWorkspaceStore.getState().createAndSwitch('Workspace B', 'code', '/workspace-b')
    ).rejects.toThrow('工作区已创建，但进入失败：runtime transition failed');
    expect(mocks.switchWorkspace).not.toHaveBeenCalled();
  });

  it('ignores an older workspace transition that resolves after the newest focus', async () => {
    const first = deferred<any>();
    const second = deferred<any>();
    mocks.switchWorkspace.mockReturnValueOnce(first.promise).mockReturnValueOnce(second.promise);

    const switchA = useWorkspaceStore.getState().switchTo('workspace-a');
    const switchB = useWorkspaceStore.getState().switchTo('workspace-b');
    second.resolve({
      workspace: workspace('workspace-b', 'Workspace B', 'generation-b'),
      transition: {
        status: 'committed',
        previous_workspace_id: 'workspace-a',
        target_workspace_id: 'workspace-b',
        target_root: '/workspace-b',
        degraded_subsystems: [],
      },
    });
    await switchB;
    first.resolve({
      workspace: workspace('workspace-a', 'Workspace A', 'generation-a'),
      transition: {
        status: 'committed',
        previous_workspace_id: null,
        target_workspace_id: 'workspace-a',
        target_root: '/workspace-a',
        degraded_subsystems: [],
      },
    });
    await switchA;

    expect(useWorkspaceStore.getState().current?.id).toBe('workspace-b');
    expect(mocks.initConversations).toHaveBeenCalledTimes(1);
    expect(mocks.initConversations).toHaveBeenCalledWith('workspace-b');
  });

  it('deleting the focused workspace clears projections and rebinds global conversations', async () => {
    useWorkspaceStore.setState({
      current: { id: 'workspace-b', name: 'Workspace B' } as any,
      workspaces: [{ id: 'workspace-b', name: 'Workspace B' } as any],
      isLoading: false,
    });
    mocks.currentWorkspace.mockResolvedValueOnce({ workspace: null, active: false });
    mocks.listWorkspaces.mockResolvedValueOnce({ workspaces: [], count: 0 });

    await useWorkspaceStore.getState().delete('workspace-b');

    expect(mocks.deleteWorkspace).toHaveBeenCalledWith('workspace-b');
    expect(mocks.clearBrowserWorkspace).toHaveBeenCalledWith('workspace-b');
    expect(mocks.detachConversations).toHaveBeenCalledWith('workspace-b');
    expect(mocks.initConversations).toHaveBeenCalledWith('global');
    expect(useWorkspaceStore.getState().current).toBeNull();
  });
});
