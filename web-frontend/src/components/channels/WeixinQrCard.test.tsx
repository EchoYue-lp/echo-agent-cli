// @vitest-environment jsdom
import { afterEach, beforeEach, expect, it, vi } from 'vitest';
import { act, cleanup, fireEvent, render, screen } from '@testing-library/react';
import { WeixinQrCard } from './WeixinQrCard';
import type { WeixinQrStatus } from './WeixinQrCard';

const idle: WeixinQrStatus = {
  phase: 'idle',
  has_credential: false,
  configuration_revision: 'config-1',
};
const svg = '<svg xmlns="http://www.w3.org/2000/svg"><path d="M0 0h1v1z"/></svg>';
const scanning: WeixinQrStatus = {
  ...idle,
  challenge_id: 'challenge-1',
  phase: 'waiting_scan',
  qr_svg: svg,
};
const authorized: WeixinQrStatus = {
  ...idle,
  challenge_id: 'challenge-1',
  phase: 'authorized',
  has_credential: true,
  account_id: 'personal-account',
  configuration_revision: 'config-2',
};

function deferred<T>() {
  let resolve: ((value: T) => void) | undefined;
  const promise = new Promise<T>((complete) => {
    resolve = complete;
  });
  return { promise, resolve: (value: T) => resolve?.(value) };
}

function makeApi() {
  return {
    begin: vi.fn().mockResolvedValue(scanning),
    status: vi.fn().mockResolvedValue(idle),
    verify: vi.fn().mockResolvedValue(authorized),
    cancel: vi.fn().mockResolvedValue({ ...idle, phase: 'cancelled' }),
    forget: vi.fn().mockResolvedValue(undefined),
  };
}

async function settle() {
  await act(async () => {});
}

beforeEach(() => vi.useFakeTimers());
afterEach(() => {
  cleanup();
  vi.useRealTimers();
});

it('renders a local QR and separates authorization from transport readiness', async () => {
  const api = makeApi();
  const onChanged = vi.fn();
  render(<WeixinQrCard api={api} expectedRevision="config-1" onChanged={onChanged} />);
  await settle();
  fireEvent.click(screen.getByRole('button', { name: '获取微信二维码' }));
  await settle();
  expect(api.begin).toHaveBeenCalledWith('config-1');
  expect(screen.getByAltText('微信登录二维码').getAttribute('src')).toBe(
    `data:image/svg+xml;charset=utf-8,${encodeURIComponent(svg)}`
  );
  expect(screen.queryByLabelText(/App ID|密钥/)).toBeNull();
  api.status.mockResolvedValue(authorized);
  await act(async () => vi.advanceTimersByTimeAsync(2000));
  expect(api.status).toHaveBeenLastCalledWith('challenge-1');
  expect(screen.getByText('授权已保存')).toBeTruthy();
  expect(screen.queryByAltText('微信登录二维码')).toBeNull();
  expect(screen.queryByText('在线')).toBeNull();
  expect(onChanged).toHaveBeenCalledTimes(1);
  await act(async () => vi.advanceTimersByTimeAsync(2000));
  expect(onChanged).toHaveBeenCalledTimes(1);
});

it('does not reconnect an already authorized account merely by opening its card', async () => {
  const api = makeApi();
  api.status.mockResolvedValue(authorized);
  const onChanged = vi.fn();
  render(<WeixinQrCard api={api} expectedRevision="config-2" onChanged={onChanged} />);
  await settle();
  await act(async () => vi.advanceTimersByTimeAsync(4000));
  expect(screen.getByText('本机已保存授权')).toBeTruthy();
  expect(onChanged).not.toHaveBeenCalled();
  expect(api.begin).not.toHaveBeenCalled();
});

