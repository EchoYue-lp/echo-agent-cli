// @vitest-environment jsdom
import { cleanup, fireEvent, render, screen, waitFor, within } from '@testing-library/react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import type { PluginMutationResult } from '../../api/endpoints';
const api = vi.hoisted(() => ({
  list: vi.fn(),
  install: vi.fn(),
  uninstall: vi.fn(),
  enable: vi.fn(),
  disable: vi.fn(),
  reload: vi.fn(),
  configure: vi.fn(),
  themes: vi.fn(async () => ({ themes: [], omitted: 0, active: null })),
  outputStyles: vi.fn(async () => ({ styles: [], omitted: 0, active: null })),
  validate: vi.fn(async () => ({
    valid: true,
    name: 'mixed',
    components: ['skills'],
    errors: ['Warning: current component warning from validation'],
  })),
}));
vi.mock('../../api/endpoints', async (importOriginal) => ({
  ...(await importOriginal<typeof import('../../api/endpoints')>()),
  pluginApi: api,
  extensionRequestScope: () => ({ workspace_id: 'global', workspace_generation: 'global' }),
}));
vi.mock('../../stores/workspaceStore', () => ({
  useWorkspaceStore: Object.assign(
    (selector: (s: { current: null }) => unknown) => selector({ current: null }),
    { getState: () => ({ current: null }) }
  ),
}));
vi.mock('../../stores/uiStore', () => ({
  applyPluginTheme: vi.fn(),
  useUiStore: (selector: (s: { activePluginTheme: null }) => unknown) =>
    selector({ activePluginTheme: null }),
}));
import { PluginPanel } from './PluginPanel';
afterEach(cleanup);
describe('current plugin component diagnostics', () => {
  it('shows current source diagnostics even when healthy plugin components validate', async () => {
    render(<PluginPanel />);
    fireEvent.click(await screen.findByTitle('Plugin developer tools'));
    fireEvent.change(screen.getByPlaceholderText('Plugin directory'), {
      target: { value: '/isolated/mixed' },
    });
    fireEvent.click(screen.getByRole('button', { name: 'Validate' }));
    expect(await screen.findByText(/current component warning from validation/)).toBeTruthy();
    expect(api.validate).toHaveBeenCalled();
  });
});

function committedMutationResult(
  status: 'settled' | 'degraded' | 'failed' = 'degraded',
  scope = 'global'
): PluginMutationResult & { plugin_id?: string } {
  const receiptMeta = {
    request_id: 'mutation-request',
    operation_id: 'mutation-operation',
    authority_scope: scope,
    workspace_generation: scope === 'global' ? 'global' : 'generation-1',
    sender_id: null,
    sender_incarnation: null,
    status,
    error: null,
  };
  if (status === 'failed')
    return {
      success: false,
      wiring_ok: false,
      error: 'rollback completed',
      receipt: { extension: 'plugins', meta: { ...receiptMeta, status: 'failed' }, receipt: null },
    };
  const errors = status === 'degraded' ? ['Warning: retained component diagnostic'] : [];
  const projection = {
    plugin_id: 'mixed-plugin',
    plugin: null,
    status,
    target_receipts: { items: [], omitted: 0 },
    summary: {
      total: 1,
      enabled: 1,
      skills_loaded: 1,
      hooks_registered: 0,
      mcp_connected: 0,
      agents_loaded: 0,
      monitors_loaded: 0,
      themes_loaded: 0,
      output_styles_loaded: 0,
      errors: { items: errors, omitted: 0 },
    },
    active_theme: null,
    themes: { items: [], omitted: 0 },
    active_output_style: null,
    output_styles: { items: [], omitted: 0 },
  };
  return {
    success: status === 'settled',
    wiring_ok: status === 'settled',
    plugin_id: 'mixed-plugin',
    errors,
    errors_omitted: 0,
    settlement_status: status,
    projection,
    meta: receiptMeta,
    receipt: {
      extension: 'plugins',
      meta: receiptMeta,
      receipt: { action: 'mutation', projection },
    },
  };
}

function currentCatalog(enabled: boolean, value = 'old') {
  return {
    plugins: [
      {
        name: 'mixed-plugin',
        display_name: 'mixed-plugin',
        version: '1',
        description: '',
        author: null,
        license: null,
        scope: 'user',
        enabled,
        path: '/candidate/mixed',
        capabilities: ['Skills'],
        keywords: [],
        dependencies: [],
        config: {
          endpoint: {
            type: 'string',
            title: 'Endpoint',
            description: '',
            sensitive: false,
            required: false,
            default: '',
            multiple: false,
            min: null,
            max: null,
          },
        },
        config_values: { endpoint: value },
      },
    ],
    omitted: 0,
  };
}

