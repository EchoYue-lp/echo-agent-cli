import { beforeEach, describe, expect, it } from 'vitest';
import { toolExecutionIdsForOwner, useToolExecutionStore } from '../../stores/toolExecutionStore';
import type { ToolExecution } from '../../types/api';
import { toolSummaryText } from './InlineToolCall';

function summary(overrides: Partial<ToolExecution> = {}): ToolExecution {
  return {
    id: 'detail-1',
    workspace_id: 'workspace-1',
    call_id: 'call-1',
    owner: { kind: 'chat', message_id: 'message-1' },
    conversation_id: 'conversation-1',
    run_id: 'run-1',
    name: 'shell',
    args_preview: '{"command":"printf hello"}',
    status: 'succeeded',
    started_at: 1_000,
    finished_at: 2_000,
    duration_ms: 1_000,
    detail_ref: 'detail-1',
    ...overrides,
  };
}

describe('InlineToolCall', () => {
  beforeEach(() => {
    useToolExecutionStore.getState().clear();
  });

  it('keeps only summaries in the normalized store', () => {
    useToolExecutionStore.getState().ingest(summary());
    const tool = useToolExecutionStore.getState().tools['detail-1'];

    expect(tool).toEqual(summary());
    expect(toolSummaryText(tool?.name ?? '', tool?.args_preview ?? '')).toBe(
      'shell · {"command":"printf hello"}'
    );
  });

  it('uses the same row for a running subagent-owned tool', () => {
    useToolExecutionStore.getState().ingest(
      summary({
        id: 'detail-subagent',
        detail_ref: 'detail-subagent',
        owner: { kind: 'subagent', subagent_run_id: 'task-1:1' },
        name: 'read_file',
        args_preview: '{"path":"README.md"}',
        status: 'running',
        finished_at: null,
        duration_ms: null,
      })
    );
    const ownerIds = useToolExecutionStore.getState().idsByOwner['subagent:run-1:task-1:1'];

    expect(ownerIds).toEqual(['detail-subagent']);
    expect(useToolExecutionStore.getState().tools['detail-subagent']?.status).toBe('running');
  });

  it('returns a stable empty snapshot for an owner without tools', () => {
    const selectMissingOwner = () =>
      toolExecutionIdsForOwner(
        useToolExecutionStore.getState().idsByOwner,
        'subagent:without-tools'
      );

    const firstSnapshot = selectMissingOwner();
    const secondSnapshot = selectMissingOwner();

    expect(firstSnapshot).toBe(secondSnapshot);
    expect(firstSnapshot).toEqual([]);
  });
});
