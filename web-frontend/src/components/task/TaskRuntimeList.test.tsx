// @vitest-environment jsdom
import { act, cleanup, fireEvent, render, screen, waitFor, within } from '@testing-library/react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { TaskRuntimePanel } from './TaskRuntimePanel';
import { RightRail } from '../layout/RightRail';
import { useWorkspaceStore } from '../../stores/workspaceStore';
import { useConversationStore } from '../../stores/conversationStore';
import { taskTimestamp } from './TaskRuntimeList';
import { useTaskRuntimeStore, taskRuntimePresentation } from '../../stores/taskRuntimeStore';
import { FailureToast } from '../chat/FailureToast';
import { MessageBubble } from '../chat/MessageBubble';
import { useChatStore } from '../../stores/chatStore';
import { useSubagentRunStore } from '../../stores/subagentRunStore';
import type { PlanRevision, TaskRun, TaskSpec, TodoItem, TaskRetryReceipt } from '../../generated';

const originalActions = useTaskRuntimeStore.getState();
const run: TaskRun = {
  run_id: 'run-1',
  workspace_id: 'workspace-1',
  conversation_id: 'conversation-1',
  root_message_id: 'message-1',
  domain_profile: 'general',
  domain_binding: null,
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
    domain_binding: null,
    domain_role: null,
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
    domain_binding: null,
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
    taskScope: { workspaceId: 'workspace-1', workspaceGeneration: 'generation-1' },
    historySnapshot: null,
    historyError: null,
    selectedRunId: null,
    runPage: [],
    runStatuses: [],
    loadingRuns: false,
    loadingRun: false,
    runListError: null,
    nextRunContinuation: null,
    hasMoreEvents: false,
    loadingEvents: false,
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
        'bindScope',
        'loadRuns',
        'loadByRun',
        'followConversation',
        'loadMoreEvents',
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
      expect(updateTask).toHaveBeenCalledWith(
        'task-1',
        { title: '保留草稿 🧭' },
        { workspace_id: 'workspace-1', run_id: 'run-1', workspace_generation: 'generation-1' }
      )
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
        new Promise<TaskRetryReceipt>((resolve) => {
          resolveRetry = () =>
            resolve({
              kind: 'recovery_retry_recorded',
              run_id: 'run-1',
              task_id: 'task-1',
              next_attempt: 2,
            });
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
    expect(within(dialog).getByRole('status').textContent).toContain(
      '恢复重试已记录 · 当前运行已暂停'
    );
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
    await waitFor(() =>
      expect(skipTask).toHaveBeenCalledWith('task-1', {
        workspace_id: 'workspace-1',
        run_id: 'run-1',
        workspace_generation: 'generation-1',
      })
    );
  });

  it('keeps a replacement generation action pending when the old same-run receipt arrives', async () => {
    let finishOld: (() => void) | undefined;
    let finishNew: (() => void) | undefined;
    const pause = vi
      .fn()
      .mockImplementationOnce(
        () =>
          new Promise((resolve) => {
            finishOld = () => resolve({ success: false, run_id: 'run-1' });
          })
      )
      .mockImplementationOnce(
        () =>
          new Promise((resolve) => {
            finishNew = () => resolve({ success: true, run_id: 'run-1' });
          })
      );
    useTaskRuntimeStore.setState({ activeRun: { ...run, status: 'running' }, pause });
    render(<TaskRuntimePanel />);
    fireEvent.click(screen.getByRole('button', { name: '暂停' }));
    expect(pause).toHaveBeenLastCalledWith({
      workspace_id: 'workspace-1',
      run_id: 'run-1',
      workspace_generation: 'generation-1',
    });
    try {
      act(() =>
        useTaskRuntimeStore.setState({
          taskScope: { workspaceId: 'workspace-1', workspaceGeneration: 'generation-2' },
        })
      );
      fireEvent.click(screen.getByRole('button', { name: '暂停' }));
      expect(pause).toHaveBeenCalledTimes(2);
      expect(pause).toHaveBeenLastCalledWith({
        workspace_id: 'workspace-1',
        run_id: 'run-1',
        workspace_generation: 'generation-2',
      });
      await act(async () => finishOld?.());
      expect(screen.getByRole('button', { name: '暂停' }).hasAttribute('disabled')).toBe(true);
      expect(screen.queryByText(/无需重复执行/)).toBeNull();
      await act(async () => finishNew?.());
      expect(screen.getByRole('button', { name: '暂停' }).hasAttribute('disabled')).toBe(false);
      expect(screen.getByRole('status').textContent).toContain('后端已接受请求');
    } finally {
      await act(async () => {
        finishOld?.();
        finishNew?.();
      });
    }
  });

  it('keeps a late receipt on its original task when another detail is opened', async () => {
    let resolveRetry: (() => void) | undefined;
    const retryBlockedTask = vi.fn(
      () =>
        new Promise<TaskRetryReceipt>((resolve) => {
          resolveRetry = () =>
            resolve({
              kind: 'recovery_retry_recorded',
              run_id: 'run-1',
              task_id: 'task-1',
              next_attempt: 2,
            });
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
    expect(retryBlockedTask).toHaveBeenCalledWith('task-1', {
      workspace_id: 'workspace-1',
      run_id: 'run-1',
      workspace_generation: 'generation-1',
    });
    expect(within(secondDialog).queryByRole('status')).toBeNull();
    expect(within(secondDialog).queryByRole('alert')).toBeNull();
    fireEvent.click(screen.getByRole('button', { name: '关闭任务详情' }));
    fireEvent.click(screen.getByRole('button', { name: '查看任务：任务 1 中文 🧭' }));
    expect(within(screen.getByRole('dialog')).getByRole('status').textContent).toContain(
      '重试任务恢复重试已记录'
    );
    expect(useTaskRuntimeStore.getState().todos.every((todo) => todo.status === 'blocked')).toBe(
      true
    );
  });

  it('leaves missing timestamps unknown', () => {
    expect(taskTimestamp(null)).toBe('未记录');
    expect(taskTimestamp('not-a-date')).toBe('未记录');
  });
  it('presents the original typed no-op receipt without changing the displayed runtime state', async () => {
    const pause = vi.fn(async () => ({ success: false, run_id: 'run-1' }));
    useTaskRuntimeStore.setState({ activeRun: { ...run, status: 'running' }, pause });
    render(<TaskRuntimePanel />);
    fireEvent.click(screen.getByRole('button', { name: '暂停' }));
    await waitFor(() => expect(screen.getByRole('status').textContent).toContain('无需重复执行'));
    expect(useTaskRuntimeStore.getState().activeRun?.status).toBe('running');
  });

  it('makes an unfinished event history visible and reads its next page only on the original action', () => {
    const loadMoreEvents = vi.fn(async () => {});
    useTaskRuntimeStore.setState({ hasMoreEvents: true, loadMoreEvents });
    render(<TaskRuntimePanel />);
    expect(screen.getByRole('status').textContent).toContain('可能不完整');
    fireEvent.click(screen.getByRole('button', { name: '继续读取执行记录' }));
    expect(loadMoreEvents).toHaveBeenCalledTimes(1);
  });

  it('uses the actual RightRail run inventory and captured workspace generation for history selection', () => {
    const previousWorkspace = useWorkspaceStore.getState().current;
    const previousConversation = useConversationStore.getState().activeId;
    const bindScope = vi.fn();
    const loadRuns = vi.fn(async () => {});
    const loadByRun = vi.fn(async () => {});
    useWorkspaceStore.setState({
      current: {
        id: 'workspace-1',
        name: '项目',
        root: '/local/data',
        kind: { type: 'coding' },
        metadata: { tags: [] },
        product_data_generation: 'generation-1',
        created_at: '2026-10-25T00:00:00Z',
        last_active: '2026-10-25T00:00:00Z',
      },
    });
    useConversationStore.setState({ activeId: null });
    useTaskRuntimeStore.setState({
      bindScope,
      loadRuns,
      loadByRun,
      runPage: [{ ...run, run_id: 'historical', goal: '历史目标 中文 🧭' }],
      nextRunContinuation: 'next',
    });
    try {
      render(<RightRail />);
      fireEvent.click(screen.getByRole('button', { name: /历史目标 中文/ }));
      expect(loadByRun).toHaveBeenCalledWith(
        { workspaceId: 'workspace-1', workspaceGeneration: 'generation-1' },
        'historical'
      );
      fireEvent.click(screen.getByRole('button', { name: '下一页运行' }));
      expect(loadRuns).toHaveBeenLastCalledWith(
        { workspaceId: 'workspace-1', workspaceGeneration: 'generation-1' },
        [],
        true
      );
    } finally {
      useWorkspaceStore.setState({ current: previousWorkspace });
      useConversationStore.setState({ activeId: previousConversation });
    }
  });
  it('does not treat a completed attempt in an unread prefix as the current execution', () => {
    useTaskRuntimeStore.setState({ hasMoreEvents: true, activeRun: { ...run, status: 'running' } });
    useSubagentRunStore.setState({
      runs: {
        old: {
          runId: 'run-1',
          subagentRunId: 'old-attempt',
          taskId: 'task-1',
          agent: 'explorer',
          startedAt: 1,
          status: 'completed',
          events: [],
        },
      },
    });
    render(<TaskRuntimePanel />);
    expect(screen.getByText('执行记录尚未读完')).toBeTruthy();
    expect(screen.queryByText('执行完成 1 项')).toBeNull();
    expect(screen.getByText('执行记录未读完，用量汇总待确认。')).toBeTruthy();
  });
  it('keeps real chat failure and message association on the live run while the task panel inspects history', () => {
    const chat = useTaskRuntimeStore.getState();
    const historical = {
      ...taskRuntimePresentation(chat),
      activeRun: {
        ...run,
        run_id: 'historical',
        status: 'failed' as const,
        root_message_id: 'old-message',
        goal: '历史失败目标',
      },
      todos: chat.todos.map((todo) => ({ ...todo, status: 'failed' as const })),
    };
    useTaskRuntimeStore.setState({
      activeRun: { ...run, status: 'paused' },
      selectedRunId: 'historical',
      historySnapshot: historical,
    });
    const message = {
      id: 'message-1',
      role: 'assistant' as const,
      content: '当前聊天回复',
      timestamp: 1,
      thinkingSegments: [{ content: '当前执行分析' }],
    };
    useChatStore.setState({ messages: [message] });
    render(
      <>
        <TaskRuntimePanel />
        <FailureToast onDismiss={() => {}} />
        <MessageBubble message={message} onFork={() => {}} />
      </>
    );
    expect(screen.getByRole('heading', { name: '历史失败目标' })).toBeTruthy();
    expect(screen.queryByText('任务执行失败')).toBeNull();
    expect(useTaskRuntimeStore.getState().todos).toHaveLength(3);
    expect(screen.getByRole<HTMLButtonElement>('button', { name: '分叉会话' }).disabled).toBe(true);
  });
  it('uses the existing attempt selector when a newer retry has an earlier clock timestamp', () => {
    useTaskRuntimeStore.setState((state) => ({
      activeRun: { ...run, status: 'running' },
      todos: state.todos.map((todo, index) => ({
        ...todo,
        status: index === 0 ? 'running' : 'pending',
      })),
    }));
    useSubagentRunStore.setState({
      runs: {
        old: {
          runId: 'run-1',
          subagentRunId: 'old-attempt',
          taskId: 'task-1',
          attempt: 1,
          agent: 'explorer',
          startedAt: 200,
          status: 'completed',
          events: [],
        },
        current: {
          runId: 'run-1',
          subagentRunId: 'current-attempt',
          taskId: 'task-1',
          attempt: 2,
          agent: 'explorer',
          startedAt: 100,
          status: 'running',
          events: [],
        },
      },
    });
    render(<TaskRuntimePanel />);
    expect(screen.getByText(/^执行完成 \d+ 项$/).textContent).toBe('执行完成 0 项');
    fireEvent.click(screen.getByRole('button', { name: '查看任务：任务 1 中文 🧭' }));
    expect(
      within(screen.getByRole('dialog')).getByRole('button', {
        name: /查看 Subagent 执行.*current-attempt/,
      })
    ).toBeTruthy();
  });
});
