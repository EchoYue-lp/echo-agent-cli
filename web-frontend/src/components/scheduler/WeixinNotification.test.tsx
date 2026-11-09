// @vitest-environment jsdom
import { afterEach, beforeEach, expect, it, vi } from 'vitest';
import { act, cleanup, fireEvent, render, screen } from '@testing-library/react';
import { WeixinNotificationConfiguration, WeixinNotificationResult } from './WeixinNotification';

const api = vi.hoisted(() => ({
  configuration: vi.fn(),
  configure: vi.fn(),
  result: vi.fn(),
}));
vi.mock('../../api/endpoints', () => ({ weixinNotificationApi: api }));

function deferred<T>() {
  let resolve: ((value: T) => void) | undefined;
  const promise = new Promise<T>((complete) => {
    resolve = complete;
  });
  return { promise, resolve: (value: T) => resolve?.(value) };
}

async function settle() {
  await act(async () => {});
}

beforeEach(() => {
  vi.useFakeTimers();
  vi.resetAllMocks();
  api.configuration.mockResolvedValue({ enabled: false, revision: 'config-1' });
  api.configure.mockResolvedValue({ enabled: true, revision: 'config-2' });
});
afterEach(() => {
  cleanup();
  vi.useRealTimers();
});

it('updates the original task and definition with its configuration revision', async () => {
  api.configure.mockResolvedValue({ enabled: true, revision: 'config-2', account_id: 'account' });
  render(
    <WeixinNotificationConfiguration task={{ id: 'task-1', definition_id: 'definition-1' }} />
  );
  await settle();
  fireEvent.click(screen.getByRole('button', { name: '启用微信结果通知' }));
  await settle();
  expect(api.configuration).toHaveBeenCalledWith('definition-1');
  expect(api.configure).toHaveBeenCalledWith('task-1', 'definition-1', 'config-1', true);
  expect(screen.getByText('微信结果通知已启用')).toBeTruthy();
  expect(screen.getByText('接收账号：account')).toBeTruthy();
  expect(screen.queryByRole('textbox')).toBeNull();
});

it('requires an explicit reload after an unconfirmed configuration update', async () => {
  api.configure.mockRejectedValue(new Error('configuration changed'));
  render(
    <WeixinNotificationConfiguration task={{ id: 'task-1', definition_id: 'definition-1' }} />
  );
  await settle();
  fireEvent.click(screen.getByRole('button', { name: '启用微信结果通知' }));
  await settle();
  expect(screen.getByRole('alert').textContent).toBe(
    '微信通知更新未确认，请重新载入配置后再操作。'
  );
  expect(screen.queryByText('微信结果通知已启用')).toBeNull();
  expect(
    (screen.getByRole('button', { name: '启用微信结果通知' }) as HTMLButtonElement).disabled
  ).toBe(true);
  api.configuration.mockResolvedValue({ enabled: true, revision: 'config-2' });
  fireEvent.click(screen.getByRole('button', { name: '重新载入通知配置' }));
  await settle();
  expect(screen.getByText('微信结果通知已启用')).toBeTruthy();
  expect(api.configure).toHaveBeenCalledTimes(1);
});

it('does not apply a previous definition response after the task changes', async () => {
  const old = deferred<{ enabled: boolean; revision: string; account_id: string }>();
  api.configuration.mockReturnValueOnce(old.promise);
  const view = render(
    <WeixinNotificationConfiguration task={{ id: 'task-1', definition_id: 'definition-1' }} />
  );
  view.rerender(
    <WeixinNotificationConfiguration task={{ id: 'task-2', definition_id: 'definition-2' }} />
  );
  await settle();
  await act(async () => old.resolve({ enabled: true, revision: 'old', account_id: 'old-account' }));
  expect(screen.queryByText('接收账号：old-account')).toBeNull();
  fireEvent.click(screen.getByRole('button', { name: '启用微信结果通知' }));
  expect(api.configure).toHaveBeenCalledWith('task-2', 'definition-2', 'config-1', true);
  await settle();
});

it('polls only its exact run until the original producer reports a terminal outcome', async () => {
  api.result
    .mockResolvedValueOnce({ configured: true, run_id: 'run-1', phase: 'awaiting_result' })
    .mockResolvedValueOnce({ configured: true, run_id: 'run-1', phase: 'effect_started' })
    .mockResolvedValue({
      configured: true,
      run_id: 'run-1',
      phase: 'turn_settled',
      outcome: 'completed',
    });
  render(<WeixinNotificationResult runId="run-1" />);
  await settle();
  expect(screen.getByText('等待本次任务结果后发送微信通知')).toBeTruthy();
  await act(async () => vi.advanceTimersByTimeAsync(2000));
  expect(screen.getByText('微信发送已开始，结果待确认')).toBeTruthy();
  expect(screen.queryByText('微信已接受结果通知')).toBeNull();
  await act(async () => vi.advanceTimersByTimeAsync(2000));
  expect(screen.getByText('微信已接受结果通知')).toBeTruthy();
  await act(async () => vi.advanceTimersByTimeAsync(10000));
  expect(api.result.mock.calls).toEqual([['run-1'], ['run-1'], ['run-1']]);
  expect(api.configure).not.toHaveBeenCalled();
});

it('keeps an unknown send outcome terminal without claiming acceptance or retrying', async () => {
  api.result.mockResolvedValue({
    configured: true,
    run_id: 'run-unknown',
    phase: 'effect_started',
    outcome: 'outcome_unknown',
    reason: '服务回执未确认',
  });
  render(<WeixinNotificationResult runId="run-unknown" />);
  await settle();
  expect(screen.getByText('微信发送结果未知，请勿直接重发')).toBeTruthy();
  expect(screen.getByText('服务回执未确认')).toBeTruthy();
  await act(async () => vi.advanceTimersByTimeAsync(10000));
  expect(api.result).toHaveBeenCalledTimes(1);
  expect(screen.queryByText('微信已接受结果通知')).toBeNull();
});

it('ignores a late result after changing runs and stops on no frozen obligation', async () => {
  const old = deferred<{ configured: boolean; run_id: string; outcome: string }>();
  api.result.mockReturnValueOnce(old.promise).mockResolvedValue({
    configured: false,
    run_id: 'run-2',
  });
  const view = render(<WeixinNotificationResult runId="run-1" />);
  view.rerender(<WeixinNotificationResult runId="run-2" />);
  await settle();
  await act(async () => old.resolve({ configured: true, run_id: 'run-1', outcome: 'completed' }));
  expect(screen.getByText('本次运行未配置微信结果通知')).toBeTruthy();
  expect(screen.queryByText('微信已接受结果通知')).toBeNull();
  await act(async () => vi.advanceTimersByTimeAsync(10000));
  expect(api.result.mock.calls).toEqual([['run-1'], ['run-2']]);
});
