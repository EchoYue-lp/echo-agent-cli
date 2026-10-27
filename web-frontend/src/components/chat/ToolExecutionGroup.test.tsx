import { describe, expect, it } from 'vitest';
import type { ToolExecution, TranscriptToolRecord } from '../../types/api';
import { toolExecutionGroupPresentation } from './ToolExecutionGroup';

describe('ToolExecutionGroup', () => {
  it('hides individual tool calls by default', () => {
    const tools: ToolExecution[] = ['call-a', 'call-b'].map((id, index) => ({
      id,
      workspace_id: 'workspace-1',
      call_id: id,
      owner: { kind: 'chat', message_id: 'assistant-1' },
      name: `tool_${index + 1}`,
      args_preview: '',
      status: 'succeeded',
      started_at: 1,
      finished_at: 2,
      duration_ms: 1,
      detail_ref: id,
    }));
    const presentation = toolExecutionGroupPresentation(
      ['call-a', 'call-b'],
      Object.fromEntries(tools.map((tool) => [tool.id, tool]))
    );

    expect(presentation).toMatchObject({
      label: '已执行 2 个工具',
      runningCount: 0,
      failedCount: 0,
      missingCount: 0,
    });
  });

  it('does not report success when referenced tool state is missing', () => {
    expect(toolExecutionGroupPresentation(['call-a', 'call-b'], {})).toMatchObject({
      label: '2 个工具 · 2 个状态未恢复',
      missingCount: 2,
    });
  });

  it('does not present interrupted and timed-out tools as successful', () => {
    const interrupted = {
      id: 'call-a',
      workspace_id: 'workspace-1',
      call_id: 'call-a',
      owner: { kind: 'chat', message_id: 'assistant-1' },
      name: 'shell',
      args_preview: '',
      status: 'interrupted',
      started_at: 1,
      finished_at: 2,
      duration_ms: 1,
      detail_ref: 'call-a',
    } satisfies ToolExecution;
    const timedOut = {
      ...interrupted,
      id: 'call-b',
      call_id: 'call-b',
      status: 'timed_out',
      detail_ref: 'call-b',
    } satisfies ToolExecution;

    expect(
      toolExecutionGroupPresentation(['call-a', 'call-b'], {
        'call-a': interrupted,
        'call-b': timedOut,
      })
    ).toMatchObject({ failedCount: 1, uncertainCount: 1, missingCount: 0 });
  });
});

describe('ToolExecutionGroup transcript identity', () => {
  const transcript: TranscriptToolRecord = {
    id: 'record-1',
    workspaceId: 'workspace-1',
    conversationId: 'conversation-1',
    messageId: 'message-1',
    callId: 'call-1',
    name: 'shell',
  };
  const real: ToolExecution = {
    id: 'detail-1',
    workspace_id: 'workspace-1',
    conversation_id: 'conversation-1',
    owner: { kind: 'chat', message_id: 'message-1' },
    call_id: 'call-1',
    name: 'shell',
    args_preview: '',
    status: 'running',
    started_at: 1,
    finished_at: null,
    duration_ms: null,
    detail_ref: 'detail-1',
  };
  it('uses only the uniquely addressed live summary for the same original message', () => {
    expect(
      toolExecutionGroupPresentation(['record-1'], { 'detail-1': real }, [transcript])
    ).toMatchObject({ runningCount: 1, missingCount: 0 });
  });
  it('keeps duplicate identities and foreign owners unknown', () => {
    expect(
      toolExecutionGroupPresentation(
        ['record-1'],
        { 'detail-1': real, 'detail-2': { ...real, id: 'detail-2' } },
        [transcript]
      )
    ).toMatchObject({ runningCount: 0, missingCount: 1 });
    expect(
      toolExecutionGroupPresentation(
        ['record-1'],
        { 'record-1': { ...real, owner: { kind: 'chat', message_id: 'other-message' } } },
        [transcript]
      )
    ).toMatchObject({ runningCount: 0, missingCount: 1 });
  });
});
