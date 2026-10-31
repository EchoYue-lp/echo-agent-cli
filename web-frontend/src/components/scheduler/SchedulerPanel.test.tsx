// @vitest-environment jsdom
import { act, cleanup, fireEvent, render, screen, waitFor } from '@testing-library/react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

vi.hoisted(() => {
  Object.defineProperty(window, 'matchMedia', {
    configurable: true,
    value: () => ({ matches: false }),
  });
});
import { SchedulerPanel } from './SchedulerPanel';

const addToast = vi.hoisted(() => vi.fn());
vi.mock('../../stores/toastStore', () => ({
  useToastStore: (selector: (state: { addToast: typeof addToast }) => unknown) =>
    selector({ addToast }),
}));

const api = vi.hoisted(() => ({
  list: vi.fn(),
  create: vi.fn(),
  edit: vi.fn(),
  run: vi.fn(),
  updateStatus: vi.fn(),
  delete: vi.fn(),
  history: vi.fn(),
}));
const notifications = vi.hoisted(() => ({
  configuration: vi.fn(),
  configure: vi.fn(),
  result: vi.fn(),
}));
vi.mock('../../api/endpoints', () => ({
  schedulerApi: api,
  weixinNotificationApi: notifications,
}));
const task = {
  id: 'task-a',
  definition_id: 'definition-old',
  control_revision: 7,
  name: '任务 A',
  cron_expr: '* * * * *',
  prompt: '检查结果',
  status: 'enabled',
  last_run_at: null,
  last_result: null,
  created_at: '2026-10-24T00:00:00Z',
  next_run: null,
};

afterEach(cleanup);
beforeEach(() => {
  vi.clearAllMocks();
  api.list.mockResolvedValue([task]);
  notifications.configuration.mockResolvedValue({ enabled: false, revision: 'notification-1' });
  notifications.result.mockImplementation(async (runId: string) => ({
    configured: false,
    run_id: runId,
  }));
});

describe('original scheduler management receipts', () => {
  it('edits the captured definition and retains the form on a stale-definition response', async () => {
    api.edit.mockResolvedValue({
      success: false,
      cause: 'definition_conflict',
      error: '定义已变化',
    });
    render(<SchedulerPanel />);
    fireEvent.click(await screen.findByRole('button', { name: '编辑任务 A' }));
    fireEvent.change(screen.getByPlaceholderText('任务名称'), { target: { value: '新名称' } });
    fireEvent.click(screen.getByRole('button', { name: '保存修改' }));
    await waitFor(() =>
      expect(api.edit).toHaveBeenCalledWith(task, expect.objectContaining({ name: '新名称' }))
    );
    expect(screen.getByDisplayValue('新名称')).toBeTruthy();
    expect(await screen.findByText(/定义已变化/)).toBeTruthy();
    expect(api.create).not.toHaveBeenCalled();
  });

  it('keeps an unknown physical/commit result distinct from completed execution', async () => {
    api.run.mockResolvedValue({
      success: false,
      outcome_unknown: true,
      cause: 'journal_failed',
      result: 'callback finished',
      error: 'terminal commit unknown',
      occurrence: {
        task_id: 'task-a',
        definition_id: 'definition-old',
        occurrence_id: 'manual-a',
        scheduled_at: '2026-10-24T00:00:00Z',
        trigger: 'manual',
      },
      attempt_id: 'attempt-a',
      attempt: 1,
    });
    render(<SchedulerPanel />);
    fireEvent.click(await screen.findByRole('button', { name: '手动运行' }));
    expect(await screen.findByText(/结算未知/)).toBeTruthy();
    expect(screen.queryByText('运行完成')).toBeNull();
    expect(screen.getByText(/manual-a/)).toBeTruthy();
    expect(screen.getByText('callback finished')).toBeTruthy();
    expect(api.run).toHaveBeenCalledTimes(1);
  });

  it('reads the exact manual run receipt separately from successful task completion', async () => {
    api.run.mockResolvedValue({
      success: true,
      result: '真实任务结果',
      run_lookup: {
        lookup: 'found',
        run: {
          run_id: 'manual-run-original',
          workspace_id: 'workspace-original',
          conversation_id: 'conversation-original',
          status: 'completed',
        },
      },
    });
    notifications.result.mockResolvedValue({
      configured: true,
      run_id: 'manual-run-original',
      phase: 'effect_started',
      outcome: 'outcome_unknown',
    });
    render(<SchedulerPanel />);
    fireEvent.click(await screen.findByRole('button', { name: '手动运行' }));
    expect(await screen.findByText('运行完成')).toBeTruthy();
    expect(await screen.findByText('微信发送结果未知，请勿直接重发')).toBeTruthy();
    expect(notifications.result).toHaveBeenCalledWith('manual-run-original');
    expect(api.history).not.toHaveBeenCalled();
    expect(api.run).toHaveBeenCalledTimes(1);
  });
});

function deferred<T>() {
  let complete!: (value: T) => void;
  const promise = new Promise<T>((resolve) => {
    complete = resolve;
  });
  return { promise, resolve: (value: T) => complete(value) };
}

