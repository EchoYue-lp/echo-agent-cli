// @vitest-environment jsdom
import { cleanup, fireEvent, render, screen } from '@testing-library/react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import type {
  AgentEvent,
  ChatEventEnvelope,
  ChatMessage,
  NativePresentationSupplement,
  ToolExecution,
} from '../../types/api';
import type { ExecEvent } from '../../generated';
import { handleChatEventEnvelope } from '../../hooks/chatEventHandler';
import { useChatStore } from '../../stores/chatStore';
import { useSubagentRunStore } from '../../stores/subagentRunStore';
import { useTaskRuntimeStore } from '../../stores/taskRuntimeStore';
import { useToolExecutionStore } from '../../stores/toolExecutionStore';
import { savedPresentationBySource } from '../../lib/nativePresentation';
import { projectNativeTimeline } from './NativeMessageTimeline';
import { ConversationTimeline } from './ConversationTimeline';

vi.mock('vega-embed', () => ({ default: vi.fn(async () => ({ finalize: vi.fn() })) }));
vi.mock('vega-interpreter', () => ({ expressionInterpreter: {} }));
vi.hoisted(() =>
  Object.defineProperty(window, 'matchMedia', {
    configurable: true,
    value: () => ({ matches: false }),
  })
);
const stream = 'original-journal';
let sequence = 0;
const ctx = {
  assistantIdRef: { current: 'live' as string | null },
  currentMessageKeyRef: { current: 'turn' as string | null },
  currentMessageIdRef: { current: 'turn' as string | null },
  isCancelledRef: { current: false },
  currentThinkingIdRef: { current: null as string | null },
};

function envelope(payload: AgentEvent | ExecEvent, execution = false): ChatEventEnvelope {
  sequence += 1;
  return {
    schema_version: 2,
    event_id: `original-${sequence}`,
    content_hash: `hash-${sequence}`,
    stream_id: stream,
    sequence,
    workspace_id: 'workspace',
    conversation_id: 'conversation',
    root_turn_id: 'turn',
    turn_id: execution ? 'child' : 'turn',
    message_id: 'turn',
    timestamp: '2026-11-07T00:00:00Z',
    payload: execution
      ? { source: 'execution', event: payload as ExecEvent }
      : {
          source: 'agent',
          event: {
            schema_version: 2,
            event_id: `native-${sequence}`,
            content_hash: `native-hash-${sequence}`,
            sequence,
            stream_id: 'native-main',
            conversation_id: 'conversation',
            run_id: null,
            turn_id: 'turn',
            message_id: 'turn',
            execution_id: 'parent',
            parent_event_id: null,
            timestamp: '2026-11-07T00:00:00Z',
            payload: payload as AgentEvent,
          },
        },
  };
}

function body(id: string, text: string): AgentEvent {
  return {
    type: 'assistant_message',
    data: {
      block: {
        block_id: id,
        request_id: `request-${id}`,
        sequence: 0,
        phase: 'commentary',
        delta: text,
        complete: true,
        provider: 'controlled',
        reasoning: null,
      },
    },
  };
}

function child(parent = 'parent'): ExecEvent {
  return {
    workspace_id: 'workspace',
    conversation_id: 'conversation',
    run_id: 'run',
    scope: 'subagent',
    task_id: 'task',
    subagent_run_id: 'child',
    event: 'started',
    agent: '原子代理',
    payload: { message_id: 'turn', task: '检查原序' },
    framework_event: {
      schema_version: 2,
      event_id: 'child-start',
      content_hash: 'child-hash',
      sequence: 1,
      stream_id: 'child-stream',
      turn_id: 'child',
      message_id: 'turn',
      execution_id: 'child',
      parent_event_id: 'native-2',
      timestamp: '2026-11-07T00:00:00Z',
      parent_agent: 'main',
      agent_name: '原子代理',
      parent_execution_id: parent,
      agent_path: null,
      task_id: 'task',
      attempt: 1,
      plan_revision: 1,
    },
  };
}

function seed(): ChatMessage {
  useChatStore.getState().startAssistantMessage('live');
  handleChatEventEnvelope(envelope(body('a', '进度 A')), ctx);
  handleChatEventEnvelope(
    envelope({
      type: 'tool_call',
      data: {
        call_id: 'provider-call',
        invocation: {
          requested_name: 'agent_tool',
          requested_args: {},
          name: 'agent_tool',
          args: {},
          rewrites: [],
        },
      },
    }),
    ctx
  );
  useToolExecutionStore.getState().replaceAll([
    {
      id: 'tool-id',
      call_id: 'provider-call',
      workspace_id: 'workspace',
      conversation_id: 'conversation',
      run_id: 'run',
      owner: { kind: 'chat', message_id: 'live' },
      name: 'agent_tool',
      args_preview: '{}',
      status: 'unknown',
      started_at: 1,
      detail_ref: '',
    } as ToolExecution,
  ]);
  useChatStore.getState().recordToolStart('live', 'tool-id');
  handleChatEventEnvelope(envelope(child(), true), ctx);
  const chart = envelope({
    type: 'chart',
    data: { spec: { mark: 'bar', data: { values: [{ name: '原始图表数据', value: 7 }] } } },
  });
  handleChatEventEnvelope(chart, ctx);
  handleChatEventEnvelope(chart, ctx);
  handleChatEventEnvelope(envelope(body('b', '进度 B')), ctx);
  const message = useChatStore.getState().messages.find((message) => message.id === 'live');
  if (!message) throw new Error('live message missing');
  return message;
}

