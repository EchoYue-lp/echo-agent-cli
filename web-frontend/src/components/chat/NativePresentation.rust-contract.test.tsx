// @vitest-environment jsdom
import { cleanup, fireEvent, render, screen } from '@testing-library/react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import cold from '../../fixtures/native-presentation-cold.json';
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
import { projectNativeTimeline } from './NativeMessageTimeline';
beforeEach(() => {
  vi.clearAllMocks();
  useWorkspaceStore.setState({ current: null, isLoading: false });
  useConversationStore.setState({
    workspaceId: cold.cold.source.workspace_id,
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
describe('真实Rust裁剪冷恢复DTO进入原conversationStore与React', () => {
  it('保持原cursor交错、共享补充、子运行及最终结果外的图表', async () => {
    const source = cold.cold.source;
    expect(await useConversationStore.getState().loadConversation(source.conversation_id)).toBe(
      true
    );
    expect(mocks.get).toHaveBeenCalledWith(source.workspace_id, source.conversation_id);
    const messages = useChatStore.getState().messages;
    expect(messages).toHaveLength(3);
    expect(
      messages.every(
        (message) => message.presentationRecords === messages.at(0)?.presentationRecords
      )
    ).toBe(true);
    const projection = projectNativeTimeline(
      messages.slice(0, 2),
      Object.values(useSubagentRunStore.getState().runs),
      useToolExecutionStore.getState().tools
    );
    expect(projection.items.map((item) => item.type)).toEqual(['body', 'process', 'body']);
    const middle = projection.items.at(1);
    expect(middle?.type === 'process' ? middle.entries.map((entry) => entry.type) : []).toEqual([
      'tool',
      'subagent',
    ]);
    render(
      <ConversationTimeline
        messages={messages}
        containerKey="actual-rust-cold"
        targetScope={{ workspaceId: source.workspace_id, conversationId: source.conversation_id }}
      />
    );
    expect(screen.getByText('最终结果独立显示')).not.toBeNull();
    expect(screen.queryByText('先核对来源')).toBeNull();
    expect(screen.getAllByRole('group', { name: '图表' })).toHaveLength(1);
    fireEvent.click(screen.getByRole('button', { name: '展开执行过程' }));
    fireEvent.click(screen.getByRole('button', { name: '展开中间过程' }));
    expect(screen.getByText('先核对来源')).not.toBeNull();
    expect(screen.getByText('已得到子运行记录')).not.toBeNull();
    expect(screen.getAllByText('child-agent').length).toBeGreaterThan(0);
    expect(document.body.textContent).not.toContain('do-not-copy-this-argument');
  });
});
