// @vitest-environment jsdom
// 这里只复现原consumer缺刷新；不是standard ToolResult生产者或图表spec证据。
import { beforeEach, describe, expect, it, vi } from 'vitest';
import { waitFor } from '@testing-library/react';
vi.hoisted(() =>
  Object.defineProperty(window, 'matchMedia', {
    configurable: true,
    value: () => ({ matches: false }),
  })
);
const mocks = vi.hoisted(() => ({ get: vi.fn() }));
vi.mock('../../api/endpoints', () => ({ conversationApi: { get: mocks.get } }));
import { handleChatEventEnvelope } from '../../hooks/chatEventHandler';
import { useChatStore } from '../../stores/chatStore';
import { useConversationStore } from '../../stores/conversationStore';
import { useWorkspaceStore } from '../../stores/workspaceStore';
import type { AgentEvent, ChatEventEnvelope } from '../../types/api';
const ctx = {
  assistantIdRef: { current: 'turn' as string | null },
  currentMessageKeyRef: { current: 'turn' as string | null },
  currentMessageIdRef: { current: 'turn' as string | null },
  isCancelledRef: { current: false },
  currentThinkingIdRef: { current: null as string | null },
  captureProjection: () => () => true,
};
function event(sequence: number, payload: AgentEvent): ChatEventEnvelope {
  return {
    schema_version: 2,
    event_id: 'outer-' + sequence,
    content_hash: 'outer-hash-' + sequence,
    stream_id: 'original-stream',
    sequence,
    workspace_id: 'global',
    conversation_id: 'conversation',
    root_turn_id: 'turn',
    turn_id: 'turn',
    message_id: 'turn',
    timestamp: '2026-11-07T00:00:00Z',
    payload: {
      source: 'agent',
      event: {
        schema_version: 2,
        event_id: 'native-' + sequence,
        content_hash: 'native-hash-' + sequence,
        sequence,
        stream_id: 'native-stream',
        conversation_id: 'conversation',
        run_id: null,
        turn_id: 'turn',
        message_id: 'turn',
        execution_id: 'execution',
        parent_event_id: null,
        timestamp: '2026-11-07T00:00:00Z',
        payload,
      },
    },
  };
}
beforeEach(() => {
  vi.clearAllMocks();
  useWorkspaceStore.setState({ current: null, isLoading: false });
  useConversationStore.setState({
    workspaceId: 'global',
    activeId: 'conversation',
    newConversationEpoch: 0,
  });
  useChatStore.getState().clearMessages();
  useChatStore.getState().startAssistantMessage('turn');
  ctx.assistantIdRef.current = 'turn';
  ctx.currentMessageKeyRef.current = 'turn';
  ctx.isCancelledRef.current = false;
  ctx.captureProjection = () => () => true;
  mocks.get.mockResolvedValue({ messages: [] });
});
describe('原成功保存事实后的readonly图表补充读取', () => {
  it('已见原generate_chart调用后，真实handler入口在有效settled触发既有get', async () => {
    const args = { chart_type: 'bar', x_field: 'x', y_field: 'y', data: [{ x: '甲', y: 1 }] };
    handleChatEventEnvelope(
      event(1, {
        type: 'tool_call',
        data: {
          call_id: 'chart-call',
          invocation: {
            requested_name: 'generate_chart',
            requested_args: args,
            name: 'generate_chart',
            args,
            rewrites: [],
          },
        },
      }),
      ctx
    );
    handleChatEventEnvelope(
      event(2, {
        type: 'transcript_projection_settlement',
        data: {
          status: 'settled',
          operation_id: 'original-save-op',
          generation_id: 'original-save-generation',
          conversation_id: 'conversation',
          attempt: 1,
          error_class: null,
          detail: null,
        },
      }),
      ctx
    );
    await waitFor(() => expect(mocks.get).toHaveBeenCalledTimes(1));
    expect(mocks.get).toHaveBeenCalledWith('global', 'conversation');
  });
});