beforeEach(() => {
  sequence = 0;
  useChatStore.getState().clearMessages();
  useSubagentRunStore.getState().clear();
  useTaskRuntimeStore.getState().reset();
  useToolExecutionStore.getState().clear();
});
afterEach(cleanup);

describe('原journal呈现引用', () => {
  it('原handler实时合并tool与Subagent到A/B之间一个入口，图表独立且重复运输不复制', () => {
    const message = seed();
    const projected = projectNativeTimeline(
      message,
      Object.values(useSubagentRunStore.getState().runs),
      useToolExecutionStore.getState().tools
    );
    expect(projected.items.map((item) => item.type)).toEqual(['body', 'process', 'body']);
    const middle = projected.items.at(1);
    expect(middle?.type === 'process' ? middle.entries.map((entry) => entry.type) : []).toEqual([
      'tool',
      'subagent',
    ]);
    render(
      <ConversationTimeline
        messages={[message]}
        containerKey="target"
        targetScope={{ workspaceId: 'workspace', conversationId: 'conversation' }}
      />
    );
    expect(screen.getAllByRole('button', { name: '展开中间过程' })).toHaveLength(1);
    expect(screen.getAllByRole('group', { name: '图表' })).toHaveLength(1);
    fireEvent.click(screen.getByRole('button', { name: '展开中间过程' }));
    expect(document.body.textContent?.indexOf('进度 A')).toBeLessThan(
      document.body.textContent?.indexOf('原子代理') ?? -1
    );
    expect(document.body.textContent?.indexOf('原子代理')).toBeLessThan(
      document.body.textContent?.indexOf('进度 B') ?? -1
    );
  });

  it('同源冷行拆分仍保持相同交错，确认final后图表与最终答案仍在整体过程之外', () => {
    const live = seed();
    const a: ChatMessage = {
      ...live,
      id: 'cold-a',
      assistantBlocks: live.assistantBlocks?.slice(0, 1),
      executionSteps: [
        { type: 'assistant', index: 0 },
        { type: 'tool', callId: 'tool-id' },
      ],
    };
    const b: ChatMessage = {
      ...live,
      id: 'cold-b',
      assistantBlocks: live.assistantBlocks?.slice(1),
      executionSteps: [{ type: 'assistant', index: 0 }],
      presentationRecords: [],
    };
    const final: ChatMessage = {
      id: 'final',
      role: 'assistant',
      content: '原已确认最终答案',
      timestamp: 1,
      turnSource: live.turnSource,
      finalAnswerConfirmed: true,
      finalAnswerExecutionId: 'parent',
      isStreaming: false,
    };
    const projection = projectNativeTimeline(
      [a, b],
      Object.values(useSubagentRunStore.getState().runs),
      useToolExecutionStore.getState().tools
    );
    expect(projection.items.map((item) => item.type)).toEqual(['body', 'process', 'body']);
    render(
      <ConversationTimeline
        messages={[a, b, final]}
        containerKey="fork-target"
        targetScope={{ workspaceId: 'other-workspace', conversationId: 'fork' }}
      />
    );
    expect(screen.queryByText('进度 A')).toBeNull();
    expect(screen.getByText('原已确认最终答案')).not.toBeNull();
    expect(screen.getAllByRole('group', { name: '图表' })).toHaveLength(1);
    fireEvent.click(screen.getByRole('button', { name: '展开执行过程' }));
    fireEvent.click(screen.getByRole('button', { name: '展开中间过程' }));
    expect(screen.getByText('原子代理')).not.toBeNull();
    expect(screen.getByTitle('查看来源结果摘要；完整日志请打开来源会话')).not.toBeNull();
  });

  it('child缺原cursor只显示位置未恢复，不按同时间或最后消息猜位置', () => {
    const message = seed();
    const runs = Object.values(useSubagentRunStore.getState().runs).map((run) => ({
      ...run,
      startedAt: -999999,
      events: run.events.map((event) => ({ ...event, journal_sequence: undefined })),
    }));
    const projection = projectNativeTimeline(message, runs, useToolExecutionStore.getState().tools);
    expect(projection.unplaced).toHaveLength(1);
    const middle = projection.items.at(1);
    expect(middle?.type === 'process' ? middle.entries.length : -1).toBe(1);
    const foreign = runs.map((run) => ({
      ...run,
      events: run.events.map((event) => ({
        ...event,
        framework_event: event.framework_event
          ? { ...event.framework_event, parent_execution_id: 'other-parent' }
          : undefined,
      })),
    }));
    expect(projectNativeTimeline(message, foreign, {}).subagents).toHaveLength(0);
  });

  it('一份冷补充共享source，跨journal generation冲突保持未恢复', () => {
    const live = seed();
    if (!live.turnSource) throw new Error('source missing');
    const supplement: NativePresentationSupplement = {
      source: live.turnSource,
      journal_identity: 'generation-1',
      through_sequence: 5,
      first_observed_sequence: 1,
      prefix_complete: true,
      records: live.presentationRecords ?? [],
    };
    const saved = {
      role: 'assistant',
      content: null,
      created_at: '2026-11-07T00:00:00Z',
      turn_source: live.turnSource,
      native_presentation: supplement,
    };
    const projected = savedPresentationBySource([
      saved,
      { ...saved, native_presentation: { ...supplement, journal_identity: 'foreign-generation' } },
    ]);
    expect([...projected.values()]).toEqual([null]);
  });
});
