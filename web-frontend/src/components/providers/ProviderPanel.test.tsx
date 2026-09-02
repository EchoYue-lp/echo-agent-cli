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
  is_default: true,
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
    mocks.listConfigured.mockResolvedValue({
      models: [model],
      default_model_id: model.id,
    });
    mocks.upsertProvider.mockResolvedValue({ success: true, provider_id: provider.id });
    mocks.upsertConfigured.mockResolvedValue({ success: true, model_id: model.id });
    mocks.deleteProvider.mockResolvedValue({ success: true });
    mocks.deleteConfigured.mockResolvedValue({ success: true });
    mocks.test.mockResolvedValue({ success: true, model: model.model });
  });

  it('shows every provider with its nested models and no permanent form', async () => {
    const { findByText, getByRole, queryByLabelText, queryByRole } = render(<ProviderPanel />);

    expect(await findByText('Team Gateway')).toBeTruthy();
    expect(await findByText('Model A')).toBeTruthy();
    expect(getByRole('button', { name: '编辑 Team Gateway' })).toBeTruthy();
    expect(getByRole('button', { name: '为 Team Gateway 添加模型' })).toBeTruthy();
    expect(getByRole('button', { name: '删除 Team Gateway' })).toBeTruthy();
    expect(getByRole('button', { name: '编辑 Model A' })).toBeTruthy();
    expect(getByRole('button', { name: '删除 Model A' })).toBeTruthy();
    expect(queryByLabelText('Provider 名称')).toBeNull();
    expect(queryByLabelText('API 模型名称')).toBeNull();
    expect(queryByRole('button', { name: '启用' })).toBeNull();
  });

  it('adds only provider information from the page action', async () => {
    const { getByLabelText, getByRole } = render(<ProviderPanel />);
    await waitFor(() => getByRole('button', { name: '添加 Provider' }));

    fireEvent.click(getByRole('button', { name: '添加 Provider' }));
    fireEvent.change(getByLabelText('Provider 名称'), { target: { value: 'Local Lab' } });
    fireEvent.change(getByLabelText('API 根地址'), {
      target: { value: 'http://127.0.0.1:11434/v1' },
    });
    expect(getByRole('dialog', { name: '新增 Provider' })).toBeTruthy();
    expect(() => getByLabelText('API 模型名称')).toThrow();
    fireEvent.click(getByRole('button', { name: '保存 Provider' }));

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

  it('edits provider information without exposing internal identifiers', async () => {
    const { getByLabelText, getByRole, queryByLabelText } = render(<ProviderPanel />);
    await waitFor(() => getByRole('button', { name: '编辑 Team Gateway' }));

    fireEvent.click(getByRole('button', { name: '编辑 Team Gateway' }));
    expect((getByLabelText('Provider 名称') as HTMLInputElement).value).toBe('Team Gateway');
    expect(queryByLabelText('Provider ID')).toBeNull();
    expect(queryByLabelText('环境变量')).toBeNull();
    fireEvent.click(getByRole('button', { name: '保存 Provider' }));

    await waitFor(() =>
      expect(mocks.upsertProvider).toHaveBeenCalledWith(
        expect.objectContaining({
          id: provider.id,
          api_key_env: provider.api_key_env,
          requires_api_key: true,
        })
      )
    );
  });

  it('adds a model under the selected provider with its own protocol and capabilities', async () => {
    const { getByLabelText, getByRole } = render(<ProviderPanel />);
    await waitFor(() => getByRole('button', { name: '为 Team Gateway 添加模型' }));

    fireEvent.click(getByRole('button', { name: '为 Team Gateway 添加模型' }));
    expect(getByRole('dialog', { name: '新增模型' })).toBeTruthy();
    fireEvent.change(getByLabelText('API 模型名称'), { target: { value: 'vision-model' } });
    fireEvent.click(getByRole('button', { name: 'Chat Completions' }));
    fireEvent.click(getByLabelText('图片'));
    fireEvent.click(getByLabelText('音频'));
    fireEvent.click(getByLabelText('视频'));
    fireEvent.click(getByRole('button', { name: '保存模型' }));

    await waitFor(() =>
      expect(mocks.upsertConfigured).toHaveBeenCalledWith(
        expect.objectContaining({
          provider: provider.id,
          model: 'vision-model',
          api_protocol: 'chat_completions',
          input_modalities: ['text', 'image', 'audio', 'video'],
          set_default: false,
        })
      )
    );
  });

  it('opens an existing model in the independent model editor', async () => {
    const { getByLabelText, getByRole } = render(<ProviderPanel />);
    await waitFor(() => getByRole('button', { name: '编辑 Model A' }));

    fireEvent.click(getByRole('button', { name: '编辑 Model A' }));
    expect(getByRole('dialog', { name: '编辑模型' })).toBeTruthy();
    expect((getByLabelText('API 模型名称') as HTMLInputElement).value).toBe('model-a');
    expect((getByLabelText('图片') as HTMLInputElement).checked).toBe(true);
  });

  it('summarizes inferred thinking levels without expanding every option', async () => {
    const { findByText, queryByText } = render(<ProviderPanel />);

    expect(await findByText('思考 6 档')).toBeTruthy();
    expect(queryByText('思考: 自动 / 关闭 / 低 / 中 / 高 / 很高 / 最高')).toBeNull();
  });

  it('confirms that deleting a provider also deletes its models', async () => {
    const { findByText, getByRole } = render(<ProviderPanel />);
    await waitFor(() => getByRole('button', { name: '删除 Team Gateway' }));

    fireEvent.click(getByRole('button', { name: '删除 Team Gateway' }));
    expect(await findByText('确定删除“Team Gateway”及其 1 个模型吗？')).toBeTruthy();
    fireEvent.click(getByRole('button', { name: /^删除$/ }));

    await waitFor(() => expect(mocks.deleteProvider).toHaveBeenCalledWith(provider.id));
  });

  it('keeps model deletion as a separate model action', async () => {
    const { findByText, getByRole } = render(<ProviderPanel />);
    await waitFor(() => getByRole('button', { name: '删除 Model A' }));

    fireEvent.click(getByRole('button', { name: '删除 Model A' }));
    expect(await findByText('确定删除“Model A”吗？')).toBeTruthy();
    fireEvent.click(getByRole('button', { name: /^删除$/ }));

    await waitFor(() => expect(mocks.deleteConfigured).toHaveBeenCalledWith(model.id));
  });
});
