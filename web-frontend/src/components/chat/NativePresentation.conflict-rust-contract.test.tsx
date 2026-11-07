// @vitest-environment jsdom
import { cleanup, render, screen } from '@testing-library/react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import cold from '../../fixtures/native-presentation-conflict.json';
vi.hoisted(() =>
  Object.defineProperty(window, 'matchMedia', {
    configurable: true,
    value: () => ({ matches: false }),
  })
);
const mocks = vi.hoisted(() => ({ get: vi.fn(), restore: vi.fn(), tools: vi.fn() }));
vi.mock('../../api/endpoints', () => ({
  conversationApi: { get: mocks.get, restore: mocks.restore },
  toolExecutionApi: { list: mocks.tools },
  workspaceApi: { list: vi.fn() },
  sessionApi: { reset: vi.fn() },
}));
vi.mock('vega-embed', () => ({ default: vi.fn(async () => ({ finalize: vi.fn() })) }));
vi.mock('vega-interpreter', () => ({ expressionInterpreter: {} }));
import { useConversationStore } from '../../stores/conversationStore';
import { useWorkspaceStore } from '../../stores/workspaceStore';
import { useChatStore } from '../../stores/chatStore';
import { useSubagentRunStore } from '../../stores/subagentRunStore';
import { useToolExecutionStore } from '../../stores/toolExecutionStore';
import { useTaskRuntimeStore } from '../../stores/taskRuntimeStore';
import { ConversationTimeline } from './ConversationTimeline';
beforeEach(() => {
  vi.clearAllMocks();
  useWorkspaceStore.setState({ current: null, isLoading: false });
  useConversationStore.setState({
    workspaceId: 'target-workspace',
    activeId: null,
    isLoading: false,
  });
  useChatStore.getState().clearMessages();
  useSubagentRunStore.getState().clear();
  useToolExecutionStore.getState().clear();
  useTaskRuntimeStore.getState().reset();
  mocks.get.mockResolvedValue({ messages: cold.messages });
  mocks.restore.mockResolvedValue({ success: true });
  mocks.tools.mockResolvedValue([]);
});
afterEach(cleanup);
describe('真实双journal冲突DTO进入原loader与React', () => {
  it('保留原进度与缺详情，拒绝恢复冲突产物及最终折叠', async () => {
    const errors = vi.spyOn(console, 'error').mockImplementation(() => undefined);
    try {
      expect(cold.verification).toContain('real imported history transport');
      expect(cold.messages).toHaveLength(3);
      expect(cold.messages.every((message) => message.presentation_unavailable)).toBe(true);
      expect(await useConversationStore.getState().loadConversation('target-conversation')).toBe(
        true
      );
      expect(mocks.get).toHaveBeenCalledWith('target-workspace', 'target-conversation');
      const messages = useChatStore.getState().messages;
      expect(messages).toHaveLength(3);
      expect(messages.every((message) => message.presentationUnavailable)).toBe(true);
      expect(messages.flatMap((message) => message.chartSpecs ?? [])).toHaveLength(0);
      expect(Object.values(useSubagentRunStore.getState().runs)).toHaveLength(0);
      render(
        <ConversationTimeline
          messages={messages}
          containerKey="actual-rust-conflict"
          targetScope={{ workspaceId: 'target-workspace', conversationId: 'target-conversation' }}
        />
      );
      expect(screen.getAllByText('各自已核验的原进度')).toHaveLength(3);
      expect(screen.getByText('部分图表或执行位置未恢复')).not.toBeNull();
      expect(screen.queryByRole('group', { name: '图表' })).toBeNull();
      expect(screen.queryByRole('button', { name: '展开执行过程' })).toBeNull();
      expect(errors).not.toHaveBeenCalled();
    } finally {
      errors.mockRestore();
    }
  });
});
