// 原Root完整六模式测试通过后导出的实际JSON；不合成Chart事件或spec。
// @vitest-environment jsdom
import { cleanup, fireEvent, render, screen, waitFor } from '@testing-library/react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import original from '../../fixtures/standard-chart-real-root.json';
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
import { handleChatEventEnvelope } from '../../hooks/chatEventHandler';
import { captureConversationView, useConversationStore } from '../../stores/conversationStore';
import { useChatStore } from '../../stores/chatStore';
import { useWorkspaceStore } from '../../stores/workspaceStore';
import { useSubagentRunStore } from '../../stores/subagentRunStore';
import { useToolExecutionStore } from '../../stores/toolExecutionStore';
import { useTaskRuntimeStore } from '../../stores/taskRuntimeStore';
import { ConversationTimeline } from './ConversationTimeline';
import type { ChatEventEnvelope } from '../../types/api';
const events = original.events as unknown as ChatEventEnvelope[]; // 原Rust serde合同，下面核原schema/来源，不改写字段。
const first = events.at(0);
if (!first?.conversation_id) throw new Error('真实Root fixture缺原会话');
const workspaceId = first.workspace_id,
  conversationId = first.conversation_id,
  rootId = first.root_turn_id;
const ctx = {
  assistantIdRef: { current: rootId as string | null },
  currentMessageKeyRef: { current: rootId as string | null },
  currentMessageIdRef: { current: rootId as string | null },
  isCancelledRef: { current: false },
  currentThinkingIdRef: { current: null as string | null },
  captureProjection: captureConversationView,
};
function View() {
  const messages = useChatStore((state) => state.messages);
  return (
    <ConversationTimeline
      messages={messages}
      containerKey="actual-standard-chart"
      targetScope={{ workspaceId, conversationId }}
    />
  );
}
beforeEach(() => {
  vi.clearAllMocks();
  useChatStore.getState().clearMessages();
  useSubagentRunStore.getState().clear();
  useToolExecutionStore.getState().clear();
  useTaskRuntimeStore.getState().reset();
  useWorkspaceStore.setState({
    current: {
      id: workspaceId,
      name: 'controlled',
      root: '/controlled',
      project_root: '/controlled',
      kind: { type: 'local' },
      metadata: {
        tags: [],
        project_location: { kind: 'local', path: '/controlled' },
        project_execution_location: { kind: 'local', path: '/controlled' },
      },
      product_data_generation: 'actual-root-dto',
      created_at: '2026-11-07T00:00:00Z',
      last_active: '2026-11-07T00:00:00Z',
    },
    isLoading: false,
  });
  useConversationStore.setState({ workspaceId, activeId: conversationId, newConversationEpoch: 0 });
  useChatStore.getState().startAssistantMessage(rootId);
  ctx.assistantIdRef.current = rootId;
  ctx.currentMessageKeyRef.current = rootId;
  ctx.isCancelledRef.current = false;
  mocks.get.mockResolvedValue({ messages: original.after_batch_messages });
  mocks.restore.mockResolvedValue({ success: true });
  mocks.tools.mockResolvedValue([]);
});
afterEach(cleanup);
describe('真实标准工具Root数据消费', () => {
  it('原保存后get显示实际tool spec；原final外保留，cold原loader一致', async () => {
    expect(
      events.every((event) => event.schema_version === 2 && event.payload.source === 'agent')
    ).toBe(true);
    expect(
      events.some(
        (event) => event.payload.source === 'agent' && event.payload.event.payload.type === 'chart'
      )
    ).toBe(false);
    const batch = events.findIndex((event) => event.sequence === original.batch_sequence);
    expect(batch).toBeGreaterThan(-1);
    for (const envelope of events.slice(0, batch + 1)) handleChatEventEnvelope(envelope, ctx);
    render(<View />);
    await waitFor(() => expect(screen.getAllByRole('group', { name: '图表' })).toHaveLength(1));
    expect(mocks.get).toHaveBeenCalledWith(workspaceId, conversationId);
    fireEvent.click(screen.getByText('查看图表数据'));
    expect(JSON.parse(screen.getByText(/\$schema/).textContent ?? 'null')).toEqual(
      original.actual_spec
    );
    mocks.get.mockResolvedValue({ messages: original.cold_messages });
    for (const envelope of events.slice(batch + 1)) handleChatEventEnvelope(envelope, ctx);
    expect(screen.getAllByRole('group', { name: '图表' })).toHaveLength(1);
    await waitFor(() => {
      expect(screen.getByText('原标准图表工具已结束')).not.toBeNull();
      expect(
        screen.getByRole('button', { name: '展开执行过程' }).getAttribute('aria-expanded')
      ).toBe('false');
    });
    expect(screen.getAllByRole('group', { name: '图表' })).toHaveLength(1);
    expect(screen.queryByText('我会用原工具生成公开图表')).toBeNull();
    const beforeColdReads = mocks.get.mock.calls.length;
    cleanup();
    expect(await useConversationStore.getState().loadConversation(conversationId)).toBe(true);
    expect(mocks.get).toHaveBeenCalledTimes(beforeColdReads + 1);
    render(<View />);
    expect(screen.getAllByRole('group', { name: '图表' })).toHaveLength(1);
    expect(screen.getByText('原标准图表工具已结束')).not.toBeNull();
    expect(screen.getByRole('button', { name: '展开执行过程' }).getAttribute('aria-expanded')).toBe(
      'false'
    );
    expect(screen.queryByText('我会用原工具生成公开图表')).toBeNull();
  });
});
