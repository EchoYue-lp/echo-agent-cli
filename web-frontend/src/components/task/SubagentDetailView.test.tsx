// @vitest-environment jsdom
import { render, fireEvent, screen, cleanup } from '@testing-library/react';
import { renderToStaticMarkup } from 'react-dom/server';
import { describe, expect, it } from 'vitest';
import type { SubagentRunState } from '../../stores/subagentRunStore';
import {
  SubagentDetailView,
  subagentTimelineEntries,
  visibleSubagentTimelineEntries,
} from './SubagentDetailView';

describe('SubagentDetailView', () => {
  it('renders an inline exact-attempt composer for a running Subagent', () => {
    const run: SubagentRunState = {
      subagentRunId: 'run-1:task-1:3:2:claim-1',
      runId: 'run-1',
      taskId: 'task-1',
      planRevision: 3,
      attempt: 2,
      agent: 'implementer',
      status: 'running',
      startedAt: 1,
      events: [],
    };

    const html = renderToStaticMarkup(<SubagentDetailView run={run} onBack={() => undefined} />);

    expect(html).toContain('aria-label="Subagent 消息"');
    expect(html).toContain('aria-label="发送 Subagent 消息"');
    expect(html).toContain('aria-label="中断 Subagent"');
    expect(html).toContain('attempt 2');
    expect(html).not.toContain('window.prompt');
  });

  it('shows task, execution and result as one stream without tabs', () => {
    const run: SubagentRunState = {
      subagentRunId: 'task-1:1',
      runId: 'run-1',
      taskId: 'task-1',
      agent: 'explorer',
      task: '重构支付模块并补齐退款边界单测',
      status: 'running',
      startedAt: 1,
      events: [],
    };

    const html = renderToStaticMarkup(<SubagentDetailView run={run} onBack={() => undefined} />);

    // Task prompt renders inline (main-chat user-bubble style), no tab shell.
    expect(html).toContain('重构支付模块并补齐退款边界单测');
    expect(html).not.toContain('提示词 / 任务');
    expect(html).not.toContain('aria-pressed');
  });

  it('shows the complete terminal output without protocol metadata', () => {
    const actualResult = '# Complete architecture report\n\n' + 'User-facing details. '.repeat(12);
    const run: SubagentRunState = {
      subagentRunId: 'task-1:1',
      runId: 'run-1',
      taskId: 'task-1',
      agent: 'explorer',
      task: '重构支付模块并补齐退款边界单测',
      status: 'completed',
      startedAt: 1,
      finalOutput: `${actualResult}\n\n## Result\n\n\`\`\`json\n{"contract_version":1,"summary":"done"}\n\`\`\``,
      outcome: {
        contract_version: 0,
        status: 'completed',
        summary: '见上方分析结果。',
        artifacts: [],
        evidence: [],
        verification: [],
        remaining_work: [],
        touched_files: { read: ['Cargo.toml'], written: [] },
      },
      events: [],
    };

    const html = renderToStaticMarkup(<SubagentDetailView run={run} onBack={() => undefined} />);

    expect(html).toContain('Complete architecture report');
    expect(html).toContain('User-facing details');
    // The task prompt stays visible alongside the terminal output.
    expect(html).toContain('重构支付模块并补齐退款边界单测');
    expect(html).not.toContain('见上方分析结果');
    expect(html).not.toContain('## Result');
    expect(html).not.toContain('Cargo.toml');
  });

  it('routes a settled attempt through the follow-up composer presentation', () => {
    const run: SubagentRunState = {
      subagentRunId: 'run-1:task-1:3:2:claim-1',
      runId: 'run-1',
      workspaceId: 'workspace-1',
      taskId: 'task-1',
      planRevision: 3,
      attempt: 2,
      agent: 'reviewer',
      status: 'failed',
      startedAt: 1,
      error: 'provider unavailable',
      outcome: {
        contract_version: 1,
        status: 'failed',
        summary: 'provider unavailable',
        artifacts: [],
        evidence: [],
        verification: [],
        remaining_work: ['provider unavailable'],
        touched_files: { read: [], written: [] },
      },
      events: [],
    };

    const html = renderToStaticMarkup(<SubagentDetailView run={run} onBack={() => undefined} />);

    expect(html).toContain('aria-label="Subagent 后续任务"');
    expect(html).toContain('aria-label="发送 Subagent 后续任务"');
    expect(html.match(/provider unavailable/g)).toHaveLength(1);
    expect(html).not.toContain('未完成');
  });

  it('uses terminal output once and reports a fully evicted gap inclusively', () => {
    const run: SubagentRunState = {
      subagentRunId: 'execution-1',
      runId: '',
      agent: 'explorer',
      status: 'completed',
      startedAt: 1,
      finalOutput: 'complete answer',
      events: [
        {
          kind: 'subagent',
          workspace_id: 'workspace-1',
          conversation_id: 'conversation-1',
          run_id: '',
          subagent_run_id: 'execution-1',
          agent: 'explorer',
          event: 'token_delta',
          content: 'complete answer',
        },
        {
          kind: 'subagent',
          workspace_id: 'workspace-1',
          conversation_id: 'conversation-1',
          run_id: '',
          subagent_run_id: 'execution-1',
          agent: 'explorer',
          event: 'subagent_stream_gap',
          requested_after: 1,
          available_from: null,
          latest_sequence: 2,
        },
      ],
    };
    const timeline = subagentTimelineEntries(run, [], {});
    expect(timeline.find((entry) => entry.kind === 'gap')).toMatchObject({ from: 1, to: 2 });
    expect(visibleSubagentTimelineEntries(true, 'complete answer', timeline)).toEqual([
      expect.objectContaining({ kind: 'gap', from: 1, to: 2 }),
    ]);
  });

  it('keeps partial streamed output when a failed terminal has no complete output', () => {
    const entries = [
      { kind: 'text' as const, key: 'partial', content: 'partial investigation result' },
    ];

    expect(visibleSubagentTimelineEntries(true, '', entries)).toEqual(entries);
    expect(visibleSubagentTimelineEntries(true, 'different failure summary', entries)).toEqual(
      entries
    );
  });
});

