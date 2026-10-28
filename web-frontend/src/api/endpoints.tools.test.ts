import { beforeEach, describe, expect, it, vi } from 'vitest';

const bridge = vi.hoisted(() => ({ apiInvoke: vi.fn(), isTauri: vi.fn(() => true) }));
vi.mock('../lib/tauri-bridge', () => bridge);
import { toolsApi } from './endpoints';

describe('工具设置的原工作空间身份', () => {
  beforeEach(() => {
    bridge.apiInvoke.mockReset();
  });
  it('carries the same workspace generation through list and each original mutation IPC', async () => {
    const scope = { workspaceId: 'project-A', workspaceGeneration: 'created-A:revision-4' };
    bridge.apiInvoke.mockResolvedValue({ success: true });
    await toolsApi.list(scope);
    await toolsApi.enable('shell', scope);
    await toolsApi.disable('shell', scope);
    expect(bridge.apiInvoke).toHaveBeenNthCalledWith(1, 'list_tools', scope);
    expect(bridge.apiInvoke).toHaveBeenNthCalledWith(2, 'enable_tool', { name: 'shell', ...scope });
    expect(bridge.apiInvoke).toHaveBeenNthCalledWith(3, 'disable_tool', {
      name: 'shell',
      ...scope,
    });
  });
});
