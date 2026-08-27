import { describe, expect, it } from 'vitest';
import type { TodoStatus } from '../../generated';
import type { SubagentRunState } from '../../stores/subagentRunStore';
import {
  continuationBudgetLabels,
  formatDurationSeconds,
  parseContinuationBudgetInput,
  todoShouldSpin,
  todoStatusDescription,
  traceRunsForTaskRun,
} from './TaskRuntimePanel';

describe('continuation budget presentation', () => {
  it('parses positive budgets and treats blank input as unbounded', () => {
    expect(parseContinuationBudgetInput(' 120000 ', 'Token 预算')).toBe(120_000);
    expect(parseContinuationBudgetInput('', '时间预算')).toBeNull();
    expect(() => parseContinuationBudgetInput('0', '时间预算')).toThrow(
      '时间预算必须是正整数，留空表示不限'
    );
    expect(() => parseContinuationBudgetInput('1.5', 'Token 预算')).toThrow(
      'Token 预算必须是正整数，留空表示不限'
    );
  });

  it('shows used, budget, and remaining values for both limits', () => {
    expect(
      continuationBudgetLabels({
        token_budget: 100_000,
        tokens_used: 12_000,
        time_budget_seconds: 7_200,
        time_used_seconds: 90,
      })
    ).toEqual({
      tokens: 'Token 已用 12,000 · 预算 100,000 · 剩余 88,000',
      time: '时间已用 1 分钟 30 秒 · 预算 2 小时 · 剩余 1 小时 58 分钟 30 秒',
    });
  });

  it('clamps exhausted budgets at zero remaining', () => {
    expect(
      continuationBudgetLabels({
        token_budget: 1_000,
        tokens_used: 1_200,
        time_budget_seconds: 60,
        time_used_seconds: 90,
      })
    ).toEqual({
      tokens: 'Token 已用 1,200 · 预算 1,000 · 剩余 0',
      time: '时间已用 1 分钟 30 秒 · 预算 1 分钟 · 剩余 0 秒',
    });
  });

  it('makes unlimited budgets explicit', () => {
    expect(
      continuationBudgetLabels({
        token_budget: null,
        tokens_used: 42,
        time_budget_seconds: null,
        time_used_seconds: 3_661,
      })
    ).toEqual({
      tokens: 'Token 已用 42 · 预算不限 · 剩余不限',
      time: '时间已用 1 小时 1 分钟 1 秒 · 预算不限 · 剩余不限',
    });
    expect(formatDurationSeconds(Number.NaN)).toBe('0 秒');
  });
});

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

describe('Todo projection rendering', () => {
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

  it('describes a completed execution with a running task as review or integration', () => {
    expect(
      todoStatusDescription({ task_id: 'task-1', status: 'running' as TodoStatus }, [
        run('completed'),
      ])
    ).toBe('执行已完成 · 评审/收尾中');
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
});
