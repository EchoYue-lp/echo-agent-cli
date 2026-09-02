// @vitest-environment jsdom
import { cleanup, render, waitFor } from '@testing-library/react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

const mocks = vi.hoisted(() => ({
  listConfigured: vi.fn(),
  setThinking: vi.fn(),
  getPermissionMode: vi.fn(),
  workspaceId: 'workspace-a',
}));

vi.mock('../../api/endpoints', () => ({
  providerApi: {
    listConfigured: mocks.listConfigured,
    setThinking: mocks.setThinking,
    setDefault: vi.fn(),
  },
  permissionsApi: {
    getMode: mocks.getPermissionMode,
    setMode: vi.fn(),
  },
}));

vi.mock('../../stores/workspaceStore', () => ({
  useWorkspaceStore: (selector: (state: unknown) => unknown) =>
    selector({ current: { id: mocks.workspaceId } }),
}));

vi.mock('../../stores/uiStore', () => ({
  useUiStore: (selector: (state: unknown) => unknown) =>
    selector({ setActiveSettingsTab: vi.fn() }),
}));

import { ChatInput } from './ChatInput';

describe('ChatInput thinking publication', () => {
  afterEach(cleanup);

  beforeEach(() => {
    vi.clearAllMocks();
    mocks.workspaceId = 'workspace-a';
    localStorage.setItem('echo_thinking_level', 'high');
    mocks.getPermissionMode.mockResolvedValue({ mode: 'default' });
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
          temperature: null,
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
});
