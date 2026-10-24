// @vitest-environment jsdom
import { act, cleanup, fireEvent, render, screen, waitFor } from '@testing-library/react';
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
  useUiStore.setState({ settingsOpen: false, activeSettingsTab: 'overview' });
  vi.restoreAllMocks();
});

describe('optional settings screens', () => {
  it('retires LSP navigation and safely falls back from an obsolete settings key', () => {
    useUiStore.setState({ settingsOpen: true, activeSettingsTab: 'lsp' as never });
    render(<SettingsDialog />);
    expect(screen.queryByRole('button', { name: /^LSP/ })).toBeNull();
    expect(screen.getByText('运行与记录')).toBeTruthy();
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
    fireEvent.click(screen.getByRole('button', { name: '总览' }));
    expect(screen.queryByRole('status')).toBeNull();
    expect(screen.getByText('运行与记录')).toBeTruthy();

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

    fireEvent.click(screen.getByRole('button', { name: '返回总览' }));
    expect(screen.queryByRole('alert')).toBeNull();
    expect(screen.getByText('运行与记录')).toBeTruthy();
    fireEvent.keyDown(document, { key: 'Escape' });
    await waitFor(() => expect(screen.queryByRole('dialog')).toBeNull());
    expect(document.activeElement).toBe(opener);
    expect(unmounts).toBe(0);
    expect(consoleError).toHaveBeenCalled();

    view.unmount();
    opener.remove();
  });
});
