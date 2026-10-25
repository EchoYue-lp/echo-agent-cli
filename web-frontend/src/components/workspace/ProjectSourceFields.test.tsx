// @vitest-environment jsdom
import { cleanup, fireEvent, render, waitFor } from '@testing-library/react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { emptyProjectSource, localProjectSource, ProjectSourceFields } from './ProjectSourceFields';

const api = vi.hoisted(() => ({ connectSsh: vi.fn(), verifySsh: vi.fn(), closeSsh: vi.fn() }));
vi.mock('../../api/endpoints', () => ({ workspaceApi: api }));
vi.mock('../../lib/tauri-bridge', () => ({ isTauri: () => true }));
vi.mock('../terminal/Terminal', async () => {
  const React = await import('react');
  return {
    Terminal: ({
      sessionId,
      createSession,
    }: {
      sessionId: string;
      createSession: (options: { rows: number; cols: number }) => Promise<unknown>;
    }) => {
      React.useEffect(() => {
        void createSession({ rows: 24, cols: 80 }).catch(() => undefined);
      }, [createSession]);
      return <div aria-label="认证终端">{sessionId}</div>;
    },
  };
});

afterEach(cleanup);
beforeEach(() => {
  vi.clearAllMocks();
  api.closeSsh.mockResolvedValue(undefined);
  api.connectSsh.mockResolvedValue({ id: 'auth-id', target: { kind: 'ssh', host: 'example' } });
});

describe('project source admission', () => {
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
    for (const directoryName of ['', '..', '../other', 'a/b', 'a\\b']) {
      expect(() => localProjectSource({ ...draft, kind: 'new_local', directoryName })).toThrow();
    }
  });

  it('offers all three sources and uses the existing local browser action', () => {
    const change = vi.fn();
    const browse = vi.fn();
    const { getByRole } = render(
      <ProjectSourceFields
        draft={emptyProjectSource()}
        onChange={change}
        onBrowse={browse}
        onRemoteSource={vi.fn()}
        busy={false}
      />
    );
    fireEvent.click(getByRole('button', { name: '浏览' }));
    expect(browse).toHaveBeenCalledOnce();
    fireEvent.click(getByRole('button', { name: '新建本地文件夹' }));
    expect(change).toHaveBeenLastCalledWith(expect.objectContaining({ kind: 'new_local' }));
    fireEvent.click(getByRole('button', { name: '远程 SSH 设备' }));
    expect(change).toHaveBeenLastCalledWith(expect.objectContaining({ kind: 'ssh' }));
  });

  it('does not admit SSH after process start, and uses the actual verified remote directory', async () => {
    const onSource = vi.fn();
    api.verifySsh
      .mockRejectedValueOnce(new Error('认证尚未完成'))
      .mockResolvedValueOnce({ path: '/srv/远程项目' });
    const { getByRole } = render(
      <ProjectSourceFields
        draft={{
          ...emptyProjectSource(),
          kind: 'ssh',
          host: 'example',
          user: 'echo',
          remotePath: '~/project',
        }}
        onChange={vi.fn()}
        onBrowse={vi.fn()}
        onRemoteSource={onSource}
        busy={false}
      />
    );
    fireEvent.click(getByRole('button', { name: '连接并验证远程目录' }));
    await waitFor(() => expect(api.connectSsh).toHaveBeenCalledOnce());
    expect(onSource).not.toHaveBeenCalledWith(expect.objectContaining({ kind: 'ssh' }));
    fireEvent.click(getByRole('button', { name: '验证认证与目录' }));
    await waitFor(() => expect(getByRole('alert').textContent).toContain('认证尚未完成'));
    expect(onSource).not.toHaveBeenCalledWith(expect.objectContaining({ kind: 'ssh' }));
    fireEvent.click(getByRole('button', { name: '验证认证与目录' }));
    await waitFor(() =>
      expect(onSource).toHaveBeenCalledWith({
        kind: 'ssh',
        host: 'example',
        user: 'echo',
        port: null,
        path: '/srv/远程项目',
        connection_id: expect.any(String),
      })
    );
    expect(api.verifySsh).toHaveBeenLastCalledWith(
      expect.any(String),
      { kind: 'ssh', host: 'example', user: 'echo', port: null },
      '~/project'
    );
  });

  it('waits for the original auth creation before closing a cancelled draft', async () => {
    let settle: ((session: unknown) => void) | undefined;
    api.connectSsh.mockImplementation(
      () =>
        new Promise((resolve) => {
          settle = resolve;
        })
    );
    const { getByRole, unmount } = render(
      <ProjectSourceFields
        draft={{ ...emptyProjectSource(), kind: 'ssh', host: 'example' }}
        onChange={vi.fn()}
        onBrowse={vi.fn()}
        onRemoteSource={vi.fn()}
        busy={false}
      />
    );
    fireEvent.click(getByRole('button', { name: '连接并验证远程目录' }));
    await waitFor(() => expect(api.connectSsh).toHaveBeenCalledOnce());
    const owner = api.connectSsh.mock.calls.at(0)?.at(0);
    unmount();
    expect(api.closeSsh).not.toHaveBeenCalled();
    settle?.({ id: owner });
    await waitFor(() => expect(api.closeSsh).toHaveBeenCalledExactlyOnceWith(owner));
  });
});
