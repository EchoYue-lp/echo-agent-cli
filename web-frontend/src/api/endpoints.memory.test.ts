import { beforeEach, describe, expect, it, vi } from 'vitest';

const bridge = vi.hoisted(() => ({
  apiInvoke: vi.fn(),
  isTauri: vi.fn(() => true),
}));

vi.mock('../lib/tauri-bridge', () => bridge);

import { autoMemoryApi, memoryApi, worktreeApi } from './endpoints';

describe('workspace-scoped memory API', () => {
  beforeEach(() => {
    bridge.apiInvoke.mockReset();
    bridge.apiInvoke.mockResolvedValue([]);
    bridge.isTauri.mockReturnValue(true);
  });

  it('passes the exact workspace identity to every memory IPC command', async () => {
    await memoryApi.list('workspace-a', 'agent/memories');
    await memoryApi.search('workspace-a', 'query', 'agent/memories');
    await memoryApi.add('workspace-a', {
      namespace: 'agent/memories',
      key: 'fact',
      value: 'workspace A',
    });
    await memoryApi.delete('workspace-a', {
      namespace: 'agent/memories',
      key: 'fact',
    });
    await memoryApi.reflect('workspace-a', 'conversation-a');

    expect(bridge.apiInvoke).toHaveBeenNthCalledWith(1, 'list_memory', {
      workspaceId: 'workspace-a',
      namespace: 'agent/memories',
    });
    expect(bridge.apiInvoke).toHaveBeenNthCalledWith(2, 'search_memory', {
      workspaceId: 'workspace-a',
      query: 'query',
      namespace: 'agent/memories',
    });
    expect(bridge.apiInvoke).toHaveBeenNthCalledWith(3, 'add_memory', {
      workspaceId: 'workspace-a',
      namespace: 'agent/memories',
      key: 'fact',
      value: 'workspace A',
    });
    expect(bridge.apiInvoke).toHaveBeenNthCalledWith(4, 'delete_memory', {
      workspaceId: 'workspace-a',
      namespace: 'agent/memories',
      key: 'fact',
    });
    expect(bridge.apiInvoke).toHaveBeenNthCalledWith(5, 'reflect_session', {
      workspaceId: 'workspace-a',
      conversationId: 'conversation-a',
    });
  });

  it('uses global only when the caller explicitly supplies global', async () => {
    await memoryApi.list('global');

    expect(bridge.apiInvoke).toHaveBeenCalledWith('list_memory', {
      workspaceId: 'global',
      namespace: undefined,
    });
  });

  it('passes the exact workspace to every auto-memory command', async () => {
    await autoMemoryApi.status('workspace-b');
    await autoMemoryApi.toggle('workspace-b', true);
    await autoMemoryApi.preview('workspace-b');
    await autoMemoryApi.extract('workspace-b');

    expect(bridge.apiInvoke).toHaveBeenNthCalledWith(1, 'get_auto_memory_status', {
      workspaceId: 'workspace-b',
    });
    expect(bridge.apiInvoke).toHaveBeenNthCalledWith(2, 'toggle_auto_memory', {
      workspaceId: 'workspace-b',
      enabled: true,
    });
    expect(bridge.apiInvoke).toHaveBeenNthCalledWith(3, 'get_auto_memory_observations', {
      workspaceId: 'workspace-b',
    });
    expect(bridge.apiInvoke).toHaveBeenNthCalledWith(4, 'extract_auto_memory', {
      workspaceId: 'workspace-b',
    });
  });

  it('keeps primary and unattended worktree IPC explicitly scoped', async () => {
    await worktreeApi.list('workspace-c');
    await worktreeApi.create('workspace-c', { branch: 'feature/c' });
    await worktreeApi.remove('workspace-c', '/tmp/worktree-c');
    await worktreeApi.listUnattended('workspace-c');
    await worktreeApi.mergeUnattended('workspace-c', 'run-c');
    await worktreeApi.discardUnattended('workspace-c', 'run-c');
    await worktreeApi.cleanupUnattended('workspace-c');

    expect(bridge.apiInvoke).toHaveBeenNthCalledWith(1, 'list_worktrees', {
      workspaceId: 'workspace-c',
    });
    expect(bridge.apiInvoke).toHaveBeenNthCalledWith(2, 'create_worktree', {
      workspaceId: 'workspace-c',
      branch: 'feature/c',
      base: undefined,
      path: undefined,
    });
    expect(bridge.apiInvoke).toHaveBeenNthCalledWith(3, 'remove_worktree', {
      workspaceId: 'workspace-c',
      path: '/tmp/worktree-c',
    });
    expect(bridge.apiInvoke).toHaveBeenNthCalledWith(4, 'list_unattended_worktrees', {
      workspaceId: 'workspace-c',
    });
    expect(bridge.apiInvoke).toHaveBeenNthCalledWith(5, 'merge_unattended_worktree', {
      workspaceId: 'workspace-c',
      runId: 'run-c',
    });
    expect(bridge.apiInvoke).toHaveBeenNthCalledWith(6, 'discard_unattended_worktree', {
      workspaceId: 'workspace-c',
      runId: 'run-c',
    });
    expect(bridge.apiInvoke).toHaveBeenNthCalledWith(7, 'cleanup_unattended_worktrees', {
      workspaceId: 'workspace-c',
    });
  });
});
