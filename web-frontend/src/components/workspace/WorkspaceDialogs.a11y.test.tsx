// @vitest-environment jsdom
import { act, cleanup, fireEvent, render, waitFor } from '@testing-library/react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import type { Workspace } from '../../api/endpoints';

const mocks = vi.hoisted(() => ({
  desktop: false,
  connectSsh: vi
    .fn()
    .mockResolvedValue({ id: 'auth', pid: 1, target: { kind: 'ssh', host: 'alias' } }),
  verifySsh: vi.fn().mockResolvedValue({ path: '/srv/project' }),
  testSsh: vi.fn().mockResolvedValue({ authenticated: true }),
  closeSsh: vi.fn().mockResolvedValue(undefined),
  workspaceState: {
    workspaces: [
      {
        id: 'workspace-1',
        name: '研究工作区',
        root: '/tmp/research',
        kind: { type: 'research' },
        metadata: { tags: [], project_location: { kind: 'local' as const, path: '/tmp/research' } },
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
    testSsh: mocks.testSsh,
    closeSsh: mocks.closeSsh,
  },
}));

vi.mock('../../lib/tauri-bridge', () => ({
  fileSystem: { selectDirectory: vi.fn() },
  isTauri: () => mocks.desktop,
}));

vi.mock('@tauri-apps/api/event', () => ({ listen: vi.fn().mockResolvedValue(() => {}) }));

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

beforeEach(() => {
  vi.clearAllMocks();
  mocks.connectSsh
    .mockReset()
    .mockResolvedValue({ id: 'auth', pid: 1, target: { kind: 'ssh', host: 'alias' } });
  mocks.testSsh.mockReset().mockResolvedValue({ authenticated: true });
  mocks.verifySsh.mockReset().mockResolvedValue({ path: '/srv/project' });
  mocks.closeSsh.mockReset().mockResolvedValue(undefined);
  mocks.workspaceState.createAndSwitch.mockReset();
  mocks.workspaceState.switchTo.mockReset();
});

afterEach(() => {
  cleanup();
  vi.useRealTimers();
  mocks.desktop = false;
  mocks.workspaceState.workspaces = mocks.workspaceState.workspaces.filter(
    (workspace) => workspace.id !== 'remote-project'
  );
});

describe('workspace dialog accessibility', () => {
  it('retires authenticated draft ownership when cancelling and reopening creation in the same dialog', async () => {
    mocks.desktop = true;
    const view = render(<NewTaskDialog isOpen onClose={vi.fn()} initialSource="ssh" />);
    fireEvent.change(view.getByLabelText('工作空间名称'), { target: { value: '取消后再创建' } });
    fireEvent.change(view.getByLabelText('主机或 SSH 别名'), {
      target: { value: 'example.invalid' },
    });
    fireEvent.change(view.getByLabelText('远程项目文件夹'), { target: { value: 'ssh-test' } });
    fireEvent.click(view.getByRole('button', { name: '测试连接' }));
    await waitFor(() =>
      expect((view.getByRole('button', { name: '创建并进入' }) as HTMLButtonElement).disabled).toBe(
        false
      )
    );
    fireEvent.click(view.getByRole('button', { name: '取消' }));
    fireEvent.click(view.getByRole('button', { name: /创建工作空间/ }));
    expect(view.getByText('请先测试 SSH 连接，登录成功后才可创建。')).toBeTruthy();
    expect((view.getByRole('button', { name: '创建并进入' }) as HTMLButtonElement).disabled).toBe(
      true
    );
    expect(mocks.verifySsh).not.toHaveBeenCalled();
    expect(mocks.workspaceState.createAndSwitch).not.toHaveBeenCalled();
    await waitFor(() => expect(mocks.closeSsh).toHaveBeenCalledOnce());
  });

  it('bounds a pending directory observation and never creates from its late result after cancellation', async () => {
    mocks.desktop = true;
    mocks.workspaceState.createAndSwitch.mockClear();
    mocks.verifySsh.mockClear();
    let complete: ((value: { path: string }) => void) | undefined;
    mocks.verifySsh.mockReturnValueOnce(
      new Promise((resolve) => {
        complete = resolve;
      })
    );
    const view = render(<NewTaskDialog isOpen onClose={vi.fn()} initialSource="ssh" />);
    fireEvent.change(view.getByLabelText('工作空间名称'), { target: { value: '验证未确认保留' } });
    fireEvent.change(view.getByLabelText('主机或 SSH 别名'), {
      target: { value: 'example.invalid' },
    });
    fireEvent.change(view.getByLabelText('远程项目文件夹'), { target: { value: 'ssh-test' } });
    fireEvent.click(view.getByRole('button', { name: '测试连接' }));
    await waitFor(() =>
      expect(view.getByText('SSH 登录已验证；创建时再验证远程目录。')).toBeTruthy()
    );
    await waitFor(() =>
      expect((view.getByRole('button', { name: '创建并进入' }) as HTMLButtonElement).disabled).toBe(
        false
      )
    );
    await waitFor(() =>
      expect((view.getByRole('button', { name: '创建并进入' }) as HTMLButtonElement).disabled).toBe(
        false
      )
    );
    vi.useFakeTimers();
    fireEvent.click(view.getByRole('button', { name: '创建并进入' }));
    expect(view.getByText('正在验证远程目录…')).toBeTruthy();
    await act(async () => {
      await vi.advanceTimersByTimeAsync(30_000);
    });
    expect(view.getByText(/远程目录验证尚未确认/)).toBeTruthy();
    expect((view.getByRole('button', { name: '取消' }) as HTMLButtonElement).disabled).toBe(false);
    expect(mocks.workspaceState.createAndSwitch).not.toHaveBeenCalled();
    fireEvent.click(view.getByRole('button', { name: '取消' }));
    await act(async () => {
      complete?.({ path: '/home/ubuntu/ssh-test' });
    });
    expect(mocks.workspaceState.createAndSwitch).not.toHaveBeenCalled();
    expect(mocks.verifySsh).toHaveBeenCalledOnce();
  });

  it('re-observes the same pending verification on explicit retry without dispatching a second request', async () => {
    mocks.desktop = true;
    mocks.workspaceState.createAndSwitch.mockClear().mockResolvedValue({ id: 'remote-project' });
    mocks.verifySsh.mockClear();
    let complete: ((value: { path: string }) => void) | undefined;
    mocks.verifySsh.mockReturnValueOnce(
      new Promise((resolve) => {
        complete = resolve;
      })
    );
    const view = render(<NewTaskDialog isOpen onClose={vi.fn()} initialSource="ssh" />);
    fireEvent.change(view.getByLabelText('工作空间名称'), { target: { value: '显式重试观察' } });
    fireEvent.change(view.getByLabelText('主机或 SSH 别名'), {
      target: { value: 'example.invalid' },
    });
    fireEvent.change(view.getByLabelText('远程项目文件夹'), { target: { value: 'ssh-test' } });
    fireEvent.click(view.getByRole('button', { name: '测试连接' }));
    await waitFor(() =>
      expect(view.getByText('SSH 登录已验证；创建时再验证远程目录。')).toBeTruthy()
    );
    await waitFor(() =>
      expect((view.getByRole('button', { name: '创建并进入' }) as HTMLButtonElement).disabled).toBe(
        false
      )
    );
    await waitFor(() =>
      expect((view.getByRole('button', { name: '创建并进入' }) as HTMLButtonElement).disabled).toBe(
        false
      )
    );
    vi.useFakeTimers();
    fireEvent.click(view.getByRole('button', { name: '创建并进入' }));
    await act(async () => {
      await vi.advanceTimersByTimeAsync(30_000);
    });
    fireEvent.click(view.getByRole('button', { name: '创建并进入' }));
    expect(mocks.verifySsh).toHaveBeenCalledOnce();
    await act(async () => {
      complete?.({ path: '/home/ubuntu/ssh-test' });
    });
    expect(mocks.workspaceState.createAndSwitch).toHaveBeenCalledOnce();
    expect(mocks.verifySsh).toHaveBeenCalledOnce();
  });

  it.each(['composition', 'webkit'] as const)(
    'does not create a workspace while %s IME Enter confirms its name',
    async (mode) => {
      mocks.workspaceState.createAndSwitch.mockClear();
      mocks.workspaceState.createAndSwitch.mockResolvedValue({ id: 'created-project' });
      const close = vi.fn();
      const view = render(<NewTaskDialog isOpen onClose={close} initialSource="existing_local" />);
      const name = view.getByLabelText('工作空间名称');
      fireEvent.change(name, { target: { value: '仍在选词的项目名称' } });
      fireEvent.change(view.getByLabelText('项目文件夹'), { target: { value: '/Volumes/code' } });
      fireEvent.keyDown(name, {
        key: 'Enter',
        isComposing: mode === 'composition',
        keyCode: mode === 'webkit' ? 229 : 13,
      });
      expect(mocks.workspaceState.createAndSwitch).not.toHaveBeenCalled();
      expect(close).not.toHaveBeenCalled();
      fireEvent.keyDown(name, { key: 'Enter', isComposing: false, keyCode: 13 });
      await waitFor(() =>
        expect(mocks.workspaceState.createAndSwitch).toHaveBeenCalledExactlyOnceWith(
          '仍在选词的项目名称',
          'code',
          undefined,
          { kind: 'existing_local', path: '/Volumes/code' }
        )
      );
      expect(close).toHaveBeenCalledOnce();
    }
  );

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

  it('verifies the directory only on creation and preserves the original connection and canonical path', async () => {
    mocks.desktop = true;
    mocks.connectSsh.mockClear();
    mocks.verifySsh.mockClear();
    mocks.testSsh.mockClear();
    mocks.workspaceState.createAndSwitch.mockClear();
    mocks.workspaceState.createAndSwitch.mockResolvedValueOnce({ id: 'remote-project' });
    const close = vi.fn();
    const view = render(<NewTaskDialog isOpen onClose={close} initialSource="ssh" />);
    fireEvent.change(view.getByLabelText('工作空间名称'), {
      target: { value: '远程项目 🦀' },
    });
    fireEvent.change(view.getByLabelText('主机或 SSH 别名'), {
      target: { value: 'alias' },
    });
    fireEvent.change(view.getByLabelText('用户名（可选）'), {
      target: { value: 'ubuntu' },
    });
    fireEvent.change(view.getByLabelText('远程项目文件夹'), {
      target: { value: '~/project' },
    });
    fireEvent.click(view.getByRole('button', { name: '测试连接' }));
    await waitFor(() => expect(mocks.connectSsh).toHaveBeenCalledOnce());
    await waitFor(() =>
      expect(view.getByText('SSH 登录已验证；创建时再验证远程目录。')).toBeTruthy()
    );
    await waitFor(() =>
      expect((view.getByRole('button', { name: '创建并进入' }) as HTMLButtonElement).disabled).toBe(
        false
      )
    );
    expect(mocks.verifySsh).not.toHaveBeenCalled();
    fireEvent.click(view.getByRole('button', { name: '创建并进入' }));

    await waitFor(() =>
      expect(mocks.workspaceState.createAndSwitch).toHaveBeenCalledWith(
        '远程项目 🦀',
        'code',
        undefined,
        {
          kind: 'ssh',
          host: 'alias',
          user: 'ubuntu',
          port: null,
          path: '/srv/project',
          connection_id: expect.any(String),
        }
      )
    );
    expect(close).toHaveBeenCalledOnce();
    expect(mocks.verifySsh).toHaveBeenCalledWith(
      expect.any(String),
      {
        kind: 'ssh',
        host: 'alias',
        user: 'ubuntu',
        port: null,
      },
      '~/project'
    );
  });

  it('keeps name and directory after a failed verification and creates only after a successful retry', async () => {
    mocks.desktop = true;
    mocks.connectSsh.mockClear();
    mocks.workspaceState.createAndSwitch.mockClear();
    mocks.workspaceState.createAndSwitch.mockResolvedValue({ id: 'remote-project' });
    mocks.verifySsh
      .mockClear()
      .mockRejectedValueOnce(new Error('远程目录不存在'))
      .mockResolvedValueOnce({ path: '/home/ubuntu/ssh-test' });
    const close = vi.fn();
    const view = render(<NewTaskDialog isOpen onClose={close} initialSource="ssh" />);
    fireEvent.change(view.getByLabelText('工作空间名称'), { target: { value: '保留项目名称' } });
    fireEvent.change(view.getByLabelText('主机或 SSH 别名'), {
      target: { value: '111.229.245.175' },
    });
    fireEvent.change(view.getByLabelText('用户名（可选）'), { target: { value: 'ubuntu' } });
    fireEvent.change(view.getByLabelText('远程项目文件夹'), {
      target: { value: 'missing-project' },
    });
    fireEvent.click(view.getByRole('button', { name: '测试连接' }));
    await waitFor(() =>
      expect(view.getByText('SSH 登录已验证；创建时再验证远程目录。')).toBeTruthy()
    );
    await waitFor(() =>
      expect((view.getByRole('button', { name: '创建并进入' }) as HTMLButtonElement).disabled).toBe(
        false
      )
    );
    expect(mocks.verifySsh).not.toHaveBeenCalled();
    fireEvent.click(view.getByRole('button', { name: '创建并进入' }));
    await waitFor(() => expect(view.getByText('远程目录不存在')).toBeTruthy());
    expect(mocks.workspaceState.createAndSwitch).not.toHaveBeenCalled();
    expect(close).not.toHaveBeenCalled();
    expect((view.getByLabelText('工作空间名称') as HTMLInputElement).value).toBe('保留项目名称');
    expect((view.getByLabelText('远程项目文件夹') as HTMLInputElement).value).toBe(
      'missing-project'
    );
    fireEvent.change(view.getByLabelText('远程项目文件夹'), { target: { value: 'ssh-test' } });
    await waitFor(() =>
      expect((view.getByRole('button', { name: '创建并进入' }) as HTMLButtonElement).disabled).toBe(
        false
      )
    );
    fireEvent.click(view.getByRole('button', { name: '创建并进入' }));
    await waitFor(() =>
      expect(mocks.workspaceState.createAndSwitch).toHaveBeenCalledWith(
        '保留项目名称',
        'code',
        undefined,
        {
          kind: 'ssh',
          host: '111.229.245.175',
          user: 'ubuntu',
          port: null,
          path: '/home/ubuntu/ssh-test',
          connection_id: expect.any(String),
        }
      )
    );
    expect(mocks.connectSsh).toHaveBeenCalledOnce();
    expect(close).toHaveBeenCalledOnce();
  });

  it('reconnects a saved remote project through the original switch owner without creating another project', async () => {
    mocks.desktop = true;
    mocks.verifySsh.mockClear();
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
          user: 'ubuntu',
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
    fireEvent.click(view.getByRole('button', { name: /远程项目 ubuntu@alias/ }));
    await waitFor(() => expect(view.getByText('连接已有远程项目')).toBeTruthy());
    fireEvent.click(view.getByRole('button', { name: '测试连接' }));
    await waitFor(() => expect(mocks.connectSsh).toHaveBeenCalled());
    await waitFor(() =>
      expect(view.getByText('SSH 登录已验证；创建时再验证远程目录。')).toBeTruthy()
    );
    await waitFor(() =>
      expect((view.getByRole('button', { name: '连接并进入' }) as HTMLButtonElement).disabled).toBe(
        false
      )
    );
    expect(mocks.verifySsh).not.toHaveBeenCalled();
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
