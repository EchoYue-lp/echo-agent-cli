// @vitest-environment jsdom
import { cleanup, fireEvent, render, waitFor } from '@testing-library/react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

const mocks = vi.hoisted(() => ({
  listConfigured: vi.fn(),
  listProviders: vi.fn(),
  setThinking: vi.fn(),
  setDefault: vi.fn(),
  getPermissionMode: vi.fn(),
  workspaceId: 'workspace-a',
  openWorkspaceDialog: vi.fn(),
  switchWorkspace: vi.fn(),
  remote: false,
  navigationEpoch: 0,
}));

vi.mock('../../api/endpoints', () => ({
  providerApi: {
    listConfigured: mocks.listConfigured,
    listProviders: mocks.listProviders,
    setThinking: mocks.setThinking,
    setDefault: mocks.setDefault,
  },
  permissionsApi: {
    getMode: mocks.getPermissionMode,
    setMode: vi.fn(),
  },
}));

vi.mock('../../stores/workspaceStore', () => {
  const state = () => {
    const current = {
      id: mocks.workspaceId,
      name: '示例项目',
      root: '/private/product-data',
      project_root: '/code/project',
      metadata: {
        tags: [],
        project_execution_location: mocks.remote
          ? { kind: 'ssh', user: 'echo', host: 'remote', port: null, path: '/srv/project' }
          : { kind: 'local', path: '/code/project' },
        project_location: { kind: 'local' as const, path: '/code/project' },
      },
      product_data_generation: 'v1',
    };
    return { current, workspaces: [current], isLoading: false, switchTo: mocks.switchWorkspace };
  };
  return {
    useWorkspaceStore: Object.assign((selector: (value: unknown) => unknown) => selector(state()), {
      getState: state,
    }),
  };
});

vi.mock('../../stores/uiStore', () => {
  const state = {
    setActiveSettingsTab: vi.fn(),
    openWorkspaceDialog: mocks.openWorkspaceDialog,
    get navigationEpoch() {
      return mocks.navigationEpoch;
    },
    beginNavigation: () => ++mocks.navigationEpoch,
  };
  return {
    useUiStore: Object.assign((selector: (value: unknown) => unknown) => selector(state), {
      getState: () => state,
    }),
  };
});
vi.mock('../../stores/workspaceViewStore', () => ({
  useWorkspaceViewStore: { getState: () => ({ openChat: vi.fn() }) },
}));

import { ChatInput } from './ChatInput';

