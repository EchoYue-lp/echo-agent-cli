// @vitest-environment jsdom
import { cleanup, fireEvent, render, waitFor } from '@testing-library/react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { ProviderPanel } from './ProviderPanel';

const mocks = vi.hoisted(() => ({
  listTemplates: vi.fn(),
  listConfigured: vi.fn(),
  test: vi.fn(),
  upsertConfigured: vi.fn(),
  setDefault: vi.fn(),
  deleteConfigured: vi.fn(),
}));

vi.mock('../../api/endpoints', () => ({
  providerApi: mocks,
}));

const openAiTemplate = {
  id: 'openai',
  name: 'OpenAI',
  base_url: 'https://api.openai.com/v1/responses',
  api_key_env: 'OPENAI_API_KEY',
  default_models: ['gpt-test'],
  requires_api_key: true,
  default_api_protocol: 'responses' as const,
};

const configuredModel = {
  id: 'openai:gpt-test',
  display_name: 'GPT Test',
  provider: 'openai',
  model: 'gpt-test',
  api_protocol: 'responses' as const,
  enabled: true,
  is_default: false,
  has_auth_token: true,
  auth_source: 'config',
  base_url: 'https://api.openai.com/v1/responses',
  temperature: null,
  max_tokens: null,
  context_window: null,
};

describe('ProviderPanel protocol selection', () => {
  afterEach(cleanup);

  beforeEach(() => {
    for (const mock of Object.values(mocks)) mock.mockReset();
    mocks.listTemplates.mockResolvedValue({ providers: [openAiTemplate] });
    mocks.listConfigured.mockResolvedValue({ models: [], default_model_id: null });
    mocks.test.mockResolvedValue({ success: true, model: 'gpt-test', auth_source: 'none' });
  });

  it('keeps the initial provider protocol automatic until the user explicitly selects one', async () => {
    const { getByRole } = render(<ProviderPanel />);
    const automatic = await waitFor(() => getByRole('button', { name: 'Auto' }));
    const responses = getByRole('button', { name: 'Responses' });

    expect(automatic.getAttribute('aria-pressed')).toBe('true');
    expect(responses.getAttribute('aria-pressed')).toBe('false');

    fireEvent.click(responses);
    expect(automatic.getAttribute('aria-pressed')).toBe('false');
    expect(responses.getAttribute('aria-pressed')).toBe('true');

    fireEvent.click(getByRole('button', { name: '测试连接' }));
    await waitFor(() => {
      expect(mocks.test).toHaveBeenCalledWith(
        expect.objectContaining({ api_protocol: 'responses', base_url: undefined })
      );
    });
  });

  it('surfaces backend validation when an explicit protocol conflicts with the endpoint', async () => {
    mocks.test.mockRejectedValueOnce(
      new Error('Configured protocol ChatCompletions does not match endpoint Responses')
    );
    const { findByText, getByRole } = render(<ProviderPanel />);
    await waitFor(() => getByRole('button', { name: 'Auto' }));

    fireEvent.click(getByRole('button', { name: 'Chat Completions' }));
    fireEvent.click(getByRole('button', { name: '测试连接' }));

    expect(
      await findByText(
        '请求失败: Configured protocol ChatCompletions does not match endpoint Responses'
      )
    ).toBeTruthy();
    expect(mocks.test).toHaveBeenCalledWith(
      expect.objectContaining({ api_protocol: 'chat_completions', base_url: undefined })
    );
  });

  it('passes custom complete endpoints to the backend instead of duplicating its parser', async () => {
    mocks.upsertConfigured.mockRejectedValueOnce({
      kind: 'validation',
      message: 'Configured protocol Responses does not match endpoint Anthropic',
    });
    const { findByText, getByDisplayValue, getByRole } = render(<ProviderPanel />);
    await waitFor(() => getByRole('button', { name: 'Auto' }));

    fireEvent.change(getByDisplayValue(openAiTemplate.base_url), {
      target: { value: 'https://gateway.example/v1/messages' },
    });
    fireEvent.click(getByRole('button', { name: 'Responses' }));
    fireEvent.click(getByRole('button', { name: '保存并使用' }));

    expect(
      await findByText('切换失败: Configured protocol Responses does not match endpoint Anthropic')
    ).toBeTruthy();
    expect(mocks.upsertConfigured).toHaveBeenCalledWith(
      expect.objectContaining({
        api_protocol: 'responses',
        base_url: 'https://gateway.example/v1/messages',
        set_default: true,
      })
    );
  });

  it('keeps Auto neutral and allows an incomplete provider root to reach backend metadata resolution', async () => {
    const { getByDisplayValue, getByRole } = render(<ProviderPanel />);
    const automatic = await waitFor(() => getByRole('button', { name: 'Auto' }));

    fireEvent.change(getByDisplayValue(openAiTemplate.base_url), {
      target: { value: 'https://gateway.example/v1' },
    });
    fireEvent.click(getByRole('button', { name: '测试连接' }));

    expect(automatic.getAttribute('aria-pressed')).toBe('true');
    await waitFor(() => {
      expect(mocks.test).toHaveBeenCalledWith(
        expect.objectContaining({
          api_protocol: undefined,
          base_url: 'https://gateway.example/v1',
        })
      );
    });
  });

  it('shows a structured set-default failure without reporting a successful switch', async () => {
    mocks.listConfigured.mockResolvedValue({
      models: [configuredModel],
      default_model_id: null,
    });
    mocks.setDefault.mockRejectedValueOnce({
      kind: 'validation',
      message: 'Invalid Authorization header value',
    });
    const { findByText, getByRole } = render(<ProviderPanel />);
    const setDefault = await waitFor(() => getByRole('button', { name: '设为默认' }));

    fireEvent.click(setDefault);

    expect(await findByText('切换失败: Invalid Authorization header value')).toBeTruthy();
    expect(mocks.setDefault).toHaveBeenCalledWith(configuredModel.id);
  });

  it('shows a structured delete failure and keeps the configured model visible', async () => {
    mocks.listConfigured.mockResolvedValue({
      models: [configuredModel],
      default_model_id: null,
    });
    mocks.deleteConfigured.mockRejectedValueOnce({
      kind: 'internal',
      message: 'Config file is read-only',
    });
    const { findByText, getByRole } = render(<ProviderPanel />);
    const deleteModel = await waitFor(() => getByRole('button', { name: '删除' }));

    fireEvent.click(deleteModel);

    expect(await findByText('删除失败: Config file is read-only')).toBeTruthy();
    expect(await findByText('GPT Test')).toBeTruthy();
  });
});
