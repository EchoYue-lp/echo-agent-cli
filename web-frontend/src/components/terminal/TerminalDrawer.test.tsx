// @vitest-environment jsdom
import { act, cleanup, fireEvent, render, screen, waitFor, within } from '@testing-library/react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { useUiStore } from '../../stores/uiStore';
import { TerminalDrawer } from './TerminalDrawer';
import { Terminal } from './Terminal';
import { terminalApi, type Workspace } from '../../api/endpoints';

const mocks = vi.hoisted(() => {
  window.localStorage.setItem('echo-theme', 'light');
  return {
    desktop: true,
    workspace: null as Workspace | null,
    invoke: vi.fn(),
    listen: vi.fn(),
    terminals: [] as Array<{
      disposed: boolean;
      writes: unknown[];
      focusCalls: number;
      data?: (value: string) => void;
    }>,
    handlers: new Map<string, Set<(event: { payload: Record<string, unknown> }) => void>>(),
  };
});
vi.mock('../../stores/workspaceStore', () => ({
  useWorkspaceStore: (selector: (state: { current: Workspace | null }) => unknown) =>
    selector({ current: mocks.workspace }),
}));
vi.mock('../../lib/tauri-bridge', async (original) => ({
  ...(await original<typeof import('../../lib/tauri-bridge')>()),
  isTauri: () => mocks.desktop,
  apiInvoke: mocks.invoke,
}));
vi.mock('@tauri-apps/api/event', () => ({ listen: mocks.listen }));
vi.mock('@xterm/xterm/css/xterm.css', () => ({}));
vi.mock('@xterm/xterm', () => ({
  Terminal: class {
    rows = 24;
    cols = 80;
    disposed = false;
    writes: unknown[] = [];
    focusCalls = 0;
    constructor() {
      mocks.terminals.push(this);
    }
    loadAddon() {}
    open() {}
    focus() {
      this.focusCalls += 1;
    }
    resize() {}
    data?: (value: string) => void;
    onData(callback: (value: string) => void) {
      this.data = callback;
      return { dispose() {} };
    }
    onResize() {
      return { dispose() {} };
    }
    write(data: unknown, done?: () => void) {
      this.writes.push(data);
      done?.();
    }
    dispose() {
      this.disposed = true;
    }
  },
}));
vi.mock('@xterm/addon-fit', () => ({
  FitAddon: class {
    fit() {}
    proposeDimensions() {
      return { cols: 80, rows: 24 };
    }
  },
}));

beforeEach(() => {
  mocks.desktop = true;
  mocks.workspace = null;
  mocks.terminals.length = 0;
  mocks.handlers.clear();
  mocks.invoke.mockReset().mockImplementation(async (command, args) => {
    if (command === 'create_terminal')
      return { id: args.id, pid: 42, target: args.target ?? { kind: 'local' } };
    return { success: true, closed: true };
  });
  mocks.listen.mockReset().mockImplementation(async (name, callback) => {
    const handlers = mocks.handlers.get(name) ?? new Set();
    mocks.handlers.set(name, handlers);
    handlers.add(callback);
    return () => handlers.delete(callback);
  });
  vi.stubGlobal(
    'ResizeObserver',
    class {
      observe() {}
      disconnect() {}
    }
  );
  useUiStore.setState({ terminalOpen: true });
});
afterEach(() => {
  cleanup();
  vi.restoreAllMocks();
  vi.unstubAllGlobals();
});

function creates() {
  return mocks.invoke.mock.calls.filter(([command]) => command === 'create_terminal');
}

