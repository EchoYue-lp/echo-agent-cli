// @vitest-environment jsdom
import { act, cleanup, fireEvent, render, screen, waitFor, within } from '@testing-library/react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { TaskRuntimePanel } from './TaskRuntimePanel';
import { taskTimestamp } from './TaskRuntimeList';
import { useTaskRuntimeStore } from '../../stores/taskRuntimeStore';
import { useSubagentRunStore } from '../../stores/subagentRunStore';
import type { PlanRevision, TaskRun, TaskSpec, TodoItem } from '../../generated';

const originalActions = useTaskRuntimeStore.getState();
const run: TaskRun = {
  run_id: 'run-1',
  workspace_id: 'workspace-1',
  conversation_id: 'conversation-1',
  root_message_id: 'message-1',
  domain_profile: 'general',
  status: 'paused',
  goal: '整理项目的任务与交付证据 🧭',
  goal_revision: 1,
  goal_sha256: 'goal-sha',
  plan_id: 'plan-1',
  route: 'complex',
  attended_mode: 'attended',
  created_at: '2026-10-25T01:00:00Z',
  updated_at: '2026-10-25T02:00:00Z',
};

function task(index: number): TaskSpec {
  return {
    id: `task-${index}`,
    title: `任务 ${index} 中文 🧭`,
    description: '复用既有任务事实与执行记录。',
    kind: 'implementation',
    agent_role: 'general-purpose',
    domain_profile: 'general',
    depends_on: index > 1 ? ['task-1'] : [],
    parallel_group: null,
    execution_target: null,
    files: ['src/中文文件.ts'],
    allowed_tools: [],
    required_artifacts: [],
    execution_checks: [],
    acceptance_criteria: ['保留原任务身份与真实状态'],
    max_retries: 3,
    sort_order: index * 10,
  };
}

function seed(count = 3) {
  const tasks = Array.from({ length: count }, (_, index) => task(index + 1));
  const plan: PlanRevision = {
    plan_id: 'plan-1',
    run_id: run.run_id,
    revision: 2,
    domain_profile: 'general',
    goal_revision: 1,
    goal_sha256: run.goal_sha256,
    assumptions: [],
    risks: [],
    execution_mode: 'parallel',
    tasks,
  };
  const todos: TodoItem[] = tasks.map((item) => ({
    id: `todo-${item.id}`,
    run_id: run.run_id,
    task_id: item.id,
    title: item.title,
    status: 'pending',
    retry_count: 0,
    max_retries: 3,
    owner_agent: null,
    started_at: null,
    completed_at: null,
    summary: null,
  }));
  useTaskRuntimeStore.setState({
    activeRun: { ...run },
    plan,
    todos,
    artifacts: [],
    recoveryBlockers: [],
    continuation: null,
    backgroundCells: [],
    completionGate: null,
    error: null,
    events: [],
    ...Object.fromEntries(
      [
        'updateTask',
        'skipTask',
        'insertTask',
        'retryBlockedTask',
        'pause',
        'cancel',
        'resumeTaskRun',
        'refresh',
      ].map((key) => [key, originalActions[key as keyof typeof originalActions]])
    ),
  });
  useSubagentRunStore.setState({ runs: {} });
}

beforeEach(() => seed());
afterEach(() => {
  cleanup();
  vi.restoreAllMocks();
});