describe('ChatInput thinking publication', () => {
  afterEach(cleanup);

  beforeEach(() => {
    vi.clearAllMocks();
    mocks.workspaceId = 'workspace-a';
    mocks.remote = false;
    mocks.navigationEpoch = 0;
    localStorage.setItem('echo_thinking_level', 'high');
    mocks.getPermissionMode.mockResolvedValue({ mode: 'default' });
    mocks.listProviders.mockResolvedValue({ providers: [] });
    mocks.listConfigured.mockResolvedValue({
      default_model_id: 'provider:model',
      models: [
        {
          id: 'provider:model',
          display_name: 'Model',
          provider: 'provider',
          model: 'model',
          api_protocol: 'responses',
          input_modalities: ['text'],
          enabled: true,
          is_default: true,
          has_auth_token: true,
          auth_source: 'config',
          base_url: 'https://example.test/v1/responses',
          max_tokens: null,
          context_window: 32_000,
          thinking_levels: ['none', 'low', 'medium', 'high'],
        },
      ],
    });
    mocks.setThinking.mockResolvedValue({ success: true, spec: 'high', applied: true });
  });

  it('preserves the stored level and republishes it when the workspace changes', async () => {
    const props = {
      onSend: vi.fn(),
      isStreaming: false,
      onCancel: vi.fn(),
    };
    const view = render(<ChatInput {...props} />);

    await waitFor(() => {
      expect(mocks.setThinking).toHaveBeenCalledWith('high', 'workspace-a');
    });
    expect(localStorage.getItem('echo_thinking_level')).toBe('high');

    mocks.workspaceId = 'workspace-b';
    view.rerender(<ChatInput {...props} />);

    await waitFor(() => {
      expect(mocks.setThinking).toHaveBeenCalledWith('high', 'workspace-b');
    });
  });

  it('区分不同Provider的同名模型，选中后仍提交完整模型ID', async () => {
    const response = await mocks.listConfigured();
    const original = response.models[0];
    mocks.listConfigured.mockResolvedValue({
      ...response,
      models: [
        { ...original, provider: 'provider-01e37193-76a9-40ed-8029-b5eaf3aa502e' },
        {
          ...original,
          id: 'other:model',
          provider: 'provider-96c1b3d5-a4c2-4db0-88c0-ab197a9e4330',
          is_default: false,
        },
      ],
    });
    mocks.listProviders.mockResolvedValue({
      providers: [
        { id: 'provider-01e37193-76a9-40ed-8029-b5eaf3aa502e', name: '官方 DeepSeek' },
        { id: 'provider-96c1b3d5-a4c2-4db0-88c0-ab197a9e4330', name: '测试中转' },
      ],
    });
    mocks.setDefault.mockResolvedValue({ success: true });
    const view = render(<ChatInput onSend={vi.fn()} />);
    const trigger = view.getByRole('button', { name: '选择模型' });
    await waitFor(() => expect(trigger.textContent).toContain('官方 DeepSeek / Model'));
    fireEvent.click(trigger);
    expect(view.getByRole('button', { name: '官方 DeepSeek / Model' })).toBeDefined();
    fireEvent.click(view.getByRole('button', { name: '测试中转 / Model' }));
    await waitFor(() => expect(mocks.setDefault).toHaveBeenCalledWith('other:model'));
  });
  it('名称查询失败时仍显示Provider标识并保留原模型选择', async () => {
    mocks.listProviders.mockRejectedValueOnce(new Error('名称暂不可用'));
    const view = render(<ChatInput onSend={vi.fn()} />);
    const trigger = view.getByRole('button', { name: '选择模型' });
    await waitFor(() => expect(trigger.textContent).toContain('provider / Model'));
    fireEvent.click(trigger);
    expect(view.getByRole('button', { name: 'provider / Model' })).toBeDefined();
  });

  it('offers real workspace sources and restores keyboard focus after closing the menu', async () => {
    const view = render(<ChatInput onSend={vi.fn()} />);
    const trigger = view.getByRole('button', { name: '选择工作空间' });
    fireEvent.click(trigger);
    const search = view.getByRole('textbox', { name: '搜索工作空间' });
    await waitFor(() => expect(document.activeElement).toBe(search));
    fireEvent.change(search, { target: { value: 'does-not-exist' } });
    expect(view.getByText('没有匹配的工作空间')).toBeTruthy();
    fireEvent.keyDown(search, { key: 'Escape' });
    expect(view.queryByRole('dialog', { name: '工作空间选择' })).toBeNull();
    expect(document.activeElement).toBe(trigger);
    for (const [label, source] of [
      ['新建工作空间', 'new_local'],
      ['打开本地文件夹', 'existing_local'],
      ['连接远程 SSH 设备', 'ssh'],
    ] as const) {
      fireEvent.click(trigger);
      fireEvent.click(view.getByRole('button', { name: label }));
      expect(mocks.openWorkspaceDialog).toHaveBeenLastCalledWith(source);
    }
  });

  it('shows the remote execution directory and keeps permission/model controls accessible', async () => {
    mocks.remote = true;
    const view = render(<ChatInput onSend={vi.fn()} />);
    expect(view.getByRole('button', { name: '选择工作空间' }).getAttribute('title')).toBe(
      'echo@remote · /srv/project'
    );
    expect(view.getByRole('button', { name: '权限配置' })).toBeTruthy();
    await waitFor(() =>
      expect(view.getByRole('button', { name: '选择模型' }).textContent).toContain('Model')
    );
    expect(view.getByRole('button', { name: '选择模型' }).getAttribute('title')).toBe(
      'provider / model'
    );
    fireEvent.click(view.getByRole('button', { name: '选择工作空间' }));
    expect(view.getByText('echo@remote · /srv/project')).toBeTruthy();
    expect(view.queryByText('/private/product-data')).toBeNull();
    fireEvent.pointerDown(document.body);
    expect(view.queryByRole('dialog', { name: '工作空间选择' })).toBeNull();
    await waitFor(() => expect(mocks.setThinking).toHaveBeenCalled());
  });
});