function chartCall() {
  const args = { chart_type: 'bar', x_field: 'x', y_field: 'y', data: [{ x: '甲', y: 1 }] };
  return event(1, {
    type: 'tool_call',
    data: {
      call_id: 'chart-call',
      invocation: {
        requested_name: 'generate_chart',
        requested_args: args,
        name: 'generate_chart',
        args,
        rewrites: [],
      },
    },
  });
}
function saved(
  overrides: Partial<Extract<AgentEvent, { type: 'transcript_projection_settlement' }>['data']> = {}
) {
  return event(2, {
    type: 'transcript_projection_settlement',
    data: {
      status: 'settled',
      operation_id: 'original-save-op',
      generation_id: 'original-save-generation',
      conversation_id: 'conversation',
      attempt: 1,
      error_class: null,
      detail: null,
      ...overrides,
    },
  });
}
// 以下是纯读取/生命周期单元反控，不是标准工具producer、Rust getter或真实spec证据。
function projectedResponseForFenceTest() {
  const message = useChatStore.getState().messages.find((entry) => entry.id === 'turn');
  const call = message?.presentationRecords?.at(0);
  if (!message?.turnSource || !call) throw new Error('原ingress调用见证缺失');
  const chart = {
    ...call,
    event_id: 'controlled-projection-ref',
    content_hash: 'controlled-projection-hash',
    cursor: { ...call.cursor, sequence: 2 },
    payload: {
      kind: 'chart',
      data: {
        execution_id: 'execution',
        publication_event_id: 'controlled-view-only',
        spec: { description: 'pure view fence test, not standardproducer proof' },
      },
    },
  };
  return {
    messages: [
      {
        role: 'assistant',
        content: '',
        created_at: '2026-11-07T00:00:00Z',
        turn_source: message.turnSource,
        native_presentation: {
          source: message.turnSource,
          journal_identity: 'controlled-unit-journal',
          through_sequence: 2,
          first_observed_sequence: 1,
          prefix_complete: true,
          records: [call, chart],
        },
      },
    ],
  };
}
function charts() {
  return useChatStore
    .getState()
    .messages.flatMap((message) => message.presentationRecords ?? [])
    .filter((record) => record.payload.kind === 'chart');
}

describe('只读补件原view生命周期', () => {
  it('当前原调用见证可接受补充；重复同settled只读一次且不复制记录', async () => {
    handleChatEventEnvelope(chartCall(), ctx);
    mocks.get.mockResolvedValue(projectedResponseForFenceTest());
    const settled = saved();
    handleChatEventEnvelope(settled, ctx);
    await waitFor(() => expect(charts()).toHaveLength(1));
    handleChatEventEnvelope(settled, ctx);
    await Promise.resolve();
    expect(mocks.get).toHaveBeenCalledTimes(1);
    expect(charts()).toHaveLength(1);
  });
  it.each([
    { status: 'deferred' as const },
    { operation_id: null },
    { generation_id: null },
    { attempt: 0 },
    { conversation_id: 'foreign' },
  ])('未确认/无覆盖资格的保存不读取: %j', async (invalid) => {
    handleChatEventEnvelope(chartCall(), ctx);
    handleChatEventEnvelope(saved(invalid), ctx);
    await Promise.resolve();
    expect(mocks.get).not.toHaveBeenCalled();
  });
  it('没有原工具调用见证不从同一保存事实猜图表', async () => {
    handleChatEventEnvelope(saved(), ctx);
    await Promise.resolve();
    expect(mocks.get).not.toHaveBeenCalled();
  });
  it.each(['retired', 'conversation', 'generation'])(
    '迟到响应不写已退役视图: %s',
    async (change) => {
      handleChatEventEnvelope(chartCall(), ctx);
      const response = projectedResponseForFenceTest();
      let resolve: ((value: typeof response) => void) | undefined;
      let alive = true;
      ctx.captureProjection = () => () => alive;
      mocks.get.mockReturnValue(
        new Promise<typeof response>((done) => {
          resolve = done;
        })
      );
      handleChatEventEnvelope(saved(), ctx);
      expect(mocks.get).toHaveBeenCalledTimes(1);
      if (change === 'retired') alive = false;
      if (change === 'conversation')
        useConversationStore.setState({ activeId: 'other-conversation' });
      if (change === 'generation')
        useWorkspaceStore.setState({
          current: {
            id: 'global',
            name: 'global',
            root: '/controlled',
            project_root: '/controlled',
            kind: { type: 'local' },
            metadata: {
              tags: [],
              project_location: { kind: 'local', path: '/controlled' },
              project_execution_location: { kind: 'local', path: '/controlled' },
            },
            product_data_generation: 'changed',
            created_at: '2026-11-07T00:00:00Z',
            last_active: '2026-11-07T00:00:00Z',
          },
        });
      resolve?.(response);
      await Promise.resolve();
      await Promise.resolve();
      expect(charts()).toHaveLength(0);
    }
  );
  it('原轮次结束不退役仍属同view的只读补件，不依赖活动执行refs', async () => {
    handleChatEventEnvelope(chartCall(), ctx);
    const response = projectedResponseForFenceTest();
    let resolve: ((value: typeof response) => void) | undefined;
    mocks.get.mockReturnValue(
      new Promise<typeof response>((done) => {
        resolve = done;
      })
    );
    handleChatEventEnvelope(saved(), ctx);
    ctx.currentMessageKeyRef.current = null;
    ctx.assistantIdRef.current = null;
    resolve?.(response);
    await waitFor(() => expect(charts()).toHaveLength(1));
  });
});
