// @vitest-environment jsdom
import { act, cleanup, fireEvent, render, screen, waitFor, within } from '@testing-library/react';
import { useEffect } from 'react';
import { afterEach, describe, expect, it, vi } from 'vitest';
import { ErrorBoundary } from '../common/ErrorBoundary';

const imports = vi.hoisted(() => {
  let resolveTools: (value: { ToolsPanel: () => string }) => void = () => {};
  let rejectProviders: (reason: Error) => void = () => {};
  const tools = new Promise<{ ToolsPanel: () => string }>((resolve) => {
    resolveTools = resolve;
  });
  const providers = new Promise<never>((_resolve, reject) => {
    rejectProviders = reject;
  });
  return { tools, providers, resolveTools, rejectProviders };
});

vi.mock('../tools/ToolsPanel', () => imports.tools);
vi.mock('../providers/ProviderPanel', () => imports.providers);

Object.defineProperty(window, 'matchMedia', {
  configurable: true,
  value: () => ({ matches: false }),
});

const { useUiStore } = await import('../../stores/uiStore');
const { SettingsDialog } = await import('./SettingsDialog');

afterEach(() => {
  cleanup();
  useUiStore.setState({ settingsOpen: false, activeSettingsTab: 'providers' });
  vi.restoreAllMocks();
});

describe('optional settings screens', () => {
  it('按用途组织入口并保留高级设置的展开访问', () => {
    useUiStore.getState().openSettings();
    render(<SettingsDialog />);
    const conversations = within(screen.getByRole('group', { name: '会话与记忆' }));
    for (const name of ['会话', '归档', '记忆', '压缩']) {
      expect(conversations.getByRole('button', { name })).toBeTruthy();
    }
    const automation = within(screen.getByRole('group', { name: '连接与自动化' }));
    for (const name of ['IM 渠道', '定时任务', 'Hooks']) {
      expect(automation.getByRole('button', { name })).toBeTruthy();
    }
    expect(screen.queryByRole('button', { name: 'Worktree' })).toBeNull();
    fireEvent.click(screen.getByRole('button', { name: '高级' }));
    expect(screen.getByRole('button', { name: '高级' }).getAttribute('aria-expanded')).toBe('true');
    for (const name of ['配置', 'Worktree', /^自进化/]) {
      expect(screen.getByRole('button', { name })).toBeTruthy();
    }
  });

  it.each(['lsp', 'overview'])('旧设置选择 %s 回到模型页且没有总览入口', (key) => {
    useUiStore.setState({ settingsOpen: true, activeSettingsTab: key as never });
    render(<SettingsDialog />);
    expect(screen.queryByRole('button', { name: /^LSP/ })).toBeNull();
    expect(screen.queryByRole('button', { name: '总览' })).toBeNull();
    expect(screen.getByRole('button', { name: '模型' }).getAttribute('aria-current')).toBe('page');
    expect(screen.queryByRole('alert')).toBeNull();
  });

  it('keeps navigation, close and the workspace available during delayed or failed imports', async () => {
    const consoleError = vi.spyOn(console, 'error').mockImplementation(() => {});
    let mounts = 0;
    let unmounts = 0;
    function Workspace() {
      useEffect(() => {
        mounts += 1;
        return () => {
          unmounts += 1;
        };
      }, []);
      return <div>Active conversation</div>;
    }

    const opener = document.createElement('button');
    document.body.append(opener);
    opener.focus();
    useUiStore.setState({ settingsOpen: true, activeSettingsTab: 'tools' });
    const view = render(
      <ErrorBoundary>
        <Workspace />
        <SettingsDialog />
      </ErrorBoundary>
    );

    expect(screen.getByRole('status').textContent).toContain('正在加载');
    fireEvent.click(screen.getByRole('button', { name: '模型' }));
    expect(screen.getByRole('button', { name: '模型' }).getAttribute('aria-current')).toBe('page');

    act(() => useUiStore.getState().setActiveSettingsTab('tools'));
    expect(screen.getByRole('status')).toBeTruthy();
    fireEvent.click(screen.getByRole('button', { name: '关闭设置' }));
    expect(screen.queryByRole('dialog')).toBeNull();
    expect(document.activeElement).toBe(opener);

    await act(async () => {
      imports.resolveTools({ ToolsPanel: () => 'Loaded tools screen' });
      await imports.tools;
    });
    act(() => useUiStore.getState().setActiveSettingsTab('tools'));
    expect(await screen.findByText('Loaded tools screen')).toBeTruthy();

    act(() => useUiStore.getState().setActiveSettingsTab('providers'));
    expect(screen.getByRole('status')).toBeTruthy();
    act(() => imports.rejectProviders(new Error('controlled settings chunk failure')));
    expect(await screen.findByRole('alert')).toBeTruthy();
    expect(screen.getByRole('dialog', { name: '设置' })).toBeTruthy();
    expect(screen.getByText('Active conversation')).toBeTruthy();
    expect(mounts).toBe(1);
    expect(unmounts).toBe(0);

    fireEvent.click(screen.getByRole('button', { name: '工具' }));
    expect(screen.queryByRole('alert')).toBeNull();
    expect(screen.getByText('Loaded tools screen')).toBeTruthy();
    fireEvent.keyDown(document, { key: 'Escape' });
    await waitFor(() => expect(screen.queryByRole('dialog')).toBeNull());
    expect(document.activeElement).toBe(opener);
    expect(unmounts).toBe(0);
    expect(consoleError).toHaveBeenCalled();

    view.unmount();
    opener.remove();
  });
});