it('renders canonical progress, tool position and native reasoning without a candidate or terminal guess', () => {
  const native = (
    sequence: number,
    phase: 'commentary' | 'reasoning' | 'partial_answer' | 'final_answer',
    delta: string
  ) => ({
    schema_version: 5,
    event_id: `native-${sequence}`,
    content_hash: `sha256:${sequence}`,
    sequence,
    stream_id: 'native-stream',
    turn_id: 'execution-1',
    message_id: 'message-1',
    conversation_id: 'conversation-1',
    run_id: 'run-1',
    execution_id: 'execution-1',
    parent_event_id: null,
    timestamp: '2026-10-28T00:00:00Z',
    payload: {
      type: 'assistant_message' as const,
      data: {
        block: {
          provider_source: null,
          block_id: `block-${sequence}`,
          request_id: 'original-request',
          sequence,
          phase,
          delta,
          complete: true,
          provider: 'fixture',
          reasoning:
            phase === 'reasoning'
              ? { type: 'signed', thinking: '折叠推理原文', signature: 'original-signature' }
              : null,
        },
      },
    },
  });
  const base = {
    kind: 'subagent' as const,
    workspace_id: 'workspace-1',
    conversation_id: 'conversation-1',
    run_id: 'run-1',
    subagent_run_id: 'execution-1',
    agent: 'primary',
  };
  const run: SubagentRunState = {
    subagentRunId: 'execution-1',
    runId: 'run-1',
    agent: 'primary',
    status: 'running',
    startedAt: 1,
    events: [
      { ...base, event: 'note', agent_envelope: native(1, 'commentary', '公开进度一') },
      { ...base, event: 'note', agent_envelope: native(2, 'reasoning', '') },
      { ...base, event: 'tool_started', call_id: 'original-call' },
      { ...base, event: 'note', agent_envelope: native(3, 'partial_answer', '公开进度二') },
      { ...base, event: 'note', agent_envelope: native(4, 'final_answer', '不得展示的候选') },
    ],
  };
  const html = renderToStaticMarkup(<SubagentDetailView run={run} onBack={() => undefined} />);
  expect(html).toContain('公开进度一');
  expect(html).toContain('original-call');
  expect(html).toContain('公开进度二');
  expect(html.indexOf('公开进度一')).toBeLessThan(html.indexOf('original-call'));
  expect(html.indexOf('original-call')).toBeLessThan(html.indexOf('公开进度二'));
  expect(html).toContain('原生消息身份与来源');
  expect(html).toContain('original-signature');
  expect(html).not.toContain('不得展示的候选');
  expect(html).toContain('data-phase="reasoning"');
  expect(run.status).toBe('running');
});

