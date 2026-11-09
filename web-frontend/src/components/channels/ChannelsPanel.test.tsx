// @vitest-environment jsdom
import { afterEach, beforeEach, expect, it, vi } from 'vitest';
import { act, cleanup, fireEvent, render, screen, waitFor, within } from '@testing-library/react';
import { ChannelsPanel } from './ChannelsPanel';
const api = vi.hoisted(() => ({
  configuration: vi.fn(),
  save: vi.fn(),
  status: vi.fn(),
  control: vi.fn(),
}));
const qrApi = vi.hoisted(() => ({
  begin: vi.fn(),
  status: vi.fn(),
  verify: vi.fn(),
  cancel: vi.fn(),
  forget: vi.fn(),
}));
vi.mock('../../api/endpoints', () => ({ channelApi: api, weixinQrApi: qrApi }));
const configuration = {
  revision: 'desired-new',
  weixin: { enabled: false, has_credential: false },
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
  qrApi.status.mockResolvedValue({
    phase: 'idle',
    has_credential: false,
    configuration_revision: 'desired-new',
  });
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
it('preserves Feishu secret handling while Weixin uses its dedicated QR card', async () => {
  api.save.mockResolvedValue({ ...configuration, revision: 'saved' });
  render(<ChannelsPanel />);
  fireEvent.change(await screen.findByLabelText('飞书 App ID'), {
    target: { value: 'feishu-app' },
  });
  fireEvent.click(screen.getByRole('button', { name: '保存飞书配置' }));
  await waitFor(() =>
    expect(api.save).toHaveBeenCalledWith(
      expect.objectContaining({
        channel: 'feishu',
        expected_revision: 'desired-new',
        app_id: 'feishu-app',
        secret: { action: 'preserve' },
        verification_token: { action: 'preserve' },
      })
    )
  );
  expect(screen.getByRole('button', { name: '获取微信二维码' })).toBeTruthy();
  expect(screen.queryByLabelText('个人微信密钥')).toBeNull();
  expect(api.control).not.toHaveBeenCalled();
});

it.each([
  ['qqbot', 'QQ Bot'],
  ['feishu', '飞书'],
] as const)('%s在原IM页保留启用、AppID及设置/明确清除密钥', async (channel, name) => {
  api.save
    .mockResolvedValueOnce({ ...configuration, revision: 'saved' })
    .mockResolvedValueOnce({ ...configuration, revision: 'cleared' });
  render(<ChannelsPanel />);
  const app = await screen.findByLabelText(`${name} App ID`);
  fireEvent.change(app, { target: { value: `${channel}-new-app` } });
  const enabled = screen.getByLabelText(`启用${name}配置`) as HTMLInputElement;
  if (!enabled.checked) fireEvent.click(enabled);
  const secret = screen.getByLabelText(`${name}密钥`) as HTMLInputElement;
  expect(secret.type).toBe('password');
  fireEvent.change(secret, { target: { value: 'synthetic-test-secret' } });
  fireEvent.click(screen.getByRole('button', { name: `保存${name}配置` }));
  await waitFor(() =>
    expect(api.save).toHaveBeenNthCalledWith(
      1,
      expect.objectContaining({
        channel,
        expected_revision: 'desired-new',
        enabled: true,
        app_id: `${channel}-new-app`,
        secret: { action: 'set', value: 'synthetic-test-secret' },
      })
    )
  );
  await waitFor(() => expect(secret.value).toBe(''));
  fireEvent.click(screen.getByLabelText(`明确清除${name}密钥`));
  fireEvent.click(screen.getByRole('button', { name: `保存${name}配置` }));
  await waitFor(() =>
    expect(api.save).toHaveBeenNthCalledWith(
      2,
      expect.objectContaining({
        channel,
        expected_revision: 'saved',
        secret: { action: 'clear' },
      })
    )
  );
  expect(api.control).not.toHaveBeenCalled();
});

it('飞书模式、webhook及验证token继续使用原渠道保存合同', async () => {
  api.save.mockResolvedValue({ ...configuration, revision: 'saved' });
  render(<ChannelsPanel />);
  fireEvent.change(await screen.findByLabelText('飞书连接模式'), {
    target: { value: 'webhook' },
  });
  fireEvent.change(screen.getByLabelText('飞书监听地址'), {
    target: { value: '127.0.0.1:9012' },
  });
  fireEvent.change(screen.getByLabelText('飞书webhook路径'), { target: { value: '/events' } });
  const token = screen.getByLabelText('飞书Verification token') as HTMLInputElement;
  expect(token.type).toBe('password');
  fireEvent.change(token, { target: { value: 'synthetic-verification-token' } });
  fireEvent.click(screen.getByRole('button', { name: '保存飞书配置' }));
  await waitFor(() =>
    expect(api.save).toHaveBeenCalledWith(
      expect.objectContaining({
        channel: 'feishu',
        mode: 'webhook',
        webhook_bind: '127.0.0.1:9012',
        webhook_path: '/events',
        secret: { action: 'preserve' },
        verification_token: { action: 'set', value: 'synthetic-verification-token' },
      })
    )
  );
  expect(await screen.findByText('飞书配置已保存')).toBeTruthy();
  expect(api.control).not.toHaveBeenCalled();
});

it.each([
  ['qqbot', 'QQ Bot'],
  ['feishu', '飞书'],
] as const)('%s缺值校验或保存失败不清草稿、不冒保存成功', async (channel, name) => {
  api.save.mockRejectedValueOnce(new Error('已启用渠道缺少App ID或密钥'));
  render(<ChannelsPanel />);
  const app = await screen.findByLabelText(`${name} App ID`);
  fireEvent.change(app, { target: { value: '' } });
  const enabled = screen.getByLabelText(`启用${name}配置`) as HTMLInputElement;
  if (!enabled.checked) fireEvent.click(enabled);
  const secret = screen.getByLabelText(`${name}密钥`) as HTMLInputElement;
  fireEvent.change(secret, { target: { value: 'synthetic-draft-secret' } });
  fireEvent.click(screen.getByRole('button', { name: `保存${name}配置` }));
  expect(await screen.findByRole('alert')).toHaveProperty(
    'textContent',
    '已启用渠道缺少App ID或密钥'
  );
  expect(api.save).toHaveBeenCalledWith(
    expect.objectContaining({ channel, app_id: '', enabled: true })
  );
  expect(screen.queryByText(`${name}配置已保存`)).toBeNull();
  expect(secret.value).toBe('synthetic-draft-secret');
  expect((app as HTMLInputElement).value).toBe('');
  expect(
    (screen.getByRole('button', { name: `保存${name}配置` }) as HTMLButtonElement).disabled
  ).toBe(false);
  expect(api.control).not.toHaveBeenCalled();
});
it('does not reconnect saved Weixin authorization or infer Ready when opening the page', async () => {
  api.configuration.mockResolvedValue({
    ...configuration,
    weixin: { enabled: true, has_credential: true, account_id: 'personal-account' },
  });
  api.status.mockResolvedValue({
    configuration_revision: 'desired-new',
    channels: [
      { ...instance, id: 'weixin', transport: { ...instance.transport, phase: 'stopped' } },
    ],
  });
  qrApi.status.mockResolvedValue({
    phase: 'authorized',
    has_credential: true,
    account_id: 'personal-account',
    configuration_revision: 'desired-new',
  });
  render(<ChannelsPanel />);
  expect(await screen.findByText('授权已保存')).toBeTruthy();
  expect(screen.queryByText('授权已保存，正在连接')).toBeNull();
  expect(screen.getByText('微信连接：传输已停止')).toBeTruthy();
  expect(screen.queryByText(/微信.*已确认/)).toBeNull();
  expect(api.control).not.toHaveBeenCalled();
});

it('仅在原微信鉴权轮询已就绪时显示确认，保留其它渠道的证据门槛', async () => {
  api.status.mockResolvedValue({
    configuration_revision: 'desired-new',
    channels: [
      {
        ...instance,
        id: 'weixin',
        transport: { ...instance.transport, phase: 'ready', evidence: 'weixin_authenticated_poll' },
      },
      {
        ...instance,
        transport: { ...instance.transport, phase: 'ready', evidence: 'qq_session_ready' },
      },
      {
        ...instance,
        id: 'feishu',
        transport: {
          ...instance.transport,
          phase: 'ready',
          evidence: 'feishu_websocket_handshake',
        },
      },
    ],
  });
  render(<ChannelsPanel />);
  expect(await screen.findByText('微信连接：微信鉴权轮询已确认')).toBeTruthy();
  expect(screen.getByText('QQ 会话就绪已确认')).toBeTruthy();
  expect(screen.getByText('飞书 WebSocket 握手已确认')).toBeTruthy();
  expect(api.control).not.toHaveBeenCalled();
});

it.each([
  ['connecting', 'weixin_authenticated_poll', '正在连接'],
  ['ready', null, '未知（尚无就绪证据）'],
])('微信阶段 %s 不因凭据或不完整证据而冒称Ready', async (phase, evidence, label) => {
  api.configuration.mockResolvedValue({
    ...configuration,
    weixin: { enabled: true, has_credential: true },
  });
  api.status.mockResolvedValue({
    configuration_revision: 'desired-new',
    channels: [
      { ...instance, id: 'weixin', transport: { ...instance.transport, phase, evidence } },
    ],
  });
  render(<ChannelsPanel />);
  expect(await screen.findByText(`微信连接：${label}`)).toBeTruthy();
  expect(screen.queryByText('微信连接：微信鉴权轮询已确认')).toBeNull();
  expect(api.control).not.toHaveBeenCalled();
});

it('微信连接区呈现原传输错误、已结算清理失败和观察时间', async () => {
  const observedAt = '2026-10-26T10:00:00Z';
  api.status.mockResolvedValue({
    configuration_revision: 'desired-new',
    channels: [
      {
        ...instance,
        id: 'weixin',
        completed_cleanup_failures: ['原微信 reader 关闭失败'],
        transport: {
          phase: 'failed',
          evidence: null,
          error: '微信鉴权轮询失败，请重新连接',
          observed_at: observedAt,
        },
      },
    ],
  });
  render(<ChannelsPanel />);
  const connection = within(await screen.findByRole('region', { name: '微信连接状态' }));
  expect(await connection.findByText('微信连接：连接失败')).toBeTruthy();
  expect(connection.getByText('微信鉴权轮询失败，请重新连接')).toBeTruthy();
  expect(connection.getByText('上次清理已结算，保留失败记录：原微信 reader 关闭失败')).toBeTruthy();
  expect(connection.getByText(`观察于 ${new Date(observedAt).toLocaleString()}`)).toBeTruthy();
});

it('Ready仍显示消息准入错误，并只通过原状态轮询清除', async () => {
  const admissionError = '消息尚未接纳；请重试原消息';
  const ready = {
    ...instance,
    id: 'weixin',
    transport: {
      ...instance.transport,
      phase: 'ready',
      evidence: 'weixin_authenticated_poll',
      error: admissionError,
    },
  };
  api.status
    .mockResolvedValueOnce({ configuration_revision: 'desired-new', channels: [ready] })
    .mockResolvedValueOnce({
      configuration_revision: 'desired-new',
      channels: [{ ...ready, transport: { ...ready.transport, error: null } }],
    });

  vi.useFakeTimers();
  let unmount: (() => void) | undefined;
  try {
    await act(async () => {
      ({ unmount } = render(<ChannelsPanel />));
    });
    const connection = within(screen.getByRole('region', { name: '微信连接状态' }));
    expect(connection.getByText('微信连接：微信鉴权轮询已确认')).toBeTruthy();
    expect(connection.getByRole('alert').textContent).toBe(admissionError);
    expect(api.status).toHaveBeenCalledTimes(1);

    await act(async () => {
      await vi.advanceTimersByTimeAsync(2000);
    });

    expect(api.status).toHaveBeenCalledTimes(2);
    expect(connection.getByText('微信连接：微信鉴权轮询已确认')).toBeTruthy();
    expect(connection.queryByText(admissionError)).toBeNull();
    expect(connection.queryByRole('alert')).toBeNull();
    expect(api.control).not.toHaveBeenCalled();
    expect(api.save).not.toHaveBeenCalled();
    expect(qrApi.begin).not.toHaveBeenCalled();
    expect(qrApi.verify).not.toHaveBeenCalled();
    expect(qrApi.cancel).not.toHaveBeenCalled();
    expect(qrApi.forget).not.toHaveBeenCalled();
  } finally {
    unmount?.();
    vi.useRealTimers();
  }
});
