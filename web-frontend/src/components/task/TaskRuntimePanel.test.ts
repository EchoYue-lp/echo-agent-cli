import { describe, expect, it } from 'vitest';
import type { TodoStatus } from '../../generated';
import type { SubagentRunState } from '../../stores/subagentRunStore';
import {
  displayedTodoStatus,
  todoShouldSpin,
  todoStatusDescription,
  traceRunsForTaskRun,
} from './TaskRuntimePanel';

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
  it('does not animate a stale running row after the TaskRun is terminal', () => {
    expect(todoShouldSpin('running', 'running', 'completed')).toBe(false);
    expect(todoShouldSpin('running', 'running', 'paused')).toBe(false);
    expect(todoShouldSpin('running', 'running', 'pending')).toBe(false);
    expect(todoShouldSpin('running', 'running', 'running')).toBe(true);
  });

  it('scopes todo execution state to the active TaskRun', () => {
    const oldRun = { ...run('completed'), runId: 'run-old', startedAt: 20 };
    const activeRun = { ...run('running'), runId: 'run-active', startedAt: 10 };

    expect(traceRunsForTaskRun('run-active', [oldRun, activeRun])).toEqual([activeRun]);
  });

  it('projects a running Subagent onto a pending todo', () => {
    expect(
      displayedTodoStatus({ task_id: 'task-1', status: 'pending' as TodoStatus }, [run('running')])
    ).toBe('running');
  });

  it('projects a cancelled Subagent onto a pending todo as cancelled', () => {
    expect(
      displayedTodoStatus({ task_id: 'task-1', status: 'pending' as TodoStatus }, [
        run('cancelled'),
      ])
    ).toBe('cancelled');
  });

  it('projects a completed inline Subagent onto a pending todo', () => {
    expect(
      displayedTodoStatus({ task_id: 'task-1', status: 'pending' as TodoStatus }, [
        run('completed'),
      ])
    ).toBe('completed');
  });

  it('projects a failed inline Subagent without changing its type', () => {
    expect(
      displayedTodoStatus({ task_id: 'task-1', status: 'pending' as TodoStatus }, [run('failed')])
    ).toBe('failed');
  });

  it('projects a timed-out inline Subagent without changing its type', () => {
    expect(
      displayedTodoStatus({ task_id: 'task-1', status: 'pending' as TodoStatus }, [
        run('timed_out'),
      ])
    ).toBe('timed_out');
  });

  it('does NOT mark an executor-owned running todo completed before review finishes', () => {
    expect(
      displayedTodoStatus({ task_id: 'task-1', status: 'running' as TodoStatus }, [
        run('completed'),
      ])
    ).toBe('running');
  });

  it('describes a completed execution with a running task as review or integration', () => {
    expect(
      todoStatusDescription({ task_id: 'task-1', status: 'running' as TodoStatus }, [
        run('completed'),
      ])
    ).toBe('执行已完成 · 评审/收尾中');
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

  it('distinguishes completed Subagent execution from a blocked review', () => {
    expect(
      todoStatusDescription({ task_id: 'task-1', status: 'blocked' as TodoStatus }, [
        run('completed'),
      ])
    ).toBe('执行已完成 · 评审未通过');
  });

  it('distinguishes completed Subagent execution from a later plan skip', () => {
    expect(
      todoStatusDescription({ task_id: 'task-1', status: 'skipped' as TodoStatus }, [
        run('completed'),
      ])
    ).toBe('执行已完成 · 任务已跳过');
  });

  it('does not count a completed Subagent execution as task acceptance', () => {
    expect(
      todoStatusDescription({ task_id: 'task-1', status: 'pending' as TodoStatus }, [
        run('completed'),
      ])
    ).toBe('执行已完成 · 任务待处理');
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