it('joins native block deltas while preserving raw records and tool boundaries', () => {
  const native = (sequence: number, delta: string, complete = false) => ({
    schema_version: 5,
    event_id: `chunk-${sequence}`,
    content_hash: 'hash',
    sequence,
    stream_id: 'stream',
    turn_id: 'execution',
    message_id: null,
    conversation_id: 'conv',
    run_id: 'run',
    execution_id: 'execution',
    parent_event_id: null,
    timestamp: '2026-10-28T00:00:00Z',
    payload: {
      type: 'assistant_message' as const,
      data: {
        block: {
          provider_source: null,
          block_id: 'same-block',
          request_id: 'request',
          sequence,
          phase: 'commentary' as const,
          delta,
          complete,
          provider: null,
          reasoning: null,
        },
      },
    },
  });
  const base = {
    kind: 'subagent' as const,
    workspace_id: 'workspace',
    conversation_id: 'conv',
    run_id: 'run',
    subagent_run_id: 'execution',
    agent: 'primary',
  };
  const run: SubagentRunState = {
    subagentRunId: 'execution',
    runId: 'run',
    agent: 'primary',
    status: 'running',
    startedAt: 1,
    events: [
      { ...base, event: 'note', agent_envelope: native(1, '检') },
      { ...base, event: 'note', agent_envelope: native(2, '查') },
      { ...base, event: 'note', agent_envelope: native(3, '完成', true) },
      { ...base, event: 'tool_started', call_id: 'original-call' },
      { ...base, event: 'note', agent_envelope: native(4, '后续段落', true) },
    ],
  };
  const entries = subagentTimelineEntries(run, [], {});
  expect(entries.map((entry) => entry.kind)).toEqual(['native', 'tool_ref', 'native']);
  const first = entries[0];
  expect(first.kind === 'native' && first.content).toBe('检查完成');
  expect(
    first.kind === 'native' &&
      first.originals?.map(
        (item) => item.payload.type === 'assistant_message' && item.payload.data.block.delta
      )
  ).toEqual(['检', '查', '完成']);
});
it('renders a confirmed final once and shows unreadable native records without crashing', () => {
  const envelope = {
    schema_version: 5,
    event_id: 'final',
    content_hash: 'hash',
    sequence: 1,
    stream_id: 'stream',
    turn_id: 'execution',
    message_id: null,
    conversation_id: 'conv',
    run_id: 'run',
    execution_id: 'execution',
    parent_event_id: null,
    timestamp: '2026-10-28T00:00:00Z',
    payload: { type: 'final_answer' as const, data: '唯一原确认回答' },
  };
  const base = {
    kind: 'subagent' as const,
    workspace_id: 'workspace',
    conversation_id: 'conv',
    run_id: 'run',
    subagent_run_id: 'execution',
    agent: 'primary',
  };
  const run: SubagentRunState = {
    subagentRunId: 'execution',
    runId: 'run',
    agent: 'primary',
    status: 'completed',
    startedAt: 1,
    finalOutput: '唯一原确认回答',
    events: [
      { ...base, event: 'note', agent_envelope: envelope },
      { ...base, event: 'note', native_decode_error: '原生事件不可读，原始记录仍保留' },
    ],
  };
  const html = renderToStaticMarkup(<SubagentDetailView run={run} onBack={() => undefined} />);
  expect(html.match(/唯一原确认回答/g)).toHaveLength(1);
  expect(html).toContain('原生事件不可读');
});

it('shows accumulated signed reasoning after empty deltas without the placeholder', () => {
  const native = (sequence: number, thinking: string) => ({
    schema_version: 5,
    event_id: `signed-${sequence}`,
    content_hash: 'hash',
    sequence,
    stream_id: 'stream',
    turn_id: 'execution',
    message_id: null,
    conversation_id: 'conv',
    run_id: 'run',
    execution_id: 'execution',
    parent_event_id: null,
    timestamp: '2026-10-28T00:00:00Z',
    payload: {
      type: 'assistant_message' as const,
      data: {
        block: {
          provider_source: null,
          block_id: 'signed-block',
          request_id: 'request',
          sequence,
          phase: 'reasoning' as const,
          delta: '',
          complete: sequence === 2,
          provider: 'fixture',
          reasoning: { type: 'signed', thinking, signature: `sig-${sequence}` },
        },
      },
    },
  });
  const base = {
    kind: 'subagent' as const,
    workspace_id: 'workspace',
    conversation_id: 'conv',
    run_id: 'run',
    subagent_run_id: 'execution',
    agent: 'primary',
  };
  const run: SubagentRunState = {
    subagentRunId: 'execution',
    runId: 'run',
    agent: 'primary',
    status: 'running',
    startedAt: 1,
    events: [
      { ...base, event: 'note', agent_envelope: native(1, '第一段') },
      { ...base, event: 'note', agent_envelope: native(2, '第二段') },
    ],
  };
  render(<SubagentDetailView run={run} onBack={() => undefined} />);
  try {
    fireEvent.click(screen.getByRole('button', { name: '展开思考' }));
    expect(screen.getByText('第一段第二段')).toBeTruthy();
    expect(screen.queryByText('已保留原生推理状态')).toBeNull();
  } finally {
    cleanup();
  }
});
