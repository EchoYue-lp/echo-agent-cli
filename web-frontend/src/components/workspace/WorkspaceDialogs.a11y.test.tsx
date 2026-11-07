// @vitest-environment jsdom
import { act, cleanup, fireEvent, render, waitFor } from '@testing-library/react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import type { Workspace } from '../../api/endpoints';

const mocks = vi.hoisted(() => ({
  desktop: false,
  defaultRoot: vi.fn().mockResolvedValue({ default_root: '/local/eko/data' }),
  ui: {
    workspaceDialogReconnect: null as { workspaceId: string; workspaceGeneration: string } | null,
  },
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

vi.mock('../../stores/uiStore', () => ({
  useUiStore: (select: (s: typeof mocks.ui) => unknown) => select(mocks.ui),
}));
vi.mock('@tauri-apps/api/path', () => ({
  dirname: async (p: string) => p.slice(0, p.lastIndexOf('/')),
  join: async (...parts: string[]) => parts.join('/'),
}));

vi.mock('../../stores/workspaceStore', () => ({
  useWorkspaceStore: (selector: (state: typeof mocks.workspaceState) => unknown) =>
    selector(mocks.workspaceState),
}));

vi.mock('../../api/endpoints', () => ({
  workspaceApi: {
    defaultRoot: mocks.defaultRoot,
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
  mocks.ui.workspaceDialogReconnect = null;
  mocks.defaultRoot
    .mockReset()
    .mockImplementation(async (name: string) => ({ default_root: `/local/eko/${name}` }));
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
  const savedRemote = (): Workspace => ({
    id: 'remote-project',
    name: '远程项目',
    root: '/local/metadata',
    kind: { type: 'code' },
    metadata: {
      tags: [],
      project_location: {
        kind: 'ssh',
        host: '111.229.245.175',
        user: 'ubuntu',
        port: 22,
        path: '/home/ubuntu/ssh-test',
      },
    },
    product_data_generation: 'original-generation',
    created_at: '',
    last_active: '',
  });
  it('opens the existing reconnect form from the sidebar intent without switching or creating', async () => {
    mocks.desktop = true;
    mocks.workspaceState.workspaces.push(savedRemote());
    mocks.ui.workspaceDialogReconnect = {
      workspaceId: 'remote-project',
      workspaceGeneration: 'original-generation',
    };
    const view = render(<NewTaskDialog isOpen onClose={vi.fn()} initialSource="ssh" />);
    expect(view.getByText('连接已有远程项目')).toBeTruthy();
    expect((view.getByLabelText('用户名（可选）') as HTMLInputElement).value).toBe('ubuntu');
    fireEvent.click(view.getByRole('button', { name: '测试连接' }));
    await waitFor(() =>
      expect(view.getByText('SSH 登录已验证；创建时再验证远程目录。')).toBeTruthy()
    );
    expect(mocks.workspaceState.switchTo).not.toHaveBeenCalled();
    await waitFor(() =>
      expect((view.getByRole('button', { name: '连接并进入' }) as HTMLButtonElement).disabled).toBe(
        false
      )
    );
    fireEvent.click(view.getByRole('button', { name: '连接并进入' }));
    await waitFor(() =>
      expect(mocks.workspaceState.switchTo).toHaveBeenCalledWith(
        'remote-project',
        expect.any(String),
        'original-generation'
      )
    );
    expect(mocks.verifySsh).toHaveBeenCalledWith(
      expect.any(String),
      { kind: 'ssh', host: '111.229.245.175', user: 'ubuntu', port: 22 },
      '/home/ubuntu/ssh-test'
    );
    expect(mocks.workspaceState.createAndSwitch).not.toHaveBeenCalled();
  });
  it.each(['cancel', 'generation'] as const)(
    'does not switch on late directory verification after %s',
    async (change) => {
      mocks.desktop = true;
      const remote = savedRemote();
      mocks.workspaceState.workspaces.push(remote);
      mocks.ui.workspaceDialogReconnect = {
        workspaceId: remote.id,
        workspaceGeneration: remote.product_data_generation,
      };
      let done: (x: { path: string }) => void = () => {};
      mocks.verifySsh.mockImplementationOnce(
        () =>
          new Promise((resolve) => {
            done = resolve;
          })
      );
      const close = vi.fn();
      const view = render(<NewTaskDialog isOpen onClose={close} initialSource="ssh" />);
      fireEvent.click(view.getByRole('button', { name: '测试连接' }));
      await waitFor(() =>
        expect(view.getByText('SSH 登录已验证；创建时再验证远程目录。')).toBeTruthy()
      );
      await waitFor(() =>
        expect(
          (view.getByRole('button', { name: '连接并进入' }) as HTMLButtonElement).disabled
        ).toBe(false)
      );
      fireEvent.click(view.getByRole('button', { name: '连接并进入' }));
      await waitFor(() => expect(mocks.verifySsh).toHaveBeenCalled());
      if (change === 'cancel')
        view.rerender(<NewTaskDialog isOpen={false} onClose={close} initialSource="ssh" />);
      else {
        mocks.workspaceState.workspaces = mocks.workspaceState.workspaces.map((x) =>
          x.id === remote.id ? { ...x, product_data_generation: 'replacement' } : x
        );
        view.rerender(<NewTaskDialog isOpen onClose={close} initialSource="ssh" />);
      }
      await act(async () => done({ path: '/home/ubuntu/ssh-test' }));
      expect(mocks.workspaceState.switchTo).not.toHaveBeenCalled();
      expect(mocks.workspaceState.createAndSwitch).not.toHaveBeenCalled();
      expect(close).not.toHaveBeenCalled();
      if (change === 'generation')
        expect(view.getByText('工作空间已变化，请重新选择后连接。')).toBeTruthy();
    }
  );
  it('rejects a stale dialog intent before authenticating a replacement workspace', () => {
    mocks.workspaceState.workspaces.push(savedRemote());
    mocks.ui.workspaceDialogReconnect = {
      workspaceId: 'remote-project',
      workspaceGeneration: 'replaced-generation',
    };
    const view = render(<NewTaskDialog isOpen onClose={vi.fn()} initialSource="ssh" />);
    expect(view.getByText('工作空间已变化，请重新选择后连接。')).toBeTruthy();
    expect(mocks.connectSsh).not.toHaveBeenCalled();
    expect(mocks.workspaceState.switchTo).not.toHaveBeenCalled();
  });

  it('creates a new local project through the default constructor when parent and folder name are blank', async () => {
    mocks.workspaceState.createAndSwitch.mockResolvedValue({ id: 'Test' });
    const close = vi.fn();
    const view = render(<NewTaskDialog isOpen onClose={close} initialSource="new_local" />);
    fireEvent.change(view.getByLabelText('工作空间名称'), { target: { value: 'Test' } });
    await waitFor(() => expect(view.getByText('/local/eko/Test')).toBeTruthy());
    fireEvent.click(view.getByRole('button', { name: '创建并进入' }));
    await waitFor(() =>
      expect(mocks.workspaceState.createAndSwitch).toHaveBeenCalledWith(
        'Test',
        'code',
        undefined,
        undefined
      )
    );
    expect(close).toHaveBeenCalledOnce();
  });
  it('rejects an explicitly whitespace-only directory instead of using the default name', async () => {
    const view = render(<NewTaskDialog isOpen onClose={vi.fn()} initialSource="new_local" />);
    fireEvent.change(view.getByLabelText('工作空间名称'), { target: { value: 'Test' } });
    fireEvent.change(view.getByLabelText('新文件夹名称'), { target: { value: '   ' } });
    fireEvent.click(view.getByRole('button', { name: '创建并进入' }));
    await waitFor(() =>
      expect(view.getByText('请输入一个新的文件夹名称，不要包含路径分隔符')).toBeTruthy()
    );
    expect(mocks.workspaceState.createAndSwitch).not.toHaveBeenCalled();
    expect((view.getByLabelText('新文件夹名称') as HTMLInputElement).value).toBe('   ');
  });
  it('keeps an explicit unicode directory name below the backend default parent', async () => {
    mocks.workspaceState.createAndSwitch.mockResolvedValue({ id: 'Test' });
    const view = render(<NewTaskDialog isOpen onClose={vi.fn()} initialSource="new_local" />);
    fireEvent.change(view.getByLabelText('工作空间名称'), { target: { value: 'Test' } });
    fireEvent.change(view.getByLabelText('新文件夹名称'), { target: { value: '项目 🦀' } });
    fireEvent.click(view.getByRole('button', { name: '创建并进入' }));
    await waitFor(() =>
      expect(mocks.workspaceState.createAndSwitch).toHaveBeenCalledWith('Test', 'code', undefined, {
        kind: 'new_local',
        parent: '',
        directory_name: '项目 🦀',
      })
    );
  });
  it('uses the workspace name for a blank directory name under an explicit parent', async () => {
    mocks.workspaceState.createAndSwitch.mockResolvedValue({ id: 'named' });
    const view = render(<NewTaskDialog isOpen onClose={vi.fn()} initialSource="new_local" />);
    fireEvent.change(view.getByLabelText('工作空间名称'), { target: { value: '项目 🦀' } });
    fireEvent.change(view.getByLabelText('父文件夹'), { target: { value: '/custom/projects' } });
    fireEvent.click(view.getByRole('button', { name: '创建并进入' }));
    await waitFor(() =>
      expect(mocks.workspaceState.createAndSwitch).toHaveBeenCalledWith(
        '项目 🦀',
        'code',
        undefined,
        { kind: 'new_local', parent: '/custom/projects', directory_name: '项目 🦀' }
      )
    );
  });
  it('does not use the default directory for an existing-local selection', async () => {
    const view = render(<NewTaskDialog isOpen onClose={vi.fn()} initialSource="existing_local" />);
    fireEvent.change(view.getByLabelText('工作空间名称'), { target: { value: 'Test' } });
    fireEvent.click(view.getByRole('button', { name: '创建并进入' }));
    await waitFor(() => expect(view.getByText('请选择本地文件夹')).toBeTruthy());
    expect(mocks.workspaceState.createAndSwitch).not.toHaveBeenCalled();
  });
  it('retains the draft and backend collision error without closing or retrying', async () => {
    mocks.workspaceState.createAndSwitch.mockRejectedValue(new Error('目录已存在，未覆盖'));
    const close = vi.fn();
    const view = render(<NewTaskDialog isOpen onClose={close} initialSource="new_local" />);
    fireEvent.change(view.getByLabelText('工作空间名称'), { target: { value: 'Test' } });
    fireEvent.click(view.getByRole('button', { name: '创建并进入' }));
    await waitFor(() => expect(view.getByText('目录已存在，未覆盖')).toBeTruthy());
    expect((view.getByLabelText('工作空间名称') as HTMLInputElement).value).toBe('Test');
    expect(close).not.toHaveBeenCalled();
    expect(mocks.workspaceState.createAndSwitch).toHaveBeenCalledOnce();
  });
  it('ignores a late default path after changing name, manual parent, mode and closing', async () => {
    let done: (x: { default_root: string }) => void = () => {};
    mocks.defaultRoot.mockImplementationOnce(
      () =>
        new Promise((resolve) => {
          done = resolve;
        })
    );
    const view = render(<NewTaskDialog isOpen onClose={vi.fn()} initialSource="new_local" />);
    fireEvent.change(view.getByLabelText('工作空间名称'), { target: { value: 'first' } });
    fireEvent.change(view.getByLabelText('工作空间名称'), { target: { value: 'second' } });
    fireEvent.change(view.getByLabelText('父文件夹'), { target: { value: '/manual' } });
    fireEvent.change(view.getByLabelText('新文件夹名称'), { target: { value: 'typed' } });
    fireEvent.click(view.getByRole('button', { name: '已有本地文件夹' }));
    view.rerender(<NewTaskDialog isOpen={false} onClose={vi.fn()} initialSource="new_local" />);
    await act(async () => done({ default_root: '/stale' }));
    expect(mocks.workspaceState.createAndSwitch).not.toHaveBeenCalled();
    expect(view.queryByText('/stale')).toBeNull();
  });

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
    await waitFor(() =>
      expect((view.getByRole('button', { name: '连接并进入' }) as HTMLButtonElement).disabled).toBe(
        false
      )
    );
    fireEvent.click(view.getByRole('button', { name: '连接并进入' }));
    await waitFor(() =>
      expect(mocks.workspaceState.switchTo).toHaveBeenLastCalledWith(
        'remote-project',
        expect.any(String),
        'generation'
      )
    );
    expect(JSON.stringify(workspace)).toBe(before);
    expect(close).toHaveBeenCalledOnce();
  });
});
