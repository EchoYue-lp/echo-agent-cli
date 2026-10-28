import { beforeEach, describe, expect, it, vi } from 'vitest';

const bridge = vi.hoisted(() => ({ apiInvoke: vi.fn(), isTauri: vi.fn(() => true) }));
const http = vi.hoisted(() => ({ get: vi.fn(), post: vi.fn(), del: vi.fn() }));
vi.mock('../lib/tauri-bridge', () => bridge);
vi.mock('./client', () => http);
import { taskRuntimeApi } from './endpoints';

beforeEach(() => {
  vi.resetAllMocks();
  bridge.isTauri.mockReturnValue(true);
});

describe('formal task history IPC contracts', () => {
  it('preserves the captured workspace generation, status filter and opaque run continuation', async () => {
    const scope = { workspaceId: 'workspace-a', workspaceGeneration: 'generation-a' };
    const page = { items: [], next_continuation: 'next 中文 🧭' };
    bridge.apiInvoke.mockResolvedValueOnce(page);
    await expect(taskRuntimeApi.listRuns(scope, ['paused'], 'previous 中文 🧭')).resolves.toBe(
      page
    );
    expect(bridge.apiInvoke).toHaveBeenCalledExactlyOnceWith('list_task_runs', {
      ...scope,
      statuses: ['paused'],
      continuation: 'previous 中文 🧭',
      limit: 20,
    });
    expect(http.get).not.toHaveBeenCalled();
  });

  it('reads the selected run and lossless event cursor without querying conversation latest', async () => {
    const sequence = '9007199254740993';
    bridge.apiInvoke.mockResolvedValueOnce(null).mockResolvedValueOnce({
      events: [],
      next_after_sequence: sequence,
      has_more: false,
    });
    await taskRuntimeApi.getRun('workspace-a', 'history-run', 'generation-a');
    await taskRuntimeApi.listEvents('workspace-a', 'history-run', sequence, 'generation-a');
    expect(bridge.apiInvoke).toHaveBeenNthCalledWith(1, 'get_task_run', {
      workspaceId: 'workspace-a',
      runId: 'history-run',
      workspaceGeneration: 'generation-a',
    });
    expect(bridge.apiInvoke).toHaveBeenNthCalledWith(2, 'list_task_events', {
      workspaceId: 'workspace-a',
      runId: 'history-run',
      workspaceGeneration: 'generation-a',
      sinceSeq: sequence,
      limit: 256,
    });
    expect(bridge.apiInvoke).toHaveBeenCalledTimes(2);
  });

  it('preserves native recovery and generation errors instead of falling back to an empty HTTP list', async () => {
    const failure = new Error('工作空间代际已改变，原journal无法恢复');
    bridge.apiInvoke.mockRejectedValueOnce(failure);
    await expect(
      taskRuntimeApi.listRuns({ workspaceId: 'workspace-a', workspaceGeneration: 'generation-a' })
    ).rejects.toBe(failure);
    expect(http.get).not.toHaveBeenCalled();
  });
});
