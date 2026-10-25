// @vitest-environment jsdom
import { cleanup, fireEvent, render, waitFor } from '@testing-library/react';
import { afterEach, describe, expect, it, vi } from 'vitest';
import type { Workspace } from '../../api/endpoints';

const mocks = vi.hoisted(() => ({
  desktop: false,
  connectSsh: vi
    .fn()
    .mockResolvedValue({ id: 'auth', pid: 1, target: { kind: 'ssh', host: 'alias' } }),
  verifySsh: vi.fn().mockResolvedValue({ path: '/srv/project' }),
  closeSsh: vi.fn().mockResolvedValue(undefined),
  workspaceState: {
    workspaces: [
      {
        id: 'workspace-1',
        name: '研究工作区',
        root: '/tmp/research',
        kind: { type: 'research' },
        metadata: { tags: [] },
        product_data_generation: 'workspace-generation',
        created_at: '2026-08-14T00:00:00Z',
        last_active: '2026-08-14T00:00:00Z',
      },
    ] as Workspace[],
    current: null,
    init: vi.fn(),
    switchTo: vi.fn(),
    createAndSwitch: vi.fn(),
    delete: vi.fn(),
  },
}));

vi.mock('../../stores/workspaceStore', () => ({
  useWorkspaceStore: (selector: (state: typeof mocks.workspaceState) => unknown) =>
    selector(mocks.workspaceState),
}));

vi.mock('../../api/endpoints', () => ({
  workspaceApi: {
    defaultRoot: vi.fn().mockResolvedValue({ default_root: '/local/eko/data' }),
    connectSsh: mocks.connectSsh,
    verifySsh: mocks.verifySsh,
    closeSsh: mocks.closeSsh,
  },
}));

vi.mock('../../lib/tauri-bridge', () => ({
  fileSystem: { selectDirectory: vi.fn() },
  isTauri: () => mocks.desktop,
}));

vi.mock('../terminal/Terminal', async () => {
  const React = await import('react');
  return {
    Terminal: ({
      createSession,
    }: {
      createSession?: (options: { rows: number; cols: number }) => Promise<unknown>;
    }) => {
      React.useEffect(() => {
        if (createSession) void createSession({ rows: 24, cols: 80 }).catch(() => undefined);
      }, [createSession]);
      return <div>系统认证终端</div>;
    },
  };
});

vi.mock('../../api/client', () => ({
  get: vi.fn().mockResolvedValue({
    current: '/tmp',
    parent: '/',
    entries: [],
  }),
}));

import DirectoryPicker from './DirectoryPicker';
import NewTaskDialog from './NewTaskDialog';

afterEach(() => {
  cleanup();
  mocks.desktop = false;
  mocks.workspaceState.workspaces = mocks.workspaceState.workspaces.filter(
    (workspace) => workspace.id !== 'remote-project'
  );
});

describe('workspace dialog accessibility', () => {
  it('gives the New Task close and workspace actions distinct button semantics', () => {
    const { getByRole } = render(<NewTaskDialog isOpen onClose={vi.fn()} />);

    expect(getByRole('button', { name: '关闭工作空间' })).toBeTruthy();
    const selectWorkspace = getByRole('button', {
      name: '研究工作区 /tmp/research',
    });
    const deleteWorkspace = getByRole('button', { name: '删除工作空间 研究工作区' });
    expect(selectWorkspace).not.toBe(deleteWorkspace);
    expect(selectWorkspace.contains(deleteWorkspace)).toBe(false);
  });

  it('exposes an accessible name for the Directory Picker close control', () => {
    const { getByRole } = render(<DirectoryPicker isOpen onClose={vi.fn()} onSelect={vi.fn()} />);

    expect(getByRole('button', { name: '关闭文件夹选择器' })).toBeTruthy();
  });

  it('submits an existing project through the same owned creation API with an explicit source', async () => {
    mocks.workspaceState.createAndSwitch.mockResolvedValueOnce({ id: 'created-project' });
    const close = vi.fn();
    const { getByRole, getByLabelText } = render(<NewTaskDialog isOpen onClose={close} />);
    fireEvent.click(getByRole('button', { name: /创建工作空间/ }));
    fireEvent.change(getByLabelText('工作空间名称'), { target: { value: '项目 🦀' } });
    fireEvent.change(getByLabelText('项目文件夹'), { target: { value: '/Volumes/code' } });
    fireEvent.click(getByRole('button', { name: '创建并进入' }));
    await waitFor(() =>
      expect(mocks.workspaceState.createAndSwitch).toHaveBeenCalledWith(
        '项目 🦀',
        'code',
        undefined,
        { kind: 'existing_local', path: '/Volumes/code' }
      )
    );
    expect(close).toHaveBeenCalledOnce();
  });

  it('reconnects a saved remote project through the original switch owner without creating another project', async () => {
    mocks.desktop = true;
    mocks.workspaceState.switchTo
      .mockRejectedValueOnce(new Error('SSH connection is not prepared'))
      .mockResolvedValueOnce(undefined);
    const workspace: Workspace = {
      id: 'remote-project',
      name: '远程项目',
      root: '/local/data',
      kind: { type: 'code' },
      metadata: {
        tags: [],
        project_location: {
          kind: 'ssh',
          host: 'alias',
          user: 'echo',
          port: null,
          path: '/srv/project',
        },
      },
      product_data_generation: 'generation',
      created_at: '',
      last_active: '',
    };
    const before = JSON.stringify(workspace);
    mocks.workspaceState.workspaces.push(workspace);
    const close = vi.fn();
    const view = render(<NewTaskDialog isOpen onClose={close} />);
    fireEvent.click(view.getByRole('button', { name: /远程项目 echo@alias/ }));
    await waitFor(() => expect(view.getByText('连接已有远程项目')).toBeTruthy());
    fireEvent.click(view.getByRole('button', { name: '连接并验证远程目录' }));
    await waitFor(() => expect(mocks.connectSsh).toHaveBeenCalled());
    fireEvent.click(view.getByRole('button', { name: '验证认证与目录' }));
    await waitFor(() => expect(view.getByRole('status')).toBeTruthy());
    fireEvent.click(view.getByRole('button', { name: '连接并进入' }));
    await waitFor(() =>
      expect(mocks.workspaceState.switchTo).toHaveBeenLastCalledWith(
        'remote-project',
        expect.any(String)
      )
    );
    expect(JSON.stringify(workspace)).toBe(before);
    expect(close).toHaveBeenCalledOnce();
  });
});
