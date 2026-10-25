// @vitest-environment jsdom
import { afterEach, beforeEach, expect, it, vi } from 'vitest';
import { cleanup, fireEvent, render, screen, waitFor } from '@testing-library/react';
import { ChannelsPanel } from './ChannelsPanel';
const api = vi.hoisted(() => ({
  configuration: vi.fn(),
  save: vi.fn(),
  status: vi.fn(),
  control: vi.fn(),
}));
vi.mock('../../api/endpoints', () => ({ channelApi: api }));
const configuration = {
  revision: 'desired-new',
  qq: { enabled: true, app_id: 'app', has_secret: true },
  feishu: {
    enabled: false,
    app_id: '',
    has_secret: false,
    mode: 'long_poll',
    webhook_bind: '127.0.0.1:9000',
    webhook_path: '/webhook/feishu',
    has_verification_token: false,
  },
  session: { timeout_minutes: 30 },
};
const instance = {
  id: 'qqbot',
  registered: true,
  configuration_revision: 'desired-new',
  applied_configuration_revision: 'applied-old',
  mutation_epoch: '9',
  completed_cleanup_failures: [],
  handler_attached: true,
  active_invocations: 1,
  shutdown_settled: false,
  pump_shutdown_settled: false,
  transport: { phase: 'unknown', evidence: null, error: null, observed_at: null },
};
beforeEach(() => {
  vi.clearAllMocks();
  api.configuration.mockResolvedValue(configuration);
  api.status.mockResolvedValue({ configuration_revision: 'desired-new', channels: [instance] });
});
afterEach(cleanup);
it('preserves omitted secrets and keeps saving separate from original connection control', async () => {
  api.save.mockResolvedValue({ ...configuration, revision: 'saved' });
  render(<ChannelsPanel />);
  const app = await screen.findByLabelText('QQ Bot App ID');
  fireEvent.change(app, { target: { value: 'new-app' } });
  fireEvent.click(screen.getByRole('button', { name: '保存QQ Bot配置' }));
  await waitFor(() =>
    expect(api.save).toHaveBeenCalledWith(
      expect.objectContaining({
        channel: 'qqbot',
        expected_revision: 'desired-new',
        app_id: 'new-app',
        secret: { action: 'preserve' },
      })
    )
  );
  expect(api.control).not.toHaveBeenCalled();
  expect(await screen.findByText('QQ Bot配置已保存')).toBeTruthy();
  expect(screen.queryByText('在线')).toBeNull();
});
it('disconnects the delivered old instance even when desired revision is newer', async () => {
  api.control.mockResolvedValue({ configuration_revision: 'desired-new', channels: [instance] });
  render(<ChannelsPanel />);
  fireEvent.click(await screen.findByRole('button', { name: '断开QQ Bot' }));
  await waitFor(() =>
    expect(api.control).toHaveBeenCalledWith('qqbot', 'desired-new', instance, 'stop')
  );
  expect(screen.getAllByText(/未知/).length).toBeGreaterThan(0);
  expect(screen.queryByText('已断开')).toBeNull();
});
it('shows completed cleanup errors separately from current connection evidence', async () => {
  api.status.mockResolvedValue({
    configuration_revision: 'desired-new',
    channels: [
      {
        ...instance,
        completed_cleanup_failures: ['old pump failed'],
        shutdown_settled: true,
        handler_attached: false,
        active_invocations: 0,
      },
    ],
  });
  render(<ChannelsPanel />);
  expect(await screen.findByText('上次清理已结算，保留失败记录：old pump failed')).toBeTruthy();
  expect(screen.getByText(/原连接资源已结算/)).toBeTruthy();
  expect(screen.queryByText('在线')).toBeNull();
});