describe('manual occurrence observations stay with their original task', () => {
  it('keeps concurrent pending/result panels and never unlocks another pending task', async () => {
    const first = deferred<{ success: boolean; result: string }>();
    const second = deferred<{ success: boolean; result: string }>();
    api.list.mockResolvedValue([task, { ...task, id: 'task-b', name: '任务 B' }]);
    api.run.mockImplementation((id: string) => (id === 'task-a' ? first.promise : second.promise));
    render(<SchedulerPanel />);
    const buttons = await screen.findAllByRole('button', { name: '手动运行' });
    fireEvent.click(buttons[0]);
    fireEvent.click(buttons[1]);
    expect(screen.getAllByText('运行中...')).toHaveLength(2);
    expect((buttons[0] as HTMLButtonElement).disabled).toBe(true);
    expect((buttons[1] as HTMLButtonElement).disabled).toBe(true);
    await act(async () => first.resolve({ success: true, result: 'A 的原始回执' }));
    expect(screen.getByText('A 的原始回执')).toBeTruthy();
    expect(screen.getByText('运行中...')).toBeTruthy();
    expect((buttons[1] as HTMLButtonElement).disabled).toBe(true);
    fireEvent.click(buttons[1]);
    expect(api.run.mock.calls).toEqual([['task-a'], ['task-b']]);
    await act(async () => second.resolve({ success: true, result: 'B 的原始回执' }));
    expect(screen.getByText('A 的原始回执')).toBeTruthy();
    expect(screen.getByText('B 的原始回执')).toBeTruthy();
    expect(screen.getAllByText('运行完成')).toHaveLength(2);
  });

  it('does not replace B with a late A failure and permits a fresh manual A after settlement', async () => {
    const first = deferred<{ success: boolean; outcome_unknown?: boolean; error?: string }>();
    api.list.mockResolvedValue([task, { ...task, id: 'task-b', name: '任务 B' }]);
    api.run.mockImplementation((id: string) =>
      id === 'task-a'
        ? first.promise
        : Promise.resolve({
            success: true,
            result: 'B 独立 occurrence',
          })
    );
    render(<SchedulerPanel />);
    const buttons = await screen.findAllByRole('button', { name: '手动运行' });
    fireEvent.click(buttons[0]);
    fireEvent.click(buttons[1]);
    await screen.findByText('B 独立 occurrence');
    await act(async () =>
      first.resolve({ success: false, outcome_unknown: true, error: 'A 结算未知' })
    );
    expect(screen.getByText('B 独立 occurrence')).toBeTruthy();
    expect(screen.getByText('A 结算未知')).toBeTruthy();
    expect(api.run).toHaveBeenCalledTimes(2);
    api.run.mockResolvedValueOnce({ success: true, result: 'A 的第二个独立 occurrence' });
    fireEvent.click(buttons[0]);
    expect(await screen.findByText('A 的第二个独立 occurrence')).toBeTruthy();
    expect(screen.getByText('B 独立 occurrence')).toBeTruthy();
    expect(api.run.mock.calls).toEqual([['task-a'], ['task-b'], ['task-a']]);
  });

  it('retains an IPC rejection as unconfirmed execution without automatically rerunning', async () => {
    api.run.mockRejectedValue(new Error('原 owner 的响应连接已关闭'));
    render(<SchedulerPanel />);
    fireEvent.click(await screen.findByRole('button', { name: '手动运行' }));
    expect(await screen.findByText(/结算未知/)).toBeTruthy();
    expect(screen.queryByText('运行完成')).toBeNull();
    expect(addToast).toHaveBeenCalledWith('warning', expect.stringContaining('回执未确认'));
    expect(api.run).toHaveBeenCalledTimes(1);
    expect(api.history).not.toHaveBeenCalled();
  });

  it('keeps the latest task list when independent operations refresh out of order', async () => {
    const first = deferred<{ success: boolean; result: string }>();
    const second = deferred<{ success: boolean; result: string }>();
    const oldList = deferred<Array<typeof task>>();
    const tasks = [task, { ...task, id: 'task-b', name: '任务 B' }];
    api.list.mockResolvedValue(tasks);
    api.run.mockImplementation((id: string) => (id === 'task-a' ? first.promise : second.promise));
    render(<SchedulerPanel />);
    const buttons = await screen.findAllByRole('button', { name: '手动运行' });
    fireEvent.click(buttons[0]);
    fireEvent.click(buttons[1]);
    api.list
      .mockReturnValueOnce(oldList.promise)
      .mockResolvedValueOnce([{ ...task, name: '最新定义 A' }, tasks[1]]);
    await act(async () => first.resolve({ success: true, result: 'A 已返回' }));
    await act(async () => second.resolve({ success: true, result: 'B 已返回' }));
    expect(screen.getByText('最新定义 A')).toBeTruthy();
    await act(async () => oldList.resolve(tasks));
    expect(screen.getByText('最新定义 A')).toBeTruthy();
    expect(screen.queryByText('任务 A')).toBeNull();
  });

  it('drops a closed panel observer while the admitted original operation continues', async () => {
    const original = deferred<{ success: boolean; result: string }>();
    api.run.mockReturnValue(original.promise);
    const old = render(<SchedulerPanel />);
    fireEvent.click(await screen.findByRole('button', { name: '手动运行' }));
    old.unmount();
    render(<SchedulerPanel />);
    await screen.findByText('任务 A');
    const listCalls = api.list.mock.calls.length;
    addToast.mockClear();
    await act(async () => original.resolve({ success: true, result: '旧页面已准入的真实回执' }));
    expect(screen.queryByText('旧页面已准入的真实回执')).toBeNull();
    expect(addToast).not.toHaveBeenCalled();
    expect(api.list).toHaveBeenCalledTimes(listCalls);
    expect(api.run).toHaveBeenCalledTimes(1);
  });
});
