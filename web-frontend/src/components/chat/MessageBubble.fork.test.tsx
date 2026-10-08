// @vitest-environment jsdom
import { cleanup, fireEvent, render, screen } from '@testing-library/react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { useSubagentRunStore } from '../../stores/subagentRunStore';
import { useTaskRuntimeStore } from '../../stores/taskRuntimeStore';
import type { ChatMessage } from '../../types/api';
import { MessageBubble } from './MessageBubble';

const reply: ChatMessage = {
  id: 'completed-reply',
  role: 'assistant',
  content: '已经完成的回复',
  timestamp: 1,
  isStreaming: false,
};

describe('reply Fork action', () => {
  beforeEach(() => {
    useSubagentRunStore.getState().clear();
    useTaskRuntimeStore.getState().reset();
  });
  afterEach(cleanup);

  it('offers a separate Fork action alongside copy and regenerate', () => {
    const fork = vi.fn();
    const regenerate = vi.fn();
    render(<MessageBubble message={reply} onFork={fork} onRegenerate={regenerate} />);
    expect(screen.getByRole('button', { name: '复制' })).toBeDefined();
    expect(screen.getByRole('button', { name: '重新生成' })).toBeDefined();
    fireEvent.click(screen.getByRole('button', { name: '分叉会话' }));
    expect(fork).toHaveBeenCalledExactlyOnceWith('completed-reply');
    expect(regenerate).not.toHaveBeenCalled();
  });

  it('prevents another Fork while the conversation is busy', () => {
    const fork = vi.fn();
    render(<MessageBubble message={reply} onFork={fork} forkDisabled />);
    const button = screen.getByRole('button', { name: '分叉会话' });
    expect(button.hasAttribute('disabled')).toBe(true);
    fireEvent.click(button);
    expect(fork).not.toHaveBeenCalled();
  });

  it('does not offer Fork on a streaming reply or internal Agent message', () => {
    const { rerender } = render(
      <MessageBubble message={{ ...reply, isStreaming: true }} onFork={vi.fn()} />
    );
    expect(screen.queryByRole('button', { name: '分叉会话' })).toBeNull();
    rerender(<MessageBubble message={{ ...reply, internalAgent: true }} onFork={vi.fn()} />);
    expect(screen.queryByRole('button', { name: '分叉会话' })).toBeNull();
  });
});
