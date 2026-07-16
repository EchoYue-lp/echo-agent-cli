import { beforeEach, describe, expect, it, vi } from 'vitest';

const mocks = vi.hoisted(() => ({
  pauseRun: vi.fn(),
  resumeRun: vi.fn(),
  resolveRecoveryTask: vi.fn(),
}));

vi.mock('../api/endpoints', () => ({
  taskRuntimeApi: mocks,
}));

import type { TaskRun } from '../generated';
import { useTaskRuntimeStore } from './taskRuntimeStore';

describe('taskRuntimeStore recovery controls', () => {
  const refresh = vi.fn().mockResolvedValue(undefined);

  beforeEach(() => {
    vi.clearAllMocks();
    mocks.pauseRun.mockResolvedValue({ success: true, run_id: 'run-1' });
    mocks.resumeRun.mockResolvedValue({ kind: 'resumed', run_id: 'run-1' });
    mocks.resolveRecoveryTask.mockResolvedValue(undefined);
    useTaskRuntimeStore.getState().reset();
    useTaskRuntimeStore.setState({
      activeRun: { run_id: 'run-1', status: 'paused' } as TaskRun,
      error: null,
      refresh,
    });
  });

  it('pauses through the shared runtime API and refreshes the canonical run', async () => {
    await useTaskRuntimeStore.getState().pause('run-1');

    expect(mocks.pauseRun).toHaveBeenCalledWith('run-1');
    expect(refresh).toHaveBeenCalledWith('run-1');
  });

  it('surfaces a recovery barrier when resume is rejected', async () => {
    mocks.resumeRun.mockRejectedValue(new Error('unresolved recovery barriers'));

    await useTaskRuntimeStore.getState().resumeTaskRun();

    expect(useTaskRuntimeStore.getState().error).toBe('unresolved recovery barriers');
    expect(refresh).not.toHaveBeenCalled();
  });

  it('records retry or skip decisions before refreshing', async () => {
    await useTaskRuntimeStore.getState().resolveRecoveryTask('task-1', 'retry');

    expect(mocks.resolveRecoveryTask).toHaveBeenCalledWith('run-1', 'task-1', 'retry');
    expect(refresh).toHaveBeenCalledWith('run-1');
  });
});
