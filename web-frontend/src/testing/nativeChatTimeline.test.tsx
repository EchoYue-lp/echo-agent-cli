// @vitest-environment jsdom
// 合成回归控制，只证明JS消费/真实组件/边界行为；不写性能样本或宣称backend捕获。
import { beforeEach, afterEach, describe, expect, it, vi } from 'vitest';
vi.hoisted(() => localStorage.setItem('echo-theme', 'light'));
import {
  createTimelineMeasurement,
  parseTimelineFixture,
  operations,
  type TimelineFixture,
} from './nativeChatTimeline';
import { useChatStore } from '../stores/chatStore';
import { useToolExecutionStore } from '../stores/toolExecutionStore';
import { useConversationStore } from '../stores/conversationStore';
import { useTaskRuntimeStore } from '../stores/taskRuntimeStore';
import { useSubagentRunStore } from '../stores/subagentRunStore';
import type {
  AgentEvent,
  ChatEventEnvelope,
  NativeFinalAnswerSource,
  SavedMessage,
  ToolExecution,
} from '../types/api';

const identity = {
  fixture_sha256: 'fixture',
  source_sha256: 'source',
  dataset_sha256: 'dataset',
  protocol_sha256: 'protocol',
  environment_id: 'environment',
  process_id: 'test-process',
};
function fixture(): TimelineFixture {
  let seq = 0;
  const envelope = (payload: AgentEvent, eventId?: string): ChatEventEnvelope => {
    seq += 1;
    return {
      schema_version: 1,
      workspace_id: 'w',
      conversation_id: 'c',
      root_turn_id: 't',
      turn_id: 't',
      event_id: `outer-${seq}`,
      content_hash: 'synthetic-only',
      sequence: seq,
      stream_id: 'stream',
      message_id: 'm',
      timestamp: '2026-10-29T00:00:00Z',
      payload: {
        source: 'agent',
        event: {
          schema_version: 1,
          event_id: eventId ?? `pub-${seq}`,
          content_hash: 'synthetic-only',
          sequence: seq,
          stream_id: 'stream',
          conversation_id: 'c',
          run_id: null,
          turn_id: 't',
          message_id: 'm',
          execution_id: 'e',
          parent_event_id: null,
          timestamp: '2026-10-29T00:00:00Z',
          payload,
        },
      },
    };
  };
  const block = (
    block_id: string,
    phase: 'commentary' | 'unknown',
    delta: string,
    sequence: number
  ) => ({
    block_id,
    phase,
    delta,
    sequence,
    request_id: 'r',
    complete: true,
    provider: 'test',
    reasoning: null,
  });
  const tool: ToolExecution = {
    id: 'tool-1',
    call_id: 'call-1',
    owner: { kind: 'chat', message_id: 'm' },
    workspace_id: 'w',
    conversation_id: 'c',
    run_id: null,
    name: 'read_file',
    args_preview: '{}',
    status: 'running',
    started_at: 1,
    finished_at: null,
    duration_ms: null,
    detail_ref: '',
  };
  const status = {
    execution_id: 'e',
    request_id: 'r',
    sequence: 7,
    phase: 'Done',
    status: 'settled' as const,
    reason: null,
  };
  const source: NativeFinalAnswerSource = {
    execution_id: 'e',
    request_id: 'r',
    sequence: 7,
    blocks: [
      {
        request_id: 'r',
        block_id: 'answer',
        sequence: 2,
        publication_event_id: 'pub-answer',
      },
    ],
  };
  const final: SavedMessage = {
    role: 'assistant',
    content: '最终正文',
    created_at: '2026-10-29T00:00:00Z',
    final_answer_confirmed: true,
    execution_status: status,
    final_answer_source: source,
  };
  return {
    kind: 'native-chat-timeline',
    input_class: 'synthetic-control',
    workspace_id: 'w',
    conversation_id: 'c',
    message_id: 'm',
    execution_id: 'e',
    source_sha256: 'source',
    stages: [
      {
        operation: 'mount',
        actions: [
          {
            kind: 'agent',
            envelope: envelope({
              type: 'assistant_message',
              data: { block: block('progress', 'commentary', '检查文件', 1) },
            }),
          },
        ],
        expected: {
          content: '',
          finalAnswerConfirmed: false,
          blockIds: ['progress'],
          tools: [],
          visible: [{ text: '检查文件', count: 1 }],
          absent: ['最终正文'],
        },
      },
      {
        operation: 'single-row',
        actions: [
          {
            kind: 'agent',
            envelope: envelope({
              type: 'tool_call',
              data: {
                call_id: 'call-1',
                invocation: {
                  requested_name: 'read_file',
                  requested_args: {},
                  name: 'read_file',
                  args: {},
                },
              },
            }),
          },
          { kind: 'tool', event: 'started', summary: tool },
          {
            kind: 'agent',
            envelope: envelope({
              type: 'tool_result',
              data: {
                call_id: 'call-1',
                name: 'read_file',
                result: {
                  kind: { kind_type: 'text' },
                  success: true,
                  output: '工具原始结果',
                  truncated: false,
                  metadata: {},
                },
              },
            }),
          },
          {
            kind: 'tool',
            event: 'finished',
            summary: { ...tool, status: 'succeeded', finished_at: 2 },
          },
          {
            kind: 'agent',
            envelope: envelope(
              {
                type: 'assistant_message',
                data: { block: block('answer', 'unknown', '最终正文', 2) },
              },
              'pub-answer'
            ),
          },
        ],
        expected: {
          content: '',
          finalAnswerConfirmed: false,
          blockIds: ['progress', 'answer'],
          tools: [{ id: 'tool-1', status: 'succeeded' }],
          visible: [
            { text: '检查文件', count: 1 },
            { text: '最终正文', count: 1 },
            { text: '已执行 1 个工具', count: 1 },
          ],
          absent: ['状态未恢复'],
        },
      },
      {
        operation: 'batch',
        actions: [
          {
            kind: 'agent',
            envelope: envelope({ type: 'execution_status', data: status }),
          },
        ],
        expected: {
          content: '',
          finalAnswerConfirmed: false,
          executionStatus: status,
          blockIds: ['progress', 'answer'],
          tools: [{ id: 'tool-1', status: 'succeeded' }],
          visible: [{ text: '最终正文', count: 1 }],
          absent: [],
        },
      },
      {
        operation: 'run-state',
        actions: [
          {
            kind: 'agent',
            envelope: envelope({ type: 'final_answer', data: '最终正文' }),
          },
          { kind: 'publication', message: final },
        ],
        expected: {
          content: '最终正文',
          finalAnswerConfirmed: true,
          executionStatus: status,
          finalAnswerSource: source,
          blockIds: ['progress', 'answer'],
          tools: [{ id: 'tool-1', status: 'succeeded' }],
          visible: [
            { text: '最终正文', count: 1 },
            { text: '检查文件', count: 1 },
          ],
          absent: ['状态未恢复'],
        },
      },
    ],
  };
}
const read = (f = fixture()) => parseTimelineFixture(JSON.stringify(f));
const testOptions = { functionalTest: true, watchdogMs: 500 };
beforeEach(() => {
  useChatStore.getState().clearMessages();
  useToolExecutionStore.getState().clear();
  useTaskRuntimeStore.getState().reset();
  useSubagentRunStore.getState().clear();
  useConversationStore.setState({ activeId: null });
  vi.stubGlobal(
    'fetch',
    vi.fn(() => {
      throw new Error('不允许请求网络');
    })
  );
});
afterEach(() => {
  vi.restoreAllMocks();
  vi.unstubAllGlobals();
});

