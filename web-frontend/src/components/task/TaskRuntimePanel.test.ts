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

  it('projects a timed-out Subagent as failed', () => {
    expect(
      displayedTodoStatus({ task_id: 'task-1', status: 'pending' as TodoStatus }, [
        run('timed_out'),
      ])
    ).toBe('failed');
  });
});
