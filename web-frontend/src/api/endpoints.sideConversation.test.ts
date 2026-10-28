import { beforeEach, describe, expect, it, vi } from 'vitest';

const bridge = vi.hoisted(() => ({
  apiInvoke: vi.fn(),
  isTauri: vi.fn(() => true),
}));

vi.mock('../lib/tauri-bridge', () => bridge);

import { conversationApi, workspaceApi, type ConversationForkPreparation } from './endpoints';

describe('GUI-only Side Conversation IPC adapter', () => {
  beforeEach(() => {
    bridge.apiInvoke.mockReset();
    bridge.isTauri.mockReturnValue(true);
  });

  it('binds a fork target generation to the existing native workspace switch admission', async () => {
    bridge.apiInvoke.mockResolvedValue({ success: true });
    await workspaceApi.switch('target', undefined, 'exact-generation');
    expect(bridge.apiInvoke).toHaveBeenCalledExactlyOnceWith('switch_workspace', {
      id: 'target',
      connectionId: undefined,
      expectedGeneration: 'exact-generation',
    });
  });

  it('rejects generation-bound workspace switching on a transport without that contract', async () => {
    bridge.isTauri.mockReturnValue(false);
    await expect(workspaceApi.switch('target', undefined, 'exact-generation')).rejects.toThrow(
      '桌面应用'
    );
    expect(bridge.apiInvoke).not.toHaveBeenCalled();
  });

  it('routes create and retry through discriminated Tauri conversation requests', async () => {
    bridge.apiInvoke.mockResolvedValue({
      creation: { duplicate: false, entry: {} },
      first_turn: null,
      initial_prompt: 'Investigate',
      launch_error: null,
    });

    await conversationApi.createSide({
      workspace_id: 'workspace-1',
      parent_conversation_id: 'conversation-1',
      request_id: 'request-1',
      prompt: 'Investigate',
      title: null,
      model_id: null,
    });
    await conversationApi.retrySide('workspace-1', 'side-1');
    await conversationApi.markSideViewed('workspace-1', 'side-1');

    expect(bridge.apiInvoke).toHaveBeenNthCalledWith(1, 'create_conversation', {
      workspaceId: 'workspace-1',
      request: {
        kind: 'side',
        parent_conversation_id: 'conversation-1',
        request_id: 'request-1',
        prompt: 'Investigate',
        title: null,
        model_id: null,
      },
    });
    expect(bridge.apiInvoke).toHaveBeenNthCalledWith(2, 'update_conversation', {
      workspaceId: 'workspace-1',
      id: 'side-1',
      request: { kind: 'side_retry' },
    });
    expect(bridge.apiInvoke).toHaveBeenNthCalledWith(3, 'update_conversation', {
      workspaceId: 'workspace-1',
      id: 'side-1',
      request: { kind: 'side_viewed' },
    });
  });

  it('rejects outside Tauri without calling another transport', async () => {
    bridge.isTauri.mockReturnValue(false);

    await expect(
      conversationApi.createSide({
        workspace_id: 'workspace-1',
        parent_conversation_id: 'conversation-1',
        request_id: 'request-1',
        prompt: 'Investigate',
        title: null,
        model_id: null,
      })
    ).rejects.toThrow('requires the Eclip desktop GUI');
    expect(bridge.apiInvoke).not.toHaveBeenCalled();
  });

  it('binds reply Fork preparation and creation to the same source generation, prefix and destination', async () => {
    const preparation: ConversationForkPreparation = {
      request_id: 'stable-request',
      workspace_id: 'workspace-1',
      workspace_generation: 'source-generation',
      source_id: 'conversation-1',
      user_turn_index: 2,
      prefix_sha256: 'canonical-prefix',
      message_count: 8,
      worktree: {
        available: true,
        reason: null,
        repo_root: '/source',
        head: 'fixed-head',
        branch: 'eko/fork-stable-request',
        path: '/new-worktree',
        project_location: { kind: 'local', path: '/source' },
        ssh_target: null,
        source_connection_generation: null,
        draft_connection_id: null,
      },
    };
    bridge.apiInvoke.mockResolvedValueOnce(preparation).mockResolvedValueOnce({
      success: false,
      request_id: 'stable-request',
      worktree: { created: true, observed: false, path: '/new-worktree' },
    });
    const fixed = await conversationApi.prepareFork(
      { workspaceId: 'workspace-1', workspaceGeneration: 'source-generation' },
      'conversation-1',
      2,
      'stable-request'
    );
    const fork = { preparation: fixed, destination: 'new_worktree' as const };
    const receipt = await conversationApi.branch('workspace-1', 'conversation-1', 2, true, fork);
    expect(bridge.apiInvoke).toHaveBeenNthCalledWith(1, 'prepare_conversation_fork', {
      workspaceId: 'workspace-1',
      workspaceGeneration: 'source-generation',
      id: 'conversation-1',
      userTurnIndex: 2,
      requestId: 'stable-request',
    });
    expect(bridge.apiInvoke).toHaveBeenNthCalledWith(2, 'branch_conversation', {
      workspaceId: 'workspace-1',
      id: 'conversation-1',
      userTurnIndex: 2,
      includeTurn: true,
      fork,
    });
    expect(receipt).toMatchObject({
      success: false,
      worktree: { created: true, path: '/new-worktree' },
    });
  });

  it('distinguishes completed-turn Fork from edit/regenerate on the shared branch IPC', async () => {
    bridge.apiInvoke.mockResolvedValue({ success: true, id: 'branch-1' });
    await conversationApi.branch('workspace-1', 'conversation-1', 2);
    await conversationApi.branch('workspace-1', 'conversation-1', 2, true);
    expect(bridge.apiInvoke).toHaveBeenNthCalledWith(1, 'branch_conversation', {
      workspaceId: 'workspace-1',
      id: 'conversation-1',
      userTurnIndex: 2,
      includeTurn: false,
    });
    expect(bridge.apiInvoke).toHaveBeenNthCalledWith(2, 'branch_conversation', {
      workspaceId: 'workspace-1',
      id: 'conversation-1',
      userTurnIndex: 2,
      includeTurn: true,
    });
  });
});
