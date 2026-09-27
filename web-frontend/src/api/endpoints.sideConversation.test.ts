import { beforeEach, describe, expect, it, vi } from 'vitest';

const bridge = vi.hoisted(() => ({
  apiInvoke: vi.fn(),
  isTauri: vi.fn(() => true),
}));

vi.mock('../lib/tauri-bridge', () => bridge);

import { conversationApi } from './endpoints';

describe('GUI-only Side Conversation IPC adapter', () => {
  beforeEach(() => {
    bridge.apiInvoke.mockReset();
    bridge.isTauri.mockReturnValue(true);
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
    ).rejects.toThrow('requires the EKO desktop GUI');
    expect(bridge.apiInvoke).not.toHaveBeenCalled();
  });
});
