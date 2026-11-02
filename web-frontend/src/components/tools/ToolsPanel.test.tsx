// @vitest-environment jsdom
import { act, cleanup, fireEvent, render, screen, waitFor } from '@testing-library/react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { ToolsPanel } from './ToolsPanel';
import type { ToolInfo } from '../../generated/ToolInfo';

const mocks = vi.hoisted(() => ({
  list: vi.fn(),
  enable: vi.fn(),
  disable: vi.fn(),
}));
const workspace = vi.hoisted(() => {
  let current: { id: string; product_data_generation: string } | null = null;
  const listeners = new Set<
    (next: { current: typeof current }, previous: { current: typeof current }) => void
  >();
  return {
    getState: () => ({ current }),
    subscribe: (
      listener: (next: { current: typeof current }, previous: { current: typeof current }) => void
    ) => {
      listeners.add(listener);
      return () => {
        listeners.delete(listener);
      };
    },
    set: (value: typeof current) => {
      const previous = { current };
      current = value;
      for (const listener of listeners) listener({ current }, previous);
    },
  };
});
vi.mock('../../api/endpoints', () => ({ toolsApi: mocks }));
vi.mock('../../stores/workspaceStore', () => ({
  useWorkspaceStore: workspace,
}));

function tool(name: string, changes: Partial<ToolInfo> = {}): ToolInfo {
  return {
    name,
    description: `说明 ${name}`,
    parameters: {},
    enabled: true,
    policy_enabled: true,
    required_reason: null,
    category: 'commands_runtime',
    source: { kind: 'unknown' },
    ...changes,
  };
}
const receipt = (name: string, enabled: boolean) => ({
  success: true,
  name,
  policy_enabled: enabled,
  effective_enabled: enabled,
  changed: true,
  revision: 1,
});
let rows: ToolInfo[];

beforeEach(() => {
  vi.resetAllMocks();
  workspace.set(null);
  rows = [tool('shell'), tool('run_code')];
  mocks.list.mockImplementation(async () => rows.map((entry) => ({ ...entry })));
  mocks.enable.mockImplementation(async (name: string) => {
    rows = rows.map((entry) =>
      entry.name === name ? { ...entry, policy_enabled: true, enabled: true } : entry
    );
    return receipt(name, true);
  });
  mocks.disable.mockImplementation(async (name: string) => {
    rows = rows.map((entry) =>
      entry.name === name ? { ...entry, policy_enabled: false, enabled: false } : entry
    );
    return receipt(name, false);
  });
});
afterEach(cleanup);

async function openCommands() {
  fireEvent.click(await screen.findByRole('button', { name: /命令与运行/ }));
}

