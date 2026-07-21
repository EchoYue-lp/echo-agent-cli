import { describe, expect, it } from 'vitest';
import type { TodoStatus } from '../../generated';
import type { SubagentRunState } from '../../stores/subagentRunStore';
import { displayedTodoStatus } from './TaskRuntimePanel';

function run(status: SubagentRunState['status']): SubagentRunState {
  return {
    subagentRunId: 'subagent-1',
    runId: 'run-1',
    taskId: 'task-1',
    agent: 'explorer',
    status,
    startedAt: 1,
    events: [],
  };
}

describe('displayedTodoStatus', () => {
  it('projects a running Subagent onto a pending todo', () => {
    expect(
      displayedTodoStatus({ task_id: 'task-1', status: 'pending' as TodoStatus }, [run('running')])
    ).toBe('running');
  });

  it('projects a cancelled Subagent onto a pending todo as skipped', () => {
    expect(
      displayedTodoStatus({ task_id: 'task-1', status: 'pending' as TodoStatus }, [
        run('cancelled'),
      ])
    ).toBe('skipped');
  });

  it('does NOT overwrite a persisted Blocked status with Subagent completed', () => {
    // M7: acceptance failure marks the task Blocked even though the
    // Subagent trace says completed. Overwriting Blocked → completed hid
    // the retry button. Persisted terminal statuses are authoritative.
    expect(
      displayedTodoStatus({ task_id: 'task-1', status: 'blocked' as TodoStatus }, [
        run('completed'),
      ])
    ).toBe('blocked');
  });

  it('does NOT overwrite a persisted Failed status with Subagent completed', () => {
    expect(
      displayedTodoStatus({ task_id: 'task-1', status: 'failed' as TodoStatus }, [run('completed')])
    ).toBe('failed');
  });

  it('does NOT overwrite a persisted Completed status with a later Subagent failure', () => {
    expect(
      displayedTodoStatus({ task_id: 'task-1', status: 'completed' as TodoStatus }, [run('failed')])
    ).toBe('completed');
  });
});