beforeEach(() => {
  api.list.mockReset().mockResolvedValue({ plugins: [], omitted: 0 });
  for (const action of [
    api.install,
    api.uninstall,
    api.enable,
    api.disable,
    api.reload,
    api.configure,
  ])
    action.mockReset();
});

describe('committed Plugin mutations with diagnostics', () => {
  it('refreshes a committed degraded install from the real catalog and retains diagnostics', async () => {
    api.list
      .mockResolvedValueOnce({ plugins: [], omitted: 0 })
      .mockResolvedValueOnce(currentCatalog(true));
    const result = committedMutationResult();
    api.install.mockResolvedValue(result);
    render(<PluginPanel />);
    await waitFor(() => expect(api.list).toHaveBeenCalledTimes(1));
    fireEvent.click(screen.getByRole('button', { name: /^Install$/ }));
    const source = screen.getByPlaceholderText('Local path or git URL...');
    fireEvent.change(source, { target: { value: '/candidate/mixed' } });
    const form = source.parentElement;
    if (!form) throw new Error('plugin install form missing');
    fireEvent.click(within(form).getByRole('button', { name: /^Install$/ }));
    expect(await screen.findByText('mixed-plugin')).toBeTruthy();
    expect(await screen.findByText(/已提交，部分组件未启用或存在诊断/)).toBeTruthy();
    expect(screen.getByText(/retained component diagnostic/)).toBeTruthy();
    expect(api.list).toHaveBeenCalledTimes(2);
    expect(result.success).toBe(false);
    expect(screen.queryByText('Install failed')).toBeNull();
  });
  it('refreshes an enabled retained component after degraded commit', async () => {
    api.list
      .mockResolvedValueOnce(currentCatalog(false))
      .mockResolvedValueOnce(currentCatalog(true));
    api.enable.mockResolvedValue(committedMutationResult());
    render(<PluginPanel />);
    fireEvent.click(await screen.findByTitle('Enable'));
    expect(await screen.findByTitle('Disable')).toBeTruthy();
    expect(await screen.findByText(/已提交/)).toBeTruthy();
    expect(api.list).toHaveBeenCalledTimes(2);
  });
  it('keeps the original rollback error and catalog without fabricating a commit', async () => {
    api.list.mockResolvedValue(currentCatalog(false));
    api.enable.mockResolvedValue(committedMutationResult('failed'));
    render(<PluginPanel />);
    fireEvent.click(await screen.findByTitle('Enable'));
    expect(await screen.findByText('rollback completed')).toBeTruthy();
    expect(screen.getByTitle('Enable')).toBeTruthy();
    expect(api.list).toHaveBeenCalledTimes(1);
    expect(screen.queryByText(/已提交/)).toBeNull();
  });
  it('reloads committed degraded configuration from server values', async () => {
    api.list
      .mockResolvedValueOnce(currentCatalog(true))
      .mockResolvedValueOnce(currentCatalog(true, 'server-published'));
    api.configure.mockResolvedValue(committedMutationResult());
    render(<PluginPanel />);
    const toggle = await screen.findByTitle('Disable');
    const expand = toggle.parentElement?.parentElement?.querySelector('button');
    if (!expand) throw new Error('plugin card header missing');
    fireEvent.click(expand);
    const input = await screen.findByRole('textbox', { name: 'Endpoint' });
    fireEvent.change(input, { target: { value: 'requested' } });
    fireEvent.click(screen.getByRole('button', { name: /^Save$/ }));
    await waitFor(() =>
      expect((screen.getByRole('textbox', { name: 'Endpoint' }) as HTMLInputElement).value).toBe(
        'server-published'
      )
    );
    expect(await screen.findByText(/已提交/)).toBeTruthy();
    expect(api.list).toHaveBeenCalledTimes(2);
    expect(screen.queryByText('Configuration failed')).toBeNull();
  });
  it('refreshes a committed degraded uninstall without claiming complete settlement', async () => {
    const confirm = vi.spyOn(window, 'confirm').mockReturnValue(true);
    try {
      api.list
        .mockResolvedValueOnce(currentCatalog(true))
        .mockResolvedValueOnce({ plugins: [], omitted: 0 });
      api.uninstall.mockResolvedValue(committedMutationResult());
      render(<PluginPanel />);
      fireEvent.click(await screen.findByTitle('Uninstall'));
      await waitFor(() => expect(api.list).toHaveBeenCalledTimes(2));
      expect(screen.queryByText('mixed-plugin')).toBeNull();
      expect(await screen.findByText(/已提交/)).toBeTruthy();
    } finally {
      confirm.mockRestore();
    }
  });
});