describe('六类工具与同一后端选择', () => {
  it('folds categories, searches unknown tools and restores the original fold state', async () => {
    rows.push(
      tool('new_dynamic', {
        category: null,
        source: {
          kind: 'mcp',
          server: '数据:server',
          tool: '新能力',
          plugin_id: '团队/插件',
        },
      })
    );
    render(<ToolsPanel />);
    await screen.findByRole('button', { name: /命令与运行/ });
    expect(screen.queryByRole('switch')).toBeNull();
    fireEvent.change(screen.getByRole('searchbox'), {
      target: { value: 'new_dynamic' },
    });
    expect(screen.getByRole('switch', { name: 'new_dynamic启用选择' })).toBeTruthy();
    expect(screen.getByText('插件 · 团队/插件 / MCP · 数据:server')).toBeTruthy();
    fireEvent.change(screen.getByRole('searchbox'), { target: { value: '' } });
    expect(screen.queryByRole('switch')).toBeNull();
  });

  it('keeps required tools locked and only switches optional items, even during search', async () => {
    rows = [
      tool('read_artifact', {
        category: 'files_code',
        required_reason: '读取完整目标',
      }),
      tool('read_file', { category: 'files_code' }),
      tool('apply_patch', { category: 'files_code' }),
    ];
    render(<ToolsPanel />);
    fireEvent.click(await screen.findByRole('button', { name: /文件与代码/ }));
    const locked = screen.getByRole('switch', {
      name: 'read_artifact启用选择',
    }) as HTMLInputElement;
    expect(locked.disabled).toBe(true);
    expect(screen.getByText('系统必需：读取完整目标')).toBeTruthy();
    fireEvent.change(screen.getByRole('searchbox'), {
      target: { value: 'read_file' },
    });
    fireEvent.click(screen.getByRole('checkbox', { name: '切换文件与代码全部可选工具' }));
    await waitFor(() => expect(mocks.disable).toHaveBeenCalledTimes(2));
    expect(mocks.disable.mock.calls.map(([name]) => name)).toEqual(['read_file', 'apply_patch']);
    expect(mocks.disable).not.toHaveBeenCalledWith('read_artifact', expect.anything());
    expect(mocks.disable).toHaveBeenCalledWith('read_file', {
      workspaceId: 'global',
      workspaceGeneration: 'global',
    });
  });

  it('shows mixed optional counts and disables an all-required category', async () => {
    rows = [
      tool('shell'),
      tool('run_code', { policy_enabled: false, enabled: false }),
      tool('task_list', {
        category: 'task_collaboration',
        required_reason: '任务图',
      }),
    ];
    render(<ToolsPanel />);
    const mixed = (await screen.findByRole('checkbox', {
      name: '切换命令与运行全部可选工具',
    })) as HTMLInputElement;
    expect(mixed.indeterminate).toBe(true);
    expect(mixed.getAttribute('aria-checked')).toBe('mixed');
    const required = screen.getByRole('checkbox', {
      name: '切换任务与协作全部可选工具',
    }) as HTMLInputElement;
    expect(required.disabled).toBe(true);
  });

  it('reports partial failure and keeps choices separate from effective availability', async () => {
    mocks.disable.mockImplementation(async (name: string) => {
      if (name === 'run_code') throw new Error('目录发生变化');
      rows = rows.map((entry) =>
        entry.name === name ? { ...entry, policy_enabled: false, enabled: true } : entry
      );
      return { ...receipt(name, false), effective_enabled: true };
    });
    render(<ToolsPanel />);
    await openCommands();
    fireEvent.click(screen.getByRole('checkbox', { name: '切换命令与运行全部可选工具' }));
    expect(await screen.findByRole('alert')).toHaveProperty(
      'textContent',
      expect.stringContaining('run_code')
    );
    expect(
      (
        screen.getByRole('switch', {
          name: 'shell启用选择',
        }) as HTMLInputElement
      ).checked
    ).toBe(false);
    expect(
      (
        screen.getByRole('switch', {
          name: 'run_code启用选择',
        }) as HTMLInputElement
      ).checked
    ).toBe(true);
    expect(screen.getByRole('status').textContent).toContain('已处理 1 项');
  });

  it('rechecks unregistration and required policy before submitting each selected identity', async () => {
    mocks.list
      .mockResolvedValueOnce(rows)
      .mockResolvedValueOnce([tool('run_code', { required_reason: '当前产品依赖' })]);
    render(<ToolsPanel />);
    fireEvent.click(
      await screen.findByRole('checkbox', {
        name: '切换命令与运行全部可选工具',
      })
    );
    await screen.findByRole('alert');
    expect(mocks.disable).not.toHaveBeenCalled();
    expect(screen.getByRole('alert').textContent).toContain('已注销');
    expect(screen.getByRole('alert').textContent).toContain('系统必需');
  });

  it('stops the original batch across A→B→A, including a late successful receipt', async () => {
    let finish: ((value: ReturnType<typeof receipt>) => void) | undefined;
    mocks.disable.mockImplementationOnce(
      () =>
        new Promise((resolve) => {
          finish = resolve;
        })
    );
    workspace.set({ id: 'A', product_data_generation: 'A1' });
    render(<ToolsPanel />);
    fireEvent.click(
      await screen.findByRole('checkbox', {
        name: '切换命令与运行全部可选工具',
      })
    );
    await waitFor(() => expect(mocks.disable).toHaveBeenCalledTimes(1));
    act(() => {
      workspace.set({ id: 'B', product_data_generation: 'B1' });
      workspace.set({ id: 'A', product_data_generation: 'A1' });
    });
    await act(async () => {
      finish?.(receipt('shell', false));
    });
    await waitFor(() => expect(mocks.list.mock.calls.length).toBeGreaterThanOrEqual(4));
    expect(mocks.disable).toHaveBeenCalledTimes(1);
    await openCommands();
    expect(
      (
        screen.getByRole('switch', {
          name: 'shell启用选择',
        }) as HTMLInputElement
      ).checked
    ).toBe(true);
  });

  it('does not enqueue remaining mutations after unmount', async () => {
    let finish: ((value: ReturnType<typeof receipt>) => void) | undefined;
    mocks.disable.mockImplementationOnce(
      () =>
        new Promise((resolve) => {
          finish = resolve;
        })
    );
    const view = render(<ToolsPanel />);
    fireEvent.click(
      await screen.findByRole('checkbox', {
        name: '切换命令与运行全部可选工具',
      })
    );
    await waitFor(() => expect(mocks.disable).toHaveBeenCalledTimes(1));
    view.unmount();
    await act(async () => {
      finish?.(receipt('shell', false));
    });
    expect(mocks.disable).toHaveBeenCalledTimes(1);
  });

  it('does not send mutations if the refresh fails and exposes a retry action', async () => {
    mocks.list.mockResolvedValueOnce(rows).mockRejectedValueOnce(new Error('读取目录失败'));
    render(<ToolsPanel />);
    fireEvent.click(
      await screen.findByRole('checkbox', {
        name: '切换命令与运行全部可选工具',
      })
    );
    expect((await screen.findByRole('alert')).textContent).toContain('未提交变更');
    expect(mocks.disable).not.toHaveBeenCalled();
    fireEvent.click(screen.getByRole('button', { name: '重新加载' }));
    await waitFor(() => expect(mocks.list).toHaveBeenCalledTimes(3));
  });
});