describe('现有native probe的真实聊天组件producer', () => {
  it('合成控制不可进入原生测量入口', async () => {
    await expect(createTimelineMeasurement(read(), identity)).rejects.toThrow('合成控制');
    expect(document.querySelector('[aria-label="原生聊天时间线性能观察"]')).toBeNull();
  });
  it('四操作复用原handler/store/MessageBubble并保独立DOM提交边界', async () => {
    const source = fixture();
    const original = JSON.stringify(source);
    const api = await createTimelineMeasurement(read(source), identity, testOptions);
    for (const operation of operations) {
      await api.prepare(operation, 'measurement', 0);
      const proof = await api.execute();
      expect(proof.scenario).toBe(`gui/NativeMessageTimeline/${operation}`);
      expect(proof.correctness.input_class).toBe('synthetic-control');
      expect(proof.boundaries.react_commits).toBeGreaterThan(0);
      expect(proof.boundaries.dom_ready_ms).toBeGreaterThanOrEqual(
        proof.boundaries.dispatch?.finished_ms ?? Infinity
      );
      expect(proof.boundaries.os_paint).toBe('not_measured');
      expect(proof.boundaries.transaction_ack).toBe('native probe only');
      if (operation === 'batch')
        expect(useChatStore.getState().messages.at(0)?.finalAnswerConfirmed).toBe(false);
      if (operation === 'run-state')
        expect(document.body.textContent?.split('最终正文').length).toBe(2);
      await api.cleanup();
    }
    api.finish();
    expect(JSON.stringify(source)).toBe(original);
    expect(useChatStore.getState().messages).toEqual([]);
    expect(useToolExecutionStore.getState().tools).toEqual({});
    expect(fetch).not.toHaveBeenCalled();
  });
  it('错误的publication身份不产生成功样本且清理临时root', async () => {
    const f = fixture();
    const stage = f.stages.at(-1);
    const publication = stage?.actions.find((a) => a.kind === 'publication');
    if (
      !publication ||
      publication.kind !== 'publication' ||
      !publication.message.final_answer_source
    )
      throw new Error('控制缺失');
    publication.message.final_answer_source.sequence += 1;
    const api = await createTimelineMeasurement(read(f), identity, testOptions);
    await api.prepare('run-state', 'measurement', 0);
    await expect(api.execute()).rejects.toThrow('publication');
    expect(() => api.snapshot()).toThrow();
    expect(document.querySelector('[aria-label="原生聊天时间线性能观察"]')).toBeNull();
    expect(useChatStore.getState().messages).toEqual([]);
    expect(fetch).not.toHaveBeenCalled();
  });
  it('publication只来自已确认的原get DTO，不能给status升级发布权', () => {
    const f = fixture();
    const publication = f.stages.at(-1)?.actions.find((a) => a.kind === 'publication');
    if (!publication || publication.kind !== 'publication') throw new Error('控制缺失');
    publication.message.final_answer_confirmed = false;
    expect(() => read(f)).toThrow('原get publication');
  });
  it('拒绝跨会话工具owner及会触发后端的非Agent envelope', () => {
    const f = fixture();
    const tool = f.stages.at(1)?.actions.find((a) => a.kind === 'tool');
    if (!tool || tool.kind !== 'tool') throw new Error('控制缺失');
    tool.summary.conversation_id = 'other';
    expect(() => read(f)).toThrow('工具owner');
    const other = fixture();
    const agent = other.stages.at(0)?.actions.at(0);
    if (!agent || agent.kind !== 'agent') throw new Error('控制缺失');
    agent.envelope.conversation_id = 'other';
    expect(() => read(other)).toThrow('scope');
  });
  it('重复execute、未完成cleanup及取消后snapshot均拒绝，保原空状态', async () => {
    const api = await createTimelineMeasurement(read(), identity, testOptions);
    await expect(api.cleanup()).rejects.toThrow();
    await api.prepare('mount', 'warmup', 0);
    await api.execute();
    await expect(api.execute()).rejects.toThrow();
    api.abort();
    expect(() => api.snapshot()).toThrow();
    expect(useChatStore.getState().messages).toEqual([]);
    expect(document.querySelector('[aria-label="原生聊天时间线性能观察"]')).toBeNull();
  });
  it('DOM oracle失败即拒绝并恢复class/store，不把rAF或超时当绘制完成', async () => {
    document.documentElement.setAttribute('class', 'original');
    const f = fixture();
    const first = f.stages.at(0);
    if (!first) throw new Error('控制缺失');
    first.expected.absent.push('检查文件');
    const api = await createTimelineMeasurement(read(f), identity, testOptions);
    await api.prepare('mount', 'measurement', 0);
    await expect(api.execute()).rejects.toThrow('未公开');
    expect(document.documentElement.getAttribute('class')).toBe('original');
    expect(useChatStore.getState().messages).toEqual([]);
  });
  it('已有聊天不能被测量夹具接管，原数据不变', async () => {
    useChatStore.getState().startAssistantMessage('existing');
    const before = JSON.stringify(useChatStore.getState().messages);
    await expect(createTimelineMeasurement(read(), identity, testOptions)).rejects.toThrow(
      '已有聊天'
    );
    expect(JSON.stringify(useChatStore.getState().messages)).toBe(before);
  });
  it('prepare前缀消费失败也进入terminal并恢复空现场', async () => {
    const api = await createTimelineMeasurement(read(), identity, testOptions);
    vi.spyOn(useChatStore.getState(), 'appendAssistantBlock').mockImplementationOnce(() => {
      throw new Error('prefix failure');
    });
    await expect(api.prepare('single-row', 'measurement', 0)).rejects.toThrow('prefix failure');
    await expect(api.prepare('mount', 'measurement', 0)).rejects.toThrow('失败');
    expect(() => api.snapshot()).toThrow();
    expect(document.querySelector('[aria-label="原生聊天时间线性能观察"]')).toBeNull();
    expect(useChatStore.getState().messages).toEqual([]);
  });
  it('cleanup watchdog失败后不能重用旧成功snapshot', async () => {
    const api = await createTimelineMeasurement(read(), identity, {
      ...testOptions,
      watchdogMs: 20,
    });
    await api.prepare('mount', 'measurement', 0);
    await api.execute();
    vi.spyOn(useChatStore.getState(), 'clearMessages').mockImplementationOnce(() => undefined);
    await expect(api.cleanup()).rejects.toThrow('watchdog');
    expect(() => api.snapshot()).toThrow();
    expect(document.querySelector('[aria-label="原生聊天时间线性能观察"]')).toBeNull();
    expect(useChatStore.getState().messages).toEqual([]);
  });
  it('prepare在途取消立即拒绝等待并恢复现场', async () => {
    const api = await createTimelineMeasurement(read(), identity, testOptions);
    const pending = api.prepare('mount', 'measurement', 0);
    api.abort();
    await expect(pending).rejects.toThrow('取消');
    expect(() => api.snapshot()).toThrow();
    expect(document.querySelector('[aria-label="原生聊天时间线性能观察"]')).toBeNull();
    expect(useChatStore.getState().messages).toEqual([]);
  });
  it('离线基准在组件挂载前拒绝自动媒体子资源', () => {
    const f = fixture();
    const first = f.stages.at(0)?.actions.at(0);
    if (
      !first ||
      first.kind !== 'agent' ||
      first.envelope.payload.source !== 'agent' ||
      first.envelope.payload.event.payload.type !== 'assistant_message'
    )
      throw new Error('控制缺失');
    first.envelope.payload.event.payload.data.block.delta =
      '![remote](https://example.invalid/image.png)';
    expect(() => read(f)).toThrow('自动媒体子资源');
  });
  it('同block分段拼接的Markdown图片在挂载前也拒绝', () => {
    const f = fixture();
    const first = f.stages.at(0)?.actions.at(0);
    if (
      !first ||
      first.kind !== 'agent' ||
      first.envelope.payload.source !== 'agent' ||
      first.envelope.payload.event.payload.type !== 'assistant_message'
    )
      throw new Error('控制缺失');
    const second = structuredClone(first);
    first.envelope.payload.event.payload.data.block.delta = '!';
    if (
      second.envelope.payload.source !== 'agent' ||
      second.envelope.payload.event.payload.type !== 'assistant_message'
    )
      throw new Error('控制缺失');
    second.envelope.payload.event.payload.data.block.delta =
      '[remote](https://example.invalid/image.png)';
    f.stages.at(0)?.actions.push(second);
    expect(() => read(f)).toThrow('分段自动媒体');
  });
});