it('discards old responses and hides the old QR while a refresh is pending', async () => {
  const api = makeApi();
  const onChanged = vi.fn();
  render(<WeixinQrCard api={api} expectedRevision="config-1" onChanged={onChanged} />);
  await settle();
  fireEvent.click(screen.getByRole('button', { name: '获取微信二维码' }));
  await settle();
  const oldPoll = deferred<WeixinQrStatus>();
  api.status.mockReturnValueOnce(oldPoll.promise);
  await act(async () => vi.advanceTimersByTimeAsync(2000));
  const replacement = deferred<WeixinQrStatus>();
  api.begin.mockReturnValueOnce(replacement.promise);
  fireEvent.click(screen.getByRole('button', { name: '刷新微信二维码' }));
  expect(screen.queryByAltText('微信登录二维码')).toBeNull();
  await act(async () => {
    replacement.resolve({ ...scanning, challenge_id: 'challenge-2' });
  });
  api.status.mockResolvedValue({ ...scanning, challenge_id: 'challenge-2' });
  await act(async () => vi.advanceTimersByTimeAsync(2000));
  expect(api.status).toHaveBeenLastCalledWith('challenge-2');
  await act(async () => oldPoll.resolve(authorized));
  expect(screen.getByText('请用微信扫描二维码')).toBeTruthy();
  expect(screen.queryByText('授权已保存')).toBeNull();
  expect(onChanged).not.toHaveBeenCalled();
});

it('submits digits to the current challenge and immediately clears the code', async () => {
  const api = makeApi();
  api.begin.mockResolvedValue({ ...scanning, phase: 'need_verify_code' });
  const verification = deferred<WeixinQrStatus>();
  api.verify.mockReturnValueOnce(verification.promise);
  render(<WeixinQrCard api={api} expectedRevision="config-1" onChanged={vi.fn()} />);
  await settle();
  fireEvent.click(screen.getByRole('button', { name: '获取微信二维码' }));
  await settle();
  const input = screen.getByLabelText('微信数字校验码') as HTMLInputElement;
  fireEvent.change(input, { target: { value: 'not-digits' } });
  fireEvent.click(screen.getByRole('button', { name: '提交数字校验码' }));
  expect(api.verify).not.toHaveBeenCalled();
  expect(screen.getByRole('alert').textContent).toBe('请输入手机上显示的数字校验码。');
  fireEvent.change(input, { target: { value: '123456' } });
  fireEvent.click(screen.getByRole('button', { name: '提交数字校验码' }));
  expect(api.verify).toHaveBeenCalledWith('challenge-1', '123456');
  expect(input.value).toBe('');
  await act(async () => verification.resolve(authorized));
  expect(screen.queryByLabelText('微信数字校验码')).toBeNull();
});

it('does not publish late authorization or keep polling after unmount', async () => {
  const api = makeApi();
  const pending = deferred<WeixinQrStatus>();
  api.begin.mockReturnValueOnce(pending.promise);
  const onChanged = vi.fn();
  const view = render(<WeixinQrCard api={api} expectedRevision="config-1" onChanged={onChanged} />);
  await settle();
  fireEvent.click(screen.getByRole('button', { name: '获取微信二维码' }));
  view.unmount();
  await act(async () => pending.resolve(authorized));
  await act(async () => vi.advanceTimersByTimeAsync(10000));
  expect(api.status).toHaveBeenCalledTimes(1);
  expect(onChanged).not.toHaveBeenCalled();
});

it('cancels the visible challenge and hides raw IPC failure material', async () => {
  const api = makeApi();
  render(<WeixinQrCard api={api} expectedRevision="config-1" onChanged={vi.fn()} />);
  await settle();
  fireEvent.click(screen.getByRole('button', { name: '获取微信二维码' }));
  await settle();
  api.cancel.mockRejectedValue(new Error('bot_token=private&verify_code=654321'));
  fireEvent.click(screen.getByRole('button', { name: '取消微信扫码' }));
  await settle();
  expect(api.cancel).toHaveBeenCalledWith('challenge-1');
  expect(screen.getByRole('alert').textContent).toBe('微信授权操作未确认，请检查当前状态后重试。');
  expect(screen.queryByText(/private|654321/)).toBeNull();
});