describe('锁定理由的键盘可发现性', () => {
  it('describes the focusable details control and preserves the required filter', async () => {
    rows = [
      tool('task_list', { category: 'task_collaboration', required_reason: '唯一任务图' }),
      tool('shell'),
    ];
    render(<ToolsPanel />);
    await screen.findByRole('button', { name: /任务与协作/ });
    fireEvent.click(screen.getByRole('button', { name: '系统必需' }));
    const details = screen.getByRole('button', { name: '查看task_list详情' });
    details.focus();
    expect(document.activeElement).toBe(details);
    const reason = details.getAttribute('aria-describedby');
    expect(reason).toBeTruthy();
    expect(document.getElementById(reason ?? '')?.textContent).toBe('系统必需：唯一任务图');
    expect(screen.queryByRole('switch', { name: 'shell启用选择' })).toBeNull();
    expect(
      (screen.getByRole('switch', { name: 'task_list启用选择' }) as HTMLInputElement).disabled
    ).toBe(true);
  });
});

describe('后端已变更但操作回执失败时重新读取权威目录', () => {
  it('reconciles an already committed policy after publication throws instead of leaving the switch stale', async () => {
    mocks.disable.mockImplementation(async (name: string) => {
      rows = rows.map((entry) =>
        entry.name === name ? { ...entry, policy_enabled: false, enabled: false } : entry
      );
      throw new Error('策略已变更，但发布回执失败');
    });
    render(<ToolsPanel />);
    await openCommands();
    fireEvent.click(screen.getByRole('switch', { name: 'shell启用选择' }));
    await screen.findByRole('alert');
    await waitFor(() =>
      expect(
        (screen.getByRole('switch', { name: 'shell启用选择' }) as HTMLInputElement).checked
      ).toBe(false)
    );
    expect(mocks.disable).toHaveBeenCalledTimes(1);
    expect(screen.getByRole('alert').textContent).toContain('发布回执失败');
  });

  it('removes a tool unregistered while its mutation was in flight and keeps the original failure visible', async () => {
    mocks.disable.mockImplementation(async () => {
      rows = rows.filter((entry) => entry.name !== 'shell');
      throw new Error('tool shell is not registered');
    });
    render(<ToolsPanel />);
    await openCommands();
    fireEvent.click(screen.getByRole('switch', { name: 'shell启用选择' }));
    await screen.findByRole('alert');
    await waitFor(() => expect(screen.queryByRole('switch', { name: 'shell启用选择' })).toBeNull());
    expect(screen.getByRole('alert').textContent).toContain('not registered');
  });

  it('keeps successful receipts if final reconciliation fails and explicitly asks for a refresh', async () => {
    mocks.list
      .mockResolvedValueOnce(rows)
      .mockResolvedValueOnce(rows)
      .mockRejectedValueOnce(new Error('目录暂不可读'));
    render(<ToolsPanel />);
    await openCommands();
    fireEvent.click(screen.getByRole('switch', { name: 'shell启用选择' }));
    expect((await screen.findByRole('alert')).textContent).toContain('目录暂不可读');
    expect(
      (screen.getByRole('switch', { name: 'shell启用选择' }) as HTMLInputElement).checked
    ).toBe(false);
    expect(mocks.disable).toHaveBeenCalledTimes(1);
    expect(
      (screen.getByRole('switch', { name: 'shell启用选择' }) as HTMLInputElement).disabled
    ).toBe(true);
    expect(
      (screen.getByRole('checkbox', { name: '切换命令与运行全部可选工具' }) as HTMLInputElement)
        .disabled
    ).toBe(true);
  });

  it('requires an explicit successful reload after both a committed mutation failure and an unreadable catalog', async () => {
    rows = [
      tool('shell'),
      tool('run_code'),
      tool('task_list', {
        category: 'task_collaboration',
        required_reason: '唯一任务图',
      }),
    ];
    mocks.list
      .mockResolvedValueOnce(rows)
      .mockResolvedValueOnce(rows)
      .mockRejectedValueOnce(new Error('目录暂不可读'))
      .mockRejectedValueOnce(new Error('重载仍不可读'));
    mocks.disable.mockImplementationOnce(async (name: string) => {
      rows = rows.map((entry) =>
        entry.name === name ? { ...entry, policy_enabled: false, enabled: false } : entry
      );
      throw new Error('策略已提交，发布回执失败');
    });
    render(<ToolsPanel />);
    await openCommands();
    fireEvent.click(screen.getByRole('switch', { name: 'shell启用选择' }));
    await waitFor(() =>
      expect(screen.getByRole('alert').textContent).toContain('工具状态尚未确认')
    );
    expect(screen.getByRole('alert').textContent).toContain('发布回执失败');
    const shell = screen.getByRole('switch', { name: 'shell启用选择' }) as HTMLInputElement;
    const category = screen.getByRole('checkbox', {
      name: '切换命令与运行全部可选工具',
    }) as HTMLInputElement;
    // 未返回成功回执，旧显示仍开启；不把它当成当前后端事实或自动回滚。
    expect(shell.checked).toBe(true);
    expect(shell.disabled).toBe(true);
    expect(category.disabled).toBe(true);
    fireEvent.click(shell);
    fireEvent.click(category);
    expect(mocks.disable).toHaveBeenCalledTimes(1);
    expect(mocks.enable).not.toHaveBeenCalled();
    expect(mocks.list).toHaveBeenCalledTimes(3);

    fireEvent.click(screen.getByRole('button', { name: '重新加载' }));
    await waitFor(() => expect(screen.getByRole('alert').textContent).toContain('重载仍不可读'));
    expect(
      (screen.getByRole('switch', { name: 'shell启用选择' }) as HTMLInputElement).disabled
    ).toBe(true);
    expect(mocks.disable).toHaveBeenCalledTimes(1);

    fireEvent.click(screen.getByRole('button', { name: '重新加载' }));
    await waitFor(() =>
      expect(
        (screen.getByRole('switch', { name: 'shell启用选择' }) as HTMLInputElement).checked
      ).toBe(false)
    );
    expect(
      (screen.getByRole('switch', { name: 'shell启用选择' }) as HTMLInputElement).disabled
    ).toBe(false);
    expect(mocks.list).toHaveBeenCalledTimes(5);
    expect(mocks.disable).toHaveBeenCalledTimes(1);
    expect(mocks.enable).not.toHaveBeenCalled();
    fireEvent.click(screen.getByRole('button', { name: /任务与协作/ }));
    expect(
      (screen.getByRole('switch', { name: 'task_list启用选择' }) as HTMLInputElement).disabled
    ).toBe(true);
  });
});