describe('task runtime list and details', () => {
  it('bounds visible cards and keeps search and paging on the existing projection', () => {
    seed(25);
    render(<TaskRuntimePanel />);
    expect(screen.getAllByRole('button', { name: /^查看任务：/ })).toHaveLength(20);
    fireEvent.click(screen.getByRole('button', { name: '下一页任务' }));
    expect(screen.getAllByRole('button', { name: /^查看任务：/ })).toHaveLength(5);
    fireEvent.change(screen.getByRole('textbox', { name: '搜索计划任务' }), {
      target: { value: '25 中文 🧭' },
    });
    expect(screen.getAllByRole('button', { name: /^查看任务：/ })).toHaveLength(1);
    expect(screen.getByRole('button', { name: '查看任务：任务 25 中文 🧭' })).toBeTruthy();
    fireEvent.click(screen.getByRole('button', { name: '需要关注' }));
    expect(screen.getByText('没有符合条件的任务')).toBeTruthy();
    fireEvent.click(screen.getByRole('button', { name: '清除筛选' }));
    expect(screen.getAllByRole('button', { name: /^查看任务：/ })).toHaveLength(20);
  });

  it('preserves execution-versus-task completion and does not invent progress', () => {
    useTaskRuntimeStore.setState((state) => ({
      todos: state.todos.map((todo, index) => ({
        ...todo,
        status: index === 0 ? 'running' : 'pending',
      })),
      activeRun: { ...run, status: 'running' },
    }));
    useSubagentRunStore.setState({
      runs: {
        execution: {
          runId: run.run_id,
          subagentRunId: 'subagent-1',
          taskId: 'task-1',
          agent: 'explorer',
          startedAt: 1,
          status: 'completed',
          events: [],
        },
      },
    });
    render(<TaskRuntimePanel />);
    expect(screen.getByText('执行已完成 · 评审/收尾中')).toBeTruthy();
    expect(screen.getByText('执行完成 1 项')).toBeTruthy();
    expect(screen.getByLabelText('已提交完成 0 / 3 项任务')).toBeTruthy();
    expect(screen.queryByText(/\d+%/)).toBeNull();
  });

  it('opens complete task details and returns keyboard focus without changing the task', () => {
    render(<TaskRuntimePanel />);
    const trigger = screen.getByRole('button', { name: '查看任务：任务 2 中文 🧭' });
    trigger.focus();
    fireEvent.click(trigger);
    const dialog = screen.getByRole('dialog', { name: '计划任务详情' });
    expect(within(dialog).getByRole('heading', { name: '任务 2 中文 🧭' })).toBeTruthy();
    expect(within(dialog).getByText('任务 1 中文 🧭')).toBeTruthy();
    expect(within(dialog).getByText('src/中文文件.ts')).toBeTruthy();
    expect(dialog.contains(document.activeElement)).toBe(true);
    fireEvent.keyDown(document, { key: 'Escape' });
    expect(screen.queryByRole('dialog')).toBeNull();
    expect(document.activeElement).toBe(trigger);
    expect(useTaskRuntimeStore.getState().todos.at(1)?.status).toBe('pending');
  });

  it('keeps an unsuccessful edit visible with the original error for retry', async () => {
    const updateTask = vi.fn(async () => {
      useTaskRuntimeStore.setState({ error: '任务图版本已更新，请刷新后重试' });
    });
    useTaskRuntimeStore.setState({ updateTask });
    render(<TaskRuntimePanel />);
    fireEvent.click(screen.getByRole('button', { name: '查看任务：任务 1 中文 🧭' }));
    const dialog = screen.getByRole('dialog');
    fireEvent.click(within(dialog).getByRole('button', { name: '编辑标题' }));
    fireEvent.change(within(dialog).getByRole('textbox', { name: '任务标题' }), {
      target: { value: '保留草稿 🧭' },
    });
    fireEvent.click(within(dialog).getByRole('button', { name: '保存标题' }));
    await waitFor(() =>
      expect(updateTask).toHaveBeenCalledWith('task-1', { title: '保留草稿 🧭' })
    );
    await waitFor(() =>
      expect(within(dialog).getByRole('alert').textContent).toContain('任务图版本已更新')
    );
    expect(
      within(dialog).getByRole<HTMLTextAreaElement>('textbox', { name: '任务标题' }).value
    ).toBe('保留草稿 🧭');
  });

  it('waits for an original retry receipt and never marks the task completed optimistically', async () => {
    let resolveRetry: (() => void) | undefined;
    const retryBlockedTask = vi.fn(
      () =>
        new Promise<void>((resolve) => {
          resolveRetry = resolve;
        })
    );
    useTaskRuntimeStore.setState((state) => ({
      todos: state.todos.map((todo, index) => ({
        ...todo,
        status: index === 0 ? 'blocked' : 'pending',
      })),
      retryBlockedTask,
    }));
    render(<TaskRuntimePanel />);
    fireEvent.click(screen.getByRole('button', { name: '查看任务：任务 1 中文 🧭' }));
    const dialog = screen.getByRole('dialog');
    const retry = within(dialog).getByRole('button', { name: '重试' });
    fireEvent.click(retry);
    fireEvent.click(retry);
    expect(retryBlockedTask).toHaveBeenCalledTimes(1);
    expect(useTaskRuntimeStore.getState().todos.at(0)?.status).toBe('blocked');
    expect(within(dialog).getByText('当前运行有操作请求处理中…')).toBeTruthy();
    await act(async () => resolveRetry?.());
    expect(within(dialog).getByRole('status').textContent).toContain('请求已返回 · 当前运行已暂停');
    expect(useTaskRuntimeStore.getState().todos.at(0)?.status).toBe('blocked');
  });

  it('requires explicit skip confirmation and invokes the existing task operation', async () => {
    const skipTask = vi.fn(async () => {});
    useTaskRuntimeStore.setState({ skipTask });
    render(<TaskRuntimePanel />);
    fireEvent.click(screen.getByRole('button', { name: '查看任务：任务 1 中文 🧭' }));
    const dialog = screen.getByRole('dialog');
    fireEvent.click(within(dialog).getByRole('button', { name: '跳过' }));
    expect(skipTask).not.toHaveBeenCalled();
    fireEvent.click(within(dialog).getByRole('button', { name: '确认跳过' }));
    await waitFor(() => expect(skipTask).toHaveBeenCalledWith('task-1'));
  });

  it('keeps a late receipt on its original task when another detail is opened', async () => {
    let resolveRetry: (() => void) | undefined;
    const retryBlockedTask = vi.fn(
      () =>
        new Promise<void>((resolve) => {
          resolveRetry = resolve;
        })
    );
    useTaskRuntimeStore.setState((state) => ({
      todos: state.todos.map((todo) => ({ ...todo, status: 'blocked' })),
      retryBlockedTask,
    }));
    render(<TaskRuntimePanel />);
    fireEvent.click(screen.getByRole('button', { name: '查看任务：任务 1 中文 🧭' }));
    fireEvent.click(within(screen.getByRole('dialog')).getByRole('button', { name: '重试' }));
    fireEvent.click(screen.getByRole('button', { name: '关闭任务详情' }));
    fireEvent.click(screen.getByRole('button', { name: '查看任务：任务 2 中文 🧭' }));
    const secondDialog = screen.getByRole('dialog');
    await act(async () => resolveRetry?.());
    expect(retryBlockedTask).toHaveBeenCalledWith('task-1');
    expect(within(secondDialog).queryByRole('status')).toBeNull();
    expect(within(secondDialog).queryByRole('alert')).toBeNull();
    fireEvent.click(screen.getByRole('button', { name: '关闭任务详情' }));
    fireEvent.click(screen.getByRole('button', { name: '查看任务：任务 1 中文 🧭' }));
    expect(within(screen.getByRole('dialog')).getByRole('status').textContent).toContain(
      '重试任务请求已返回'
    );
    expect(useTaskRuntimeStore.getState().todos.every((todo) => todo.status === 'blocked')).toBe(
      true
    );
  });

  it('leaves missing timestamps unknown', () => {
    expect(taskTimestamp(null)).toBe('未记录');
    expect(taskTimestamp('not-a-date')).toBe('未记录');
  });
});
