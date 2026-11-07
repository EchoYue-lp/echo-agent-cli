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
const configApi = vi.hoisted(() => ({ get: vi.fn(), getFull: vi.fn(), updateFull: vi.fn() }));
const channelApi = vi.hoisted(() => ({
  configuration: vi.fn(),
  save: vi.fn(),
  status: vi.fn(),
  control: vi.fn(),
}));
const weixinQrApi = vi.hoisted(() => ({
  begin: vi.fn(),
  status: vi.fn(),
  verify: vi.fn(),
  cancel: vi.fn(),
  forget: vi.fn(),
}));
vi.mock('../../api/endpoints', () => ({
  configApi,
  channelApi,
  weixinQrApi,
  conversationApi: { list: vi.fn().mockResolvedValue([]) },
}));

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
    const basic = within(screen.getByRole('group', { name: '基础与扩展' }));
    expect(basic.getByRole('button', { name: '基础配置' })).toBeTruthy();
    expect(basic.getByRole('button', { name: '模型' }).getAttribute('aria-current')).toBe('page');
    const conversations = within(screen.getByRole('group', { name: '会话与记忆' }));
    for (const name of ['会话', '归档', '记忆', '压缩']) {
      expect(conversations.getByRole('button', { name })).toBeTruthy();
    }
    const automation = within(screen.getByRole('group', { name: '连接与自动化' }));
    for (const name of ['定时任务', 'Hooks']) {
      expect(automation.getByRole('button', { name })).toBeTruthy();
    }
    expect(screen.queryByRole('button', { name: 'IM 渠道' })).toBeNull();
    expect(screen.queryByRole('button', { name: 'Worktree' })).toBeNull();
    fireEvent.click(screen.getByRole('button', { name: '高级' }));
    expect(screen.getByRole('button', { name: '高级' }).getAttribute('aria-expanded')).toBe('true');
    for (const name of ['Worktree', /^自进化/]) {
      expect(screen.getByRole('button', { name })).toBeTruthy();
    }
    expect(
      within(screen.getByRole('group', { name: '高级' })).queryByRole('button', {
        name: '基础配置',
      })
    ).toBeNull();
  });

  it('侧栏IM入口独立展示原渠道页，迟到加载和关闭不改变默认设置', async () => {
    const { LeftSidebar } = await import('./LeftSidebar');
    const configuration = {
      revision: 'channel-v1',
      weixin: { enabled: false, has_credential: false },
      qq: { enabled: true, app_id: 'old-app', has_secret: true },
      feishu: {
        enabled: false,
        app_id: '',
        has_secret: false,
        mode: 'long_poll',
        webhook_bind: '127.0.0.1:9000',
        webhook_path: '/events',
        has_verification_token: false,
      },
      session: { timeout_minutes: 30 },
    };
    channelApi.configuration.mockResolvedValue(configuration);
    channelApi.status.mockResolvedValue({ configuration_revision: 'channel-v1', channels: [] });
    channelApi.save.mockResolvedValue({ ...configuration, revision: 'channel-v2' });
    weixinQrApi.status.mockResolvedValue({
      phase: 'idle',
      has_credential: false,
      configuration_revision: 'channel-v1',
    });
    render(
      <>
        <LeftSidebar onNewTask={vi.fn()} />
        <SettingsDialog />
      </>
    );
    const opener = screen.getByRole('button', { name: 'IM 渠道' });
    opener.focus();
    fireEvent.click(opener);
    const dialog = screen.getByRole('dialog', { name: 'IM 渠道' });
    expect(within(dialog).queryByRole('navigation', { name: '设置分类' })).toBeNull();
    expect(dialog.querySelectorAll('#channels-dialog-title')).toHaveLength(1);
    expect(within(dialog).getByRole('status').textContent).toContain('正在加载');
    fireEvent.click(within(dialog).getByRole('button', { name: '关闭IM渠道' }));
    expect(screen.queryByRole('dialog')).toBeNull();
    expect(document.activeElement).toBe(opener);
    await act(async () => {
      await import('../channels/ChannelsPanel');
    });
    fireEvent.click(opener);
    const app = await screen.findByLabelText('QQ Bot App ID');
    fireEvent.change(app, { target: { value: 'edited-app' } });
    fireEvent.click(screen.getByRole('button', { name: '保存QQ Bot配置' }));
    await waitFor(() =>
      expect(channelApi.save).toHaveBeenCalledWith(
        expect.objectContaining({
          channel: 'qqbot',
          expected_revision: 'channel-v1',
          app_id: 'edited-app',
          secret: { action: 'preserve' },
        })
      )
    );
    expect(await screen.findByText('QQ Bot配置已保存')).toBeTruthy();
    expect(screen.getByRole('button', { name: '获取微信二维码' })).toBeTruthy();
    fireEvent.keyDown(document, { key: 'Escape' });
    await waitFor(() => expect(screen.queryByRole('dialog')).toBeNull());
    expect(document.activeElement).toBe(opener);
    act(() => useUiStore.getState().openSettings());
    expect(screen.getByRole('dialog', { name: '设置' })).toBeTruthy();
    expect(screen.getByRole('button', { name: '模型' }).getAttribute('aria-current')).toBe('page');
  });

  it('独立IM页面加载异常仍可关闭并恢复默认模型设置', async () => {
    const consoleError = vi.spyOn(console, 'error').mockImplementation(() => {});
    const { LeftSidebar } = await import('./LeftSidebar');
    channelApi.configuration.mockImplementationOnce(() => {
      throw new Error('controlled channel screen failure');
    });
    channelApi.status.mockResolvedValue({ configuration_revision: 'channel-v1', channels: [] });
    render(
      <>
        <LeftSidebar onNewTask={vi.fn()} />
        <SettingsDialog />
      </>
    );
    const opener = screen.getByRole('button', { name: 'IM 渠道' });
    opener.focus();
    fireEvent.click(opener);
    expect(await screen.findByRole('alert')).toHaveProperty(
      'textContent',
      expect.stringContaining('IM渠道页面加载失败')
    );
    expect(screen.queryByRole('navigation', { name: '设置分类' })).toBeNull();
    fireEvent.click(screen.getByRole('button', { name: '关闭IM渠道' }));
    expect(screen.queryByRole('dialog')).toBeNull();
    expect(document.activeElement).toBe(opener);
    act(() => useUiStore.getState().openSettings());
    expect(screen.getByRole('dialog', { name: '设置' })).toBeTruthy();
    expect(screen.getByRole('button', { name: '模型' }).getAttribute('aria-current')).toBe('page');
    expect(screen.queryByRole('alert')).toBeNull();
    expect(consoleError).toHaveBeenCalled();
  });

  it('从常显基础入口打开原智能体配置并沿原保存链生效', async () => {
    const saved = (depth: number) => ({
      max_subagent_depth: depth,
      model: { provider: 'test', name: 'test', has_auth_token: false },
      agent: { model: 'test', system_prompt: 'Test', token_limit: 4000 },
      mcp: { config_path: null },
      channels: {
        qq: { enabled: false, app_id: '' },
        feishu: { enabled: false, app_id: '', mode: 'long_poll' },
        session: { timeout_minutes: 60, reset_keywords: [], reset_commands: [] },
      },
      server: { host: '127.0.0.1', port: 0 },
      logging: { level: 'info', file: null },
    });
    configApi.get.mockResolvedValue(saved(1).agent);
    configApi.getFull.mockResolvedValue(saved(1));
    configApi.updateFull.mockResolvedValue(saved(2));
    useUiStore.getState().openSettings();
    render(<SettingsDialog />);
    expect(screen.getByRole('button', { name: '模型' }).getAttribute('aria-current')).toBe('page');
    expect(screen.getByRole('button', { name: '高级' }).getAttribute('aria-expanded')).toBe(
      'false'
    );
    fireEvent.click(screen.getByRole('button', { name: '基础配置' }));
    expect(useUiStore.getState().activeSettingsTab).toBe('config');
    expect(screen.getByRole('button', { name: '基础配置' }).getAttribute('aria-current')).toBe(
      'page'
    );
    const depth = await screen.findByRole('combobox', { name: '最大子智能体层数' });
    expect(screen.getByText('系统提示词')).toBeTruthy();
    expect(screen.queryByText('单轮推理安全上限')).toBeNull();
    fireEvent.change(depth, { target: { value: '2' } });
    fireEvent.click(screen.getByRole('button', { name: '保存' }));
    await waitFor(() =>
      expect(configApi.updateFull).toHaveBeenCalledWith(
        { max_subagent_depth: 2 },
        expect.any(AbortSignal)
      )
    );
    expect(await screen.findByText('已保存')).toBeTruthy();
    expect((depth as HTMLSelectElement).value).toBe('2');
    expect(screen.queryByRole('button', { name: '展开高级运行参数' })).toBeNull();
    expect(screen.queryByText('单轮推理安全上限')).toBeNull();
    expect(screen.getByRole('button', { name: '高级' }).getAttribute('aria-expanded')).toBe(
      'false'
    );
    expect(screen.queryByRole('button', { name: '总览' })).toBeNull();
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
