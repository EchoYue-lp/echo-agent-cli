// @vitest-environment jsdom
import { fireEvent, render, screen, waitFor } from '@testing-library/react';
import { beforeEach, describe, expect, it, vi } from 'vitest';

const mocks = vi.hoisted(() => ({
  create: vi.fn(),
  listConfigured: vi.fn(),
}));

vi.mock('../../api/endpoints', () => ({
  providerApi: { listConfigured: mocks.listConfigured },
}));

vi.mock('../../stores/conversationStore', () => ({
  useConversationStore: (selector: (state: unknown) => unknown) =>
    selector({ createSideConversation: mocks.create }),
}));

import { SideConversationDialog } from './SideConversationDialog';

describe('SideConversationDialog', () => {
  beforeEach(() => {
    vi.clearAllMocks();
    mocks.listConfigured.mockResolvedValue({
      default_model_id: 'provider:default',
      models: [
        {
          id: 'provider:default',
          display_name: 'Default',
          provider: 'provider',
          model: 'default',
          enabled: true,
        },
        {
          id: 'provider:disabled',
          display_name: 'Disabled',
          provider: 'provider',
          model: 'disabled',
          enabled: false,
        },
      ],
    });
    mocks.create.mockResolvedValue({ conversation_id: 'side-1' });
  });

  it('submits a prompt, title, and selected child-local model', async () => {
    const onClose = vi.fn();
    render(<SideConversationDialog isOpen onClose={onClose} />);

    await waitFor(() => expect(mocks.listConfigured).toHaveBeenCalled());
    fireEvent.change(screen.getByRole('textbox', { name: '要处理的问题' }), {
      target: { value: '验证另一个实现方向' },
    });
    fireEvent.change(screen.getByRole('textbox', { name: '标题' }), {
      target: { value: '替代方案' },
    });
    fireEvent.change(screen.getByRole('combobox', { name: '模型' }), {
      target: { value: 'provider:default' },
    });
    fireEvent.click(screen.getByRole('button', { name: '创建' }));

    await waitFor(() =>
      expect(mocks.create).toHaveBeenCalledWith({
        requestId: expect.any(String),
        prompt: '验证另一个实现方向',
        title: '替代方案',
        modelId: 'provider:default',
      })
    );
    expect(onClose).toHaveBeenCalled();
    expect(screen.queryByText('Disabled')).toBeNull();
  });

  it('reuses the draft request identity after a transport failure', async () => {
    mocks.create
      .mockRejectedValueOnce(new Error('response lost'))
      .mockResolvedValueOnce({ conversation_id: 'side-1' });
    const onClose = vi.fn();
    render(<SideConversationDialog isOpen onClose={onClose} />);
    fireEvent.change(screen.getByRole('textbox', { name: '要处理的问题' }), {
      target: { value: '保持同一个创建意图' },
    });

    fireEvent.click(screen.getByRole('button', { name: '创建' }));
    await screen.findByText('response lost');
    fireEvent.click(screen.getByRole('button', { name: '创建' }));
    await waitFor(() => expect(mocks.create).toHaveBeenCalledTimes(2));

    const first = mocks.create.mock.calls[0]?.[0];
    const second = mocks.create.mock.calls[1]?.[0];
    expect(first?.requestId).toBeTruthy();
    expect(second?.requestId).toBe(first?.requestId);
  });
});
