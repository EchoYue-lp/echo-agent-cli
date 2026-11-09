// @vitest-environment jsdom
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { cleanup, fireEvent, render, screen, waitFor } from '@testing-library/react';
import { ConfigPanel } from './ConfigPanel';

const api = vi.hoisted(() => ({ get: vi.fn(), getFull: vi.fn(), updateFull: vi.fn() }));
vi.mock('../../api/endpoints', () => ({ configApi: api }));
vi.hoisted(() => {
  Object.defineProperty(window, 'matchMedia', {
    configurable: true,
    value: () => ({ matches: false }),
  });
});

function fullConfig(depth: number = 1) {
  return {
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
  };
}

describe('EKO subagent depth configuration', () => {
  afterEach(cleanup);
  beforeEach(() => {
    vi.clearAllMocks();
    api.get.mockResolvedValue(fullConfig().agent);
    api.getFull.mockResolvedValue(fullConfig());
    api.updateFull.mockImplementation(async (request) => fullConfig(request.max_subagent_depth));
  });

  it('shows only 1, 2 and 3, defaults to 1 and explains new-run scope', async () => {
    render(<ConfigPanel />);
    const select = await screen.findByRole('combobox', { name: '最大子智能体层数' });
    expect((select as HTMLSelectElement).value).toBe('1');
    expect(Array.from(select.querySelectorAll('option'), (option) => option.value)).toEqual([
      '1',
      '2',
      '3',
    ]);
    expect(screen.getByText(/对新任务生效/)).toBeTruthy();
  });

  it('基础页不重复渠道配置，也不再放置IM渠道入口', async () => {
    const { useUiStore } = await import('../../stores/uiStore');
    useUiStore.setState({ settingsOpen: true, activeSettingsTab: 'config' });
    render(<ConfigPanel />);
    await screen.findByRole('combobox', { name: '最大子智能体层数' });
    expect(screen.queryByRole('heading', { name: 'QQ 机器人' })).toBeNull();
    expect(screen.queryByRole('heading', { name: '飞书' })).toBeNull();
    expect(screen.queryByText('客户端密钥')).toBeNull();
    expect(screen.queryByText('应用密钥')).toBeNull();
    expect(screen.queryByRole('heading', { name: 'IM 渠道' })).toBeNull();
    expect(screen.queryByRole('button', { name: '前往IM渠道' })).toBeNull();
    expect(useUiStore.getState().activeSettingsTab).toBe('config');
    expect(useUiStore.getState().settingsOpen).toBe(true);
    expect(api.updateFull).not.toHaveBeenCalled();
    useUiStore.setState({ settingsOpen: false, activeSettingsTab: 'providers' });
  });

  it('does not expose or submit a fixed Agent iteration ceiling', async () => {
    render(<ConfigPanel />);
    await screen.findByRole('combobox', { name: '最大子智能体层数' });
    expect(screen.queryByRole('button', { name: /高级运行参数/ })).toBeNull();
    expect(screen.queryByText('单轮推理安全上限')).toBeNull();
    fireEvent.change(screen.getByRole('combobox', { name: '最大子智能体层数' }), {
      target: { value: '2' },
    });
    fireEvent.click(screen.getByRole('button', { name: '保存' }));
    await waitFor(() =>
      expect(api.updateFull).toHaveBeenCalledWith(
        { max_subagent_depth: 2 },
        expect.any(AbortSignal)
      )
    );
  });

  it('saves the selected limit through the existing owned config endpoint', async () => {
    render(<ConfigPanel />);
    fireEvent.change(await screen.findByRole('combobox', { name: '最大子智能体层数' }), {
      target: { value: '2' },
    });
    fireEvent.click(screen.getByRole('button', { name: '保存' }));
    await waitFor(() =>
      expect(api.updateFull).toHaveBeenCalledWith(
        { max_subagent_depth: 2 },
        expect.any(AbortSignal)
      )
    );
    expect(await screen.findByText('已保存')).toBeTruthy();
  });

  it('freezes the depth edit until its pending save settles', async () => {
    let finish: ((value: ReturnType<typeof fullConfig>) => void) | undefined;
    api.updateFull.mockImplementation(
      () =>
        new Promise<ReturnType<typeof fullConfig>>((resolve) => {
          finish = resolve;
        })
    );
    render(<ConfigPanel />);
    const select = (await screen.findByRole('combobox', {
      name: '最大子智能体层数',
    })) as HTMLSelectElement;
    fireEvent.change(select, { target: { value: '3' } });
    fireEvent.click(screen.getByRole('button', { name: '保存' }));
    await waitFor(() => expect(api.updateFull).toHaveBeenCalledTimes(1));
    expect(select.disabled).toBe(true);
    finish?.(fullConfig(3));
    expect(await screen.findByText('已保存')).toBeTruthy();
    expect(select.value).toBe('3');
    await waitFor(() => expect(select.disabled).toBe(false));
  });

  it('keeps the edit retryable and does not report success when persistence fails', async () => {
    api.updateFull.mockRejectedValue(new Error('disk write failed'));
    render(<ConfigPanel />);
    fireEvent.change(await screen.findByRole('combobox', { name: '最大子智能体层数' }), {
      target: { value: '3' },
    });
    fireEvent.click(screen.getByRole('button', { name: '保存' }));
    expect(await screen.findByText(/保存失败/)).toBeTruthy();
    expect(screen.queryByText('已保存')).toBeNull();
    expect((screen.getByRole('button', { name: '保存' }) as HTMLButtonElement).disabled).toBe(
      false
    );
  });

  it('does not accept a successful transport response that did not save the chosen value', async () => {
    api.updateFull.mockResolvedValue(fullConfig(1));
    render(<ConfigPanel />);
    fireEvent.change(await screen.findByRole('combobox', { name: '最大子智能体层数' }), {
      target: { value: '3' },
    });
    fireEvent.click(screen.getByRole('button', { name: '保存' }));
    expect(await screen.findByText('保存失败')).toBeTruthy();
    expect(screen.queryByText('已保存')).toBeNull();
  });

  it('rejects an invalid server projection instead of silently clamping to the default', async () => {
    api.getFull.mockResolvedValue(fullConfig(4));
    render(<ConfigPanel />);
    expect(await screen.findByText(/配置加载失败/)).toBeTruthy();
    expect(screen.queryByRole('combobox', { name: '最大子智能体层数' })).toBeNull();
  });
});
