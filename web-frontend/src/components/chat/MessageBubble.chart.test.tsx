// @vitest-environment jsdom
import { cleanup, fireEvent, render, screen } from '@testing-library/react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
vi.hoisted(() =>
  Object.defineProperty(window, 'matchMedia', {
    configurable: true,
    value: () => ({ matches: false }),
  })
);
vi.mock('vega-embed', () => ({ default: vi.fn(async () => ({ finalize: vi.fn() })) }));
vi.mock('vega-interpreter', () => ({ expressionInterpreter: {} }));
import { MessageBubble } from './MessageBubble';
import type { ChatMessage } from '../../types/api';
import { useChatStore } from '../../stores/chatStore';
import { useSubagentRunStore } from '../../stores/subagentRunStore';
import { useTaskRuntimeStore } from '../../stores/taskRuntimeStore';
import { useToolExecutionStore } from '../../stores/toolExecutionStore';
beforeEach(() => {
  useChatStore.setState({ messages: [], isStreaming: false });
  useSubagentRunStore.getState().clear();
  useTaskRuntimeStore.getState().reset();
  useToolExecutionStore.getState().clear();
});
afterEach(cleanup);
describe('原chartSpecs进入实际消息渲染', () => {
  it('图表专用、正文及最终结果三类原消息保留原spec和正文', () => {
    const spec = {
      $schema: 'https://vega.github.io/schema/vega-lite/v5.json',
      mark: 'bar',
      data: { values: [{ name: '原图表数据🙂', value: 7 }] },
    };
    const base: ChatMessage = {
      id: 'chart-only',
      role: 'assistant',
      content: '',
      timestamp: 1,
      isStreaming: false,
      chartSpecs: [spec],
    };
    for (const message of [
      base,
      { ...base, id: 'body-chart', content: '公开正文' },
      { ...base, id: 'final-chart', content: '最终正文', finalAnswerConfirmed: true },
    ]) {
      const { unmount } = render(<MessageBubble message={message} />);
      expect(screen.getByRole('group', { name: '图表' })).not.toBeNull();
      fireEvent.click(screen.getByText('查看图表数据'));
      expect(screen.getByText(/原图表数据🙂/)).not.toBeNull();
      if (message.content) expect(screen.getByText(message.content)).not.toBeNull();
      unmount();
    }
  });
});