describe('terminal drawer session observation', () => {
  it('forwards keyboard focus from the accessible terminal wrapper to xterm', async () => {
    render(<Terminal sessionId="focus-probe" />);
    await waitFor(() => expect(creates()).toHaveLength(1));
    const instance = mocks.terminals.at(0);
    expect(instance).toBeDefined();
    const before = instance?.focusCalls ?? 0;
    fireEvent.focus(screen.getByRole('textbox', { name: '终端' }));
    expect(instance?.focusCalls).toBeGreaterThan(before);
  });

  it('captures the project generation once and keeps an explicit local terminal separate', async () => {
    mocks.workspace = {
      id: 'remote-project',
      name: '远程项目',
      root: '/local/product-data',
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
      product_data_generation: 'project-generation-a',
      created_at: '',
      last_active: '',
    };
    const view = render(<TerminalDrawer />);
    await waitFor(() => expect(creates()).toHaveLength(1));
    expect(creates().at(0)?.at(1)).toMatchObject({
      projectWorkspaceId: 'remote-project',
      projectWorkspaceGeneration: 'project-generation-a',
    });
    expect(creates().at(0)?.at(1).target).toBeUndefined();
    mocks.workspace = {
      ...mocks.workspace,
      id: 'another-project',
      product_data_generation: 'generation-b',
    };
    view.rerender(<TerminalDrawer />);
    expect(creates()).toHaveLength(1);
    fireEvent.click(screen.getByRole('button', { name: '新建独立本地终端' }));
    await waitFor(() => expect(creates()).toHaveLength(2));
    expect(creates().at(1)?.at(1).target).toEqual({ kind: 'local' });
    expect(creates().at(1)?.at(1).projectWorkspaceId).toBeUndefined();
  });
  it('waits for observers before creating the child', async () => {
    let ready: (() => void) | undefined;
    const gate = new Promise<void>((resolve) => {
      ready = resolve;
    });
    const install = mocks.listen.getMockImplementation();
    mocks.listen.mockImplementation(async (...args) => {
      await gate;
      return install?.(...args);
    });
    render(<TerminalDrawer />);
    await waitFor(() => expect(mocks.terminals).toHaveLength(1));
    const before = creates().length;
    await act(async () => {
      ready?.();
    });
    expect(before).toBe(0);
    await waitFor(() => expect(creates()).toHaveLength(1));
  });

  it('keeps every tab observer mounted across selection and drawer hiding', async () => {
    render(<TerminalDrawer />);
    await waitFor(() => expect(creates()).toHaveLength(1));
    fireEvent.click(screen.getByTitle('New terminal'));
    await waitFor(() => expect(creates()).toHaveLength(2));
    expect(mocks.terminals).toHaveLength(2);
    expect(mocks.terminals.every((terminal) => !terminal.disposed)).toBe(true);
    act(() => useUiStore.setState({ terminalOpen: false }));
    expect(mocks.terminals.every((terminal) => !terminal.disposed)).toBe(true);
    act(() => useUiStore.setState({ terminalOpen: true }));
    expect(creates()).toHaveLength(2);
  });

  it('retains the original tab after failed close and permits retry', async () => {
    render(<TerminalDrawer />);
    await waitFor(() => expect(creates()).toHaveLength(1));
    mocks.invoke.mockImplementation(async (command) => {
      if (command === 'close_terminal') throw new Error('original child wait is not settled');
      return { success: true };
    });
    fireEvent.click(within(screen.getByRole('tab')).getByRole('button'));
    await waitFor(() =>
      expect(mocks.invoke.mock.calls.some(([command]) => command === 'close_terminal')).toBe(true)
    );
    expect(screen.queryByRole('tab')).not.toBeNull();
    mocks.invoke.mockResolvedValue({ success: true, closed: true });
    fireEvent.click(within(screen.getByRole('tab')).getByRole('button'));
    await waitFor(() => expect(screen.queryByRole('tab')).toBeNull());
  });
  it('delivers first raw bytes and a real exit before create resolves without overwriting it', async () => {
    let resolveCreate: ((value: unknown) => void) | undefined;
    mocks.invoke.mockImplementation((command, args) => {
      if (command !== 'create_terminal') return Promise.resolve({ success: true, closed: true });
      // The event arrives inside native invocation, before the create response.
      const encoded = btoa(String.fromCharCode(0xe4));
      for (const handler of mocks.handlers.get('terminal-output') ?? [])
        handler({ payload: { id: args.id, data: encoded } });
      for (const handler of mocks.handlers.get('terminal-exit') ?? [])
        handler({ payload: { id: args.id, reason: 'process_exited', exit_code: 17 } });
      return new Promise((resolve) => {
        resolveCreate = resolve;
      });
    });
    render(<TerminalDrawer />);
    await waitFor(() => expect(creates()).toHaveLength(1));
    await waitFor(() => expect(mocks.terminals.at(0)?.writes).toHaveLength(1));
    expect(mocks.terminals.at(0)?.writes.at(0)).toEqual(Uint8Array.of(0xe4));
    const args = creates().at(0)?.at(1);
    await act(async () => resolveCreate?.({ id: args.id, pid: 42, target: { kind: 'local' } }));
    expect(screen.getByText(/本地进程退出 17/)).toBeTruthy();
    expect(screen.queryByText('本地终端进程已启动。')).toBeNull();
  });

  it('continues rendering raw output on an inactive and hidden tab and reports gaps', async () => {
    render(<TerminalDrawer />);
    await waitFor(() => expect(creates()).toHaveLength(1));
    const firstId = creates().at(0)?.at(1).id;
    fireEvent.click(screen.getByTitle('New terminal'));
    await waitFor(() => expect(creates()).toHaveLength(2));
    act(() => useUiStore.setState({ terminalOpen: false }));
    await act(async () => {
      for (const handler of mocks.handlers.get('terminal-output') ?? [])
        handler({ payload: { id: firstId, data: btoa('hidden-output') } });
      for (const handler of mocks.handlers.get('terminal-gap') ?? [])
        handler({ payload: { skipped_events: 3 } });
    });
    expect(Array.from(mocks.terminals.at(0)?.writes.at(0) as Uint8Array)).toEqual(
      Array.from(new TextEncoder().encode('hidden-output'))
    );
    expect(mocks.terminals.at(1)?.writes).toHaveLength(0);
    act(() => useUiStore.setState({ terminalOpen: true }));
    expect(screen.getAllByText(/遗漏 3 条/)).toHaveLength(2);
  });

  it('releases listeners installed after unmount and never starts a process', async () => {
    let ready = () => {};
    const gate = new Promise<void>((resolve) => {
      ready = resolve;
    });
    const releases = vi.fn();
    mocks.listen.mockImplementation(async () => {
      await gate;
      return releases;
    });
    const view = render(<TerminalDrawer />);
    await waitFor(() => expect(mocks.listen).toHaveBeenCalledTimes(4));
    view.unmount();
    await act(async () => ready());
    expect(releases).toHaveBeenCalledTimes(4);
    expect(creates()).toHaveLength(0);
    expect(mocks.terminals.at(0)?.disposed).toBe(true);
  });

  it('observes an outstanding create before compensating an unmount with close', async () => {
    let ready: ((value: unknown) => void) | undefined;
    mocks.invoke.mockImplementation((command) =>
      command === 'create_terminal'
        ? new Promise((resolve) => {
            ready = resolve;
          })
        : Promise.resolve({ success: true, closed: true })
    );
    const view = render(<TerminalDrawer />);
    await waitFor(() => expect(creates()).toHaveLength(1));
    const args = creates().at(0)?.at(1);
    view.unmount();
    expect(
      mocks.invoke.mock.calls.filter(([command]) => command === 'close_terminal')
    ).toHaveLength(0);
    await act(async () => ready?.({ id: args.id, pid: 42, target: args.target }));
    await waitFor(() =>
      expect(mocks.invoke).toHaveBeenCalledWith('close_terminal', { id: args.id })
    );
  });

  it('passes an SSH alias, account and port as target metadata without authentication claims', async () => {
    render(<TerminalDrawer />);
    await waitFor(() => expect(creates()).toHaveLength(1));
    fireEvent.click(screen.getByRole('button', { name: 'SSH' }));
    fireEvent.change(screen.getByLabelText('主机 / SSH alias'), {
      target: { value: 'work-alias' },
    });
    fireEvent.change(screen.getByLabelText('账号（可选）'), { target: { value: 'someone' } });
    fireEvent.change(screen.getByLabelText('端口（可选）'), { target: { value: '2222' } });
    fireEvent.click(screen.getByRole('button', { name: '启动 SSH' }));
    await waitFor(() => expect(creates()).toHaveLength(2));
    expect(creates().at(1)?.at(1).target).toEqual({
      kind: 'ssh',
      host: 'work-alias',
      user: 'someone',
      port: 2222,
    });
    expect(screen.getByText('SSH 本地进程已启动；不代表远端认证成功。')).toBeTruthy();
    expect(screen.queryByText('Connected')).toBeNull();
  });
  it('bounds a pending-create close observation and keeps retry enabled until original settlement', async () => {
    let ready: ((value: unknown) => void) | undefined;
    mocks.invoke.mockImplementation((command) =>
      command === 'create_terminal'
        ? new Promise((resolve) => {
            ready = resolve;
          })
        : Promise.resolve({ success: true, closed: true })
    );
    render(<TerminalDrawer />);
    await waitFor(() => expect(creates()).toHaveLength(1));
    const args = creates().at(0)?.at(1);
    vi.useFakeTimers();
    try {
      fireEvent.click(within(screen.getByRole('tab')).getByRole('button'));
      await act(async () => {
        await vi.advanceTimersByTimeAsync(5001);
      });
      const retry = screen.getByRole('button', { name: /重试关闭/ });
      expect((retry as HTMLButtonElement).disabled).toBe(false);
      expect(
        mocks.invoke.mock.calls.filter(([command]) => command === 'close_terminal')
      ).toHaveLength(0);
      await act(async () => ready?.({ id: args.id, pid: 42, target: args.target }));
      expect(mocks.invoke).toHaveBeenCalledWith('close_terminal', { id: args.id });
    } finally {
      vi.useRealTimers();
    }
  });
  it('keeps the legacy HTTP returned id for its socket and close', async () => {
    mocks.desktop = false;
    const sockets: string[] = [];
    vi.stubGlobal(
      'WebSocket',
      class {
        static OPEN = 1;
        readyState = 1;
        constructor(url: string) {
          sockets.push(url);
        }
        close() {}
      }
    );
    vi.spyOn(terminalApi, 'create').mockResolvedValue({
      id: 'http-original-id',
      pid: 10,
      target: { kind: 'local' },
    });
    const close = vi.spyOn(terminalApi, 'close').mockResolvedValue({ success: true, closed: true });
    const closed = vi.fn();
    const view = render(<Terminal sessionId="ui-only-id" onClosed={closed} />);
    await waitFor(() => expect(sockets).toHaveLength(1));
    expect(sockets.at(0)).toContain('/http-original-id/ws');
    view.rerender(<Terminal sessionId="ui-only-id" closeRequest={1} onClosed={closed} />);
    await waitFor(() => expect(closed).toHaveBeenCalledWith('ui-only-id'));
    expect(close).toHaveBeenCalledExactlyOnceWith('http-original-id');
  });

  it('serializes actual typed input and Ctrl-C while preserving an outstanding write', async () => {
    let release = () => {};
    const writeGate = new Promise<void>((resolve) => {
      release = resolve;
    });
    let writes = 0;
    const original = mocks.invoke.getMockImplementation();
    mocks.invoke.mockImplementation((...args) => {
      if (args.at(0) === 'write_terminal' && writes++ === 0) return writeGate;
      return original?.(...args);
    });
    render(<TerminalDrawer />);
    await waitFor(() => expect(creates()).toHaveLength(1));
    await act(async () => {
      mocks.terminals.at(0)?.data?.('中🙂');
      mocks.terminals.at(0)?.data?.('\x03');
    });
    const written = () =>
      mocks.invoke.mock.calls.filter(([command]) => command === 'write_terminal');
    expect(written()).toHaveLength(1);
    await act(async () => release());
    expect(written()).toHaveLength(2);
    const bytes = written().map((call) =>
      Array.from(atob(call.at(1).data), (value) => value.charCodeAt(0))
    );
    expect(bytes).toEqual([Array.from(new TextEncoder().encode('中🙂')), [3]]);
  });
});
