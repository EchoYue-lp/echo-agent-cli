// @vitest-environment jsdom
import { cleanup, fireEvent, render, waitFor } from '@testing-library/react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { ProviderPanel } from './ProviderPanel';

const mocks = vi.hoisted(() => ({
  listProviders: vi.fn(),
  listConfigured: vi.fn(),
  upsertProvider: vi.fn(),
  deleteProvider: vi.fn(),
  upsertConfigured: vi.fn(),
  deleteConfigured: vi.fn(),
  setDefault: vi.fn(),
  test: vi.fn(),
}));

vi.mock('../../api/endpoints', () => ({ providerApi: mocks }));

const provider = {
  id: 'team-gateway',
  name: 'Team Gateway',
  base_url: 'https://gateway.example/v1',
  api_key_env: 'TEAM_LLM_KEY',
  requires_api_key: true,
  default_api_protocol: 'responses' as const,
  has_auth_token: true,
  auth_source: 'config',
  model_count: 1,
};

const model = {
  id: 'team-gateway:model-a',
  display_name: 'Model A',
  provider: 'team-gateway',
  model: 'model-a',
  api_protocol: 'responses' as const,
  input_modalities: ['text', 'image'] as const,
  enabled: true,
  is_default: false,
  has_auth_token: true,
  auth_source: 'config',
  base_url: provider.base_url,
  temperature: null,
  max_tokens: null,
  context_window: null,
  thinking_levels: ['none', 'low', 'medium', 'high', 'xhigh', 'max'],
};

describe('ProviderPanel', () => {
  afterEach(cleanup);

  beforeEach(() => {
    for (const mock of Object.values(mocks)) mock.mockReset();
    mocks.listProviders.mockResolvedValue({ providers: [provider] });
    mocks.listConfigured.mockResolvedValue({ models: [model], default_model_id: null });
    mocks.upsertProvider.mockResolvedValue({ success: true, provider_id: provider.id });
    mocks.upsertConfigured.mockResolvedValue({ success: true, model_id: model.id });
    mocks.setDefault.mockResolvedValue({
      success: true,
      model_id: model.id,
      display_name: model.display_name,
      model: model.model,
      provider: provider.id,
    });
  });

  it('saves an arbitrary provider instead of selecting a built-in template', async () => {
    const { getByLabelText, getByRole } = render(<ProviderPanel />);
    await waitFor(() => getByRole('button', { name: '添加 Provider' }));
    fireEvent.click(getByRole('button', { name: '添加 Provider' }));
    fireEvent.change(getByLabelText('Provider 名称'), { target: { value: 'Local Lab' } });
    fireEvent.change(getByLabelText('API 根地址'), {
      target: { value: 'http://127.0.0.1:11434/v1' },
    });
    fireEvent.click(getByRole('button', { name: '保存' }));

    await waitFor(() =>
      expect(mocks.upsertProvider).toHaveBeenCalledWith(
        expect.objectContaining({
          id: expect.stringMatching(/^provider-/),
          name: 'Local Lab',
          base_url: 'http://127.0.0.1:11434/v1',
          default_api_protocol: 'chat_completions',
        })
      )
    );
  });

  it('shows only user-facing provider and model identifiers', async () => {
    const { findByLabelText, queryByLabelText, queryByText } = render(<ProviderPanel />);

    expect(await findByLabelText('API 模型名称')).toBeTruthy();
    expect(queryByLabelText('Provider ID')).toBeNull();
    expect(queryByLabelText('显示名称')).toBeNull();
    expect(queryByLabelText('环境变量')).toBeNull();
    expect(queryByLabelText('必须提供 API Key')).toBeNull();
    expect(queryByText('新模型默认协议')).toBeNull();
  });

  it('persists per-model protocol and multimodal capabilities', async () => {
    const { getByLabelText, getByRole } = render(<ProviderPanel />);
    await waitFor(() => getByLabelText('API 模型名称'));
    fireEvent.change(getByLabelText('API 模型名称'), { target: { value: 'vision-model' } });
    fireEvent.click(getByRole('button', { name: 'Chat Completions' }));
    fireEvent.click(getByLabelText('图片'));
    fireEvent.click(getByLabelText('音频'));
    fireEvent.click(getByLabelText('视频'));
    fireEvent.click(getByRole('button', { name: '保存并启用' }));

    await waitFor(() =>
      expect(mocks.upsertConfigured).toHaveBeenCalledWith(
        expect.objectContaining({
          provider: provider.id,
          model: 'vision-model',
          api_protocol: 'chat_completions',
          input_modalities: ['text', 'image', 'audio', 'video'],
          set_default: true,
        })
      )
    );
  });

  it('shows the thinking levels inferred by the backend for a configured model', async () => {
    const { findByText } = render(<ProviderPanel />);
    expect(await findByText('思考: 自动 / 关闭 / 低 / 中 / 高 / 很高 / 最高')).toBeTruthy();
  });

  it('surfaces provider deletion validation from the linearized backend path', async () => {
    mocks.deleteProvider.mockRejectedValueOnce({
      kind: 'validation',
      message: "Provider 'team-gateway' still has configured models",
    });
    const { findByText, getByRole } = render(<ProviderPanel />);
    const remove = await waitFor(() => getByRole('button', { name: '删除 Provider' }));
    fireEvent.click(remove);
    expect(
      await findByText("删除失败: Provider 'team-gateway' still has configured models")
    ).toBeTruthy();
  });
});
