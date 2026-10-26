// @vitest-environment jsdom
import { cleanup, fireEvent, render, screen, waitFor } from '@testing-library/react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { SchedulerPanel } from './SchedulerPanel';

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
