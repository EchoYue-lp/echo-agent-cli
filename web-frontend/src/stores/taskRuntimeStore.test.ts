import { beforeEach, describe, expect, it, vi } from 'vitest';

const mocks = vi.hoisted(() => ({
  pauseRun: vi.fn(),
  resumeRun: vi.fn(),
  resolveRecoveryTask: vi.fn(),
  patchPlan: vi.fn(),
}));

vi.mock('../api/endpoints', () => ({
  taskRuntimeApi: mocks,
}));

import type { TaskPlan, TaskRun } from '../generated';
import { useTaskRuntimeStore } from './taskRuntimeStore';

describe('taskRuntimeStore recovery controls', () => {
  const refresh = vi.fn().mockResolvedValue(undefined);

  beforeEach(() => {
    vi.clearAllMocks();
    mocks.pauseRun.mockResolvedValue({ success: true, run_id: 'run-1' });
    mocks.resumeRun.mockResolvedValue({ kind: 'resumed', run_id: 'run-1' });
    mocks.resolveRecoveryTask.mockResolvedValue(undefined);
    mocks.patchPlan.mockResolvedValue({ run_id: 'run-1', revision: 4 });
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

  it('updates a task through one revisioned plan patch', async () => {
    useTaskRuntimeStore.setState({
      plan: { run_id: 'run-1', revision: 3 } as TaskPlan,
    });

    await useTaskRuntimeStore.getState().updateTask('task-1', { title: 'Refined title' });

    expect(mocks.patchPlan).toHaveBeenCalledWith('run-1', {
      base_revision: 3,
      reason: '更新任务：task-1',
      operations: [
        {
          op: 'update',
          task_id: 'task-1',
          patch: {
            title: 'Refined title',
            description: null,
            kind: null,
            agent_role: null,
            depends_on: null,
            files: null,
            allowed_tools: null,
            required_artifacts: null,
            execution_checks: null,
            acceptance_criteria: null,
            max_retries: null,
          },
        },
      ],
    });
    expect(refresh).toHaveBeenCalledWith('run-1');
  });
});
