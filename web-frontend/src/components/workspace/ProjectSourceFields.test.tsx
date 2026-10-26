// @vitest-environment jsdom
import { StrictMode } from 'react';
import { act, cleanup, fireEvent, render, waitFor } from '@testing-library/react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { emptyProjectSource, localProjectSource, ProjectSourceFields } from './ProjectSourceFields';
import { decodeTerminalBytes, encodeTerminalBytes } from '../terminal/terminal-io';
const api = vi.hoisted(() => ({
  connectSsh: vi.fn(),
  testSsh: vi.fn(),
  verifySsh: vi.fn(),
  closeSsh: vi.fn(),
  write: vi.fn(),
  handlers: new Map<string, Set<(event: { payload: Record<string, unknown> }) => void>>(),
}));
vi.mock('../../api/endpoints', () => ({ workspaceApi: api, terminalApi: { write: api.write } }));
vi.mock('../../lib/tauri-bridge', () => ({ isTauri: () => true }));
vi.mock('@tauri-apps/api/event', () => ({
  listen: async (name: string, callback: (event: { payload: Record<string, unknown> }) => void) => {
    const handlers = api.handlers.get(name) ?? new Set();
    handlers.add(callback);
    api.handlers.set(name, handlers);
    return () => {
      handlers.delete(callback);
    };
  },
}));
function output(id: string, text: string) {
  act(() => {
    for (const receive of api.handlers.get('terminal-output') ?? [])
      receive({ payload: { id, data: encodeTerminalBytes(new TextEncoder().encode(text)) } });
  });
}
function owner() {
  return api.connectSsh.mock.calls.at(-1)?.at(0) as string;
}
function written() {
  return api.write.mock.calls.map((call) => new TextDecoder().decode(decodeTerminalBytes(call[1])));
}
const target = { kind: 'ssh', host: '111.229.245.175', user: 'ubuntu', port: null };
const sshDraft = {
  ...emptyProjectSource(),
  kind: 'ssh' as const,
  host: target.host,
  user: target.user,
  remotePath: 'ssh-test',
};
function props() {
  return {
    draft: sshDraft,
    onChange: vi.fn(),
    onBrowse: vi.fn(),
    onRemoteSource: vi.fn(),
    busy: false,
  };
}
afterEach(cleanup);
beforeEach(() => {
  vi.clearAllMocks();
  api.handlers.clear();
  api.closeSsh.mockReset().mockResolvedValue(undefined);
  api.connectSsh.mockReset().mockImplementation(async (id, target) => ({ id, pid: 1, target }));
  api.testSsh.mockReset().mockResolvedValue({ authenticated: false });
  api.verifySsh.mockReset();
  api.write.mockReset().mockResolvedValue({ success: true });
});
describe('project source admission', () => {
  it('offers a masked password and a connection-only test without an authentication terminal', () => {
    const view = render(<ProjectSourceFields {...props()} />);
    expect(view.getByLabelText('SSH 密码').getAttribute('type')).toBe('password');
    expect(view.getByRole('button', { name: '测试连接' })).toBeTruthy();
    expect(view.queryByLabelText('终端')).toBeNull();
  });
  it('keeps existing folder selection distinct from new folder creation', () => {
    const draft = {
      ...emptyProjectSource(),
      localPath: '/Volumes/项目 🦀',
      directoryName: '新项目',
    };
    expect(localProjectSource(draft)).toEqual({ kind: 'existing_local', path: '/Volumes/项目 🦀' });
    expect(localProjectSource({ ...draft, kind: 'new_local' })).toEqual({
      kind: 'new_local',
      parent: '/Volumes/项目 🦀',
      directory_name: '新项目',
    });
    for (const directoryName of ['', '..', '../other', 'a/b', 'a\\b'])
      expect(() => localProjectSource({ ...draft, kind: 'new_local', directoryName })).toThrow();
  });
  it('offers all three sources and uses the existing local browser action', () => {
    const p = { ...props(), draft: emptyProjectSource() };
    const view = render(<ProjectSourceFields {...p} />);
    fireEvent.click(view.getByRole('button', { name: '浏览' }));
    expect(p.onBrowse).toHaveBeenCalledOnce();
    fireEvent.click(view.getByRole('button', { name: '新建本地文件夹' }));
    expect(p.onChange).toHaveBeenLastCalledWith(expect.objectContaining({ kind: 'new_local' }));
    fireEvent.click(view.getByRole('button', { name: '远程 SSH 设备' }));
    expect(p.onChange).toHaveBeenLastCalledWith(expect.objectContaining({ kind: 'ssh' }));
  });
  it('writes one password only after the original prompt, clears it, and tests no directory', async () => {
    const p = props();
    const view = render(<ProjectSourceFields {...p} />);
    fireEvent.change(view.getByLabelText('SSH 密码'), { target: { value: 'public-probe-42' } });
    fireEvent.click(view.getByRole('button', { name: '测试连接' }));
    await waitFor(() => expect(api.connectSsh).toHaveBeenCalledOnce());
    expect(api.write).not.toHaveBeenCalled();
    expect(p.onRemoteSource).not.toHaveBeenCalledWith(expect.objectContaining({ kind: 'ssh' }));
    output('unrelated-owner', 'password: ');
    expect(api.write).not.toHaveBeenCalled();
    api.write.mockImplementation(async () => {
      api.testSsh.mockResolvedValue({ authenticated: true });
      return { success: true };
    });
    output(owner(), "ubuntu@host's pass");
    output(owner(), 'word: ');
    await waitFor(() => expect(written()).toEqual(['public-probe-42\r']));
    expect((view.getByLabelText('SSH 密码') as HTMLInputElement).value).toBe('');
    await waitFor(() =>
      expect(p.onRemoteSource).toHaveBeenCalledWith({
        ...target,
        path: 'ssh-test',
        connection_id: owner(),
      })
    );
    expect(api.verifySsh).not.toHaveBeenCalled();
    expect(api.testSsh).toHaveBeenLastCalledWith(owner(), target);
    expect(api.connectSsh).toHaveBeenCalledWith(owner(), target, 24, 80);
  });
  it('allows key authentication without a folder, then changes only directory on the same owner', async () => {
    api.testSsh.mockResolvedValue({ authenticated: true });
    const p = { ...props(), draft: { ...sshDraft, remotePath: '' } };
    const view = render(<ProjectSourceFields {...p} />);
    fireEvent.click(view.getByRole('button', { name: '测试连接' }));
    await waitFor(() =>
      expect(p.onRemoteSource).toHaveBeenCalledWith({ ...target, path: '', connection_id: owner() })
    );
    const id = owner();
    view.rerender(<ProjectSourceFields {...p} draft={{ ...sshDraft, remotePath: 'ssh-test' }} />);
    await waitFor(() =>
      expect(p.onRemoteSource).toHaveBeenLastCalledWith({
        ...target,
        path: 'ssh-test',
        connection_id: id,
      })
    );
    expect(api.connectSsh).toHaveBeenCalledOnce();
    expect(api.write).not.toHaveBeenCalled();
    expect(api.verifySsh).not.toHaveBeenCalled();
    expect((view.getByLabelText('远程项目文件夹') as HTMLInputElement).disabled).toBe(false);
  });

  it('clears unused credentials and locks password edits until the authenticated owner is disconnected', async () => {
    api.testSsh.mockResolvedValue({ authenticated: true });
    const view = render(<ProjectSourceFields {...props()} />);
    fireEvent.change(view.getByLabelText('SSH 密码'), { target: { value: 'public-unused-probe' } });
    fireEvent.click(view.getByRole('button', { name: '测试连接' }));
    await waitFor(() =>
      expect(view.getByText('SSH 登录已验证；创建时再验证远程目录。')).toBeTruthy()
    );
    const password = view.getByLabelText('SSH 密码') as HTMLInputElement;
    expect(password.value).toBe('');
    expect(password.disabled).toBe(true);
    expect(api.write).not.toHaveBeenCalled();
    expect(view.getByText('已登录；如需更换密码，请先断开并重新测试连接。')).toBeTruthy();
    fireEvent.click(view.getByRole('button', { name: '断开并修改连接' }));
    expect(password.disabled).toBe(false);
    fireEvent.change(password, { target: { value: 'new-public-probe' } });
    expect(password.value).toBe('new-public-probe');
  });
  it('requires explicit host fingerprint confirmation before writing a password', async () => {
    const view = render(<ProjectSourceFields {...props()} />);
    fireEvent.change(view.getByLabelText('SSH 密码'), { target: { value: 'public-probe-42' } });
    fireEvent.click(view.getByRole('button', { name: '测试连接' }));
    await waitFor(() => expect(api.connectSsh).toHaveBeenCalledOnce());
    output(
      owner(),
      'ED25519 fingerprint SHA256:public-fixture\nAre you sure you want to continue connecting (yes/no/[fingerprint])? '
    );
    await waitFor(() =>
      expect(
        (view.getByRole('button', { name: '核对指纹并信任此主机' }) as HTMLButtonElement).disabled
      ).toBe(false)
    );
    expect(api.write).not.toHaveBeenCalled();
    api.write.mockImplementation(async (_id, data) => {
      if (new TextDecoder().decode(decodeTerminalBytes(data)) === 'yes\r')
        output(owner(), "ubuntu@host's password: ");
      else api.testSsh.mockResolvedValue({ authenticated: true });
      return { success: true };
    });
    fireEvent.click(view.getByRole('button', { name: '核对指纹并信任此主机' }));
    await waitFor(() => expect(written()).toEqual(['yes\r', 'public-probe-42\r']));
  });
  it('does not replay a password after an uncertain write failure', async () => {
    const view = render(<ProjectSourceFields {...props()} />);
    fireEvent.change(view.getByLabelText('SSH 密码'), { target: { value: 'public-probe-42' } });
    fireEvent.click(view.getByRole('button', { name: '测试连接' }));
    await waitFor(() => expect(api.connectSsh).toHaveBeenCalledOnce());
    api.write.mockRejectedValue(new Error('原写入未确认'));
    output(owner(), "ubuntu@host's password: ");
    await waitFor(() => expect(view.getByRole('alert').textContent).toContain('原写入未确认'));
    expect(written()).toEqual(['public-probe-42\r']);
    expect((view.getByLabelText('SSH 密码') as HTMLInputElement).value).toBe('');
    fireEvent.click(view.getByRole('button', { name: '测试连接' }));
    await waitFor(() => expect(view.getByText(/SSH 正在等待密码/)).toBeTruthy());
    expect(api.write).toHaveBeenCalledOnce();
  });
  it('waits for original creation before closing a cancelled owner and ignores late success', async () => {
    let settle: ((session: unknown) => void) | undefined;
    api.connectSsh.mockImplementation(
      () =>
        new Promise((resolve) => {
          settle = resolve;
        })
    );
    const p = props();
    const view = render(<ProjectSourceFields {...p} />);
    fireEvent.click(view.getByRole('button', { name: '测试连接' }));
    await waitFor(() => expect(api.connectSsh).toHaveBeenCalledOnce());
    const id = owner();
    view.unmount();
    expect(api.closeSsh).not.toHaveBeenCalled();
    settle?.({ id });
    await waitFor(() => expect(api.closeSsh).toHaveBeenCalledExactlyOnceWith(id));
    expect(api.testSsh).not.toHaveBeenCalled();
    expect(p.onRemoteSource).not.toHaveBeenCalledWith(expect.objectContaining({ kind: 'ssh' }));
  });
  it('does not leak subscriptions or close the live owner during StrictMode remount', async () => {
    api.testSsh.mockResolvedValue({ authenticated: true });
    const p = props();
    const view = render(
      <StrictMode>
        <ProjectSourceFields {...p} />
      </StrictMode>
    );
    fireEvent.click(view.getByRole('button', { name: '测试连接' }));
    await waitFor(() =>
      expect(p.onRemoteSource).toHaveBeenCalledWith(expect.objectContaining({ kind: 'ssh' }))
    );
    expect(api.connectSsh).toHaveBeenCalledOnce();
    expect(api.closeSsh).not.toHaveBeenCalled();
    expect(Array.from(api.handlers.values()).every((handlers) => handlers.size === 1)).toBe(true);
    view.unmount();
    await waitFor(() => expect(api.closeSsh).toHaveBeenCalledOnce());
    expect(Array.from(api.handlers.values()).every((handlers) => handlers.size === 0)).toBe(true);
  });
});
