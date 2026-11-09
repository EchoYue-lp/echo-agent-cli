import { beforeEach, describe, expect, it } from 'vitest';
import type { RuntimeTaskEvent, TaskRun } from '../generated';
import {
  transcriptToolExecution,
  type ToolExecution,
  type TranscriptToolRecord,
} from '../types/api';
import {
  ingestTaskRuntimeToolExecutions,
  mergeHydratedToolExecutions,
  mergeTaskRuntimeToolExecutions,
  taskRuntimeToolExecutions,
  useToolExecutionStore,
} from './toolExecutionStore';

const run = {
  run_id: 'run-1',
  workspace_id: 'workspace-1',
  conversation_id: 'conversation-1',
} as TaskRun;

function runtimeEvents(): RuntimeTaskEvent[] {
  return [
    {
      seq: '1',
      run_id: 'run-1',
      task_id: 'task-1',
      step_id: 'call-1',
      event_type: 'tool_started',
      payload: {
        execution_id: 'run-1:task-1:1:1',
        call_id: 'call-1',
        tool_name: 'read_file',
      },
      timestamp: '2026-07-30T01:02:03.000Z',
    },
    {
      seq: '2',
      run_id: 'run-1',
      task_id: 'task-1',
      step_id: 'call-1',
      event_type: 'tool_completed',
      payload: {
        execution_id: 'run-1:task-1:1:1',
        call_id: 'call-1',
        tool_name: 'read_file',
      },
      timestamp: '2026-07-30T01:02:04.500Z',
    },
  ] as unknown as RuntimeTaskEvent[];
}

describe('TaskRuntime tool execution recovery', () => {
  beforeEach(() => {
    useToolExecutionStore.getState().clear();
  });

  it('reconstructs one completed Subagent tool row from durable boundaries', () => {
    expect(taskRuntimeToolExecutions(run, runtimeEvents())).toEqual([
      expect.objectContaining({
        call_id: 'call-1',
        owner: { kind: 'subagent', subagent_run_id: 'run-1:task-1:1:1' },
        name: 'read_file',
        status: 'succeeded',
        duration_ms: 1500,
        detail_ref: '',
      }),
    ]);
  });

  it('keeps the full repository summary instead of a runtime fallback', () => {
    const fallback = taskRuntimeToolExecutions(run, runtimeEvents());
    const persisted: ToolExecution = {
      id: 'detail-1',
      workspace_id: 'workspace-1',
      call_id: 'call-1',
      owner: { kind: 'subagent', subagent_run_id: 'run-1:task-1:1:1' },
      conversation_id: 'conversation-1',
      run_id: 'run-1',
      name: 'read_file',
      args_preview: '{"path":"src/main.rs"}',
      status: 'succeeded',
      started_at: 1,
      finished_at: 2,
      duration_ms: 1,
      detail_ref: 'detail-1',
    };

    expect(mergeTaskRuntimeToolExecutions([persisted], fallback)).toEqual([persisted]);
  });

  it('never overwrites a canonical detail terminal with a flattened runtime boundary', () => {
    const fallback = taskRuntimeToolExecutions(run, runtimeEvents());
    const recovered: ToolExecution = {
      id: 'detail-recovered',
      workspace_id: 'workspace-1',
      call_id: 'call-1',
      owner: { kind: 'subagent', subagent_run_id: 'run-1:task-1:1:1' },
      conversation_id: 'conversation-1',
      run_id: 'run-1',
      name: 'read_file',
      args_preview: '{"path":"src/main.rs"}',
      status: 'cancelled',
      started_at: 1,
      finished_at: 2,
      duration_ms: 1,
      detail_ref: 'detail-recovered',
    };

    expect(mergeTaskRuntimeToolExecutions([recovered], fallback)).toEqual([recovered]);
  });

  it('preserves the start timestamp when a terminal event arrives incrementally', () => {
    const events = runtimeEvents();
    ingestTaskRuntimeToolExecutions(run, events.slice(0, 1));
    ingestTaskRuntimeToolExecutions(run, events.slice(1));

    const [tool] = Object.values(useToolExecutionStore.getState().tools);
    expect(tool).toMatchObject({ status: 'succeeded', duration_ms: 1500 });
  });

  it('keeps a detailed canonical terminal when a flattened runtime event arrives', () => {
    const recovered: ToolExecution = {
      id: 'detail-recovered',
      workspace_id: 'workspace-1',
      call_id: 'call-1',
      owner: { kind: 'subagent', subagent_run_id: 'run-1:task-1:1:1' },
      conversation_id: 'conversation-1',
      run_id: 'run-1',
      name: 'read_file',
      args_preview: '{"path":"src/main.rs"}',
      status: 'cancelled',
      started_at: 1,
      finished_at: 2,
      duration_ms: 1,
      detail_ref: 'detail-recovered',
    };
    useToolExecutionStore.getState().ingest(recovered);

    ingestTaskRuntimeToolExecutions(run, runtimeEvents());

    expect(useToolExecutionStore.getState().tools['detail-recovered']).toMatchObject({
      status: 'cancelled',
      detail_ref: 'detail-recovered',
    });
  });

  it('keeps a live terminal detail when a stale running snapshot arrives later', () => {
    const terminal: ToolExecution = {
      id: 'detail-live',
      workspace_id: 'workspace-1',
      call_id: 'call-1',
      owner: { kind: 'subagent', subagent_run_id: 'run-1:task-1:1:1' },
      conversation_id: 'conversation-1',
      run_id: 'run-1',
      name: 'read_file',
      args_preview: '{"path":"src/main.rs"}',
      status: 'succeeded',
      started_at: 100,
      finished_at: 200,
      duration_ms: 100,
      detail_ref: 'detail-live',
    };
    const stale = { ...terminal, status: 'running' as const, finished_at: null, duration_ms: null };

    expect(mergeHydratedToolExecutions([terminal], [stale])).toEqual([terminal]);
  });

  it('keeps the newer live terminal state when an older terminal snapshot arrives later', () => {
    const live: ToolExecution = {
      id: 'detail-live',
      workspace_id: 'workspace-1',
      call_id: 'call-1',
      owner: { kind: 'subagent', subagent_run_id: 'run-1:task-1:1:1' },
      conversation_id: 'conversation-1',
      run_id: 'run-1',
      name: 'read_file',
      args_preview: '{"path":"src/main.rs"}',
      status: 'failed',
      started_at: 100,
      finished_at: 300,
      duration_ms: 200,
      detail_ref: 'detail-live',
    };
    const stale: ToolExecution = {
      ...live,
      status: 'succeeded',
      finished_at: 200,
      duration_ms: 100,
    };

    expect(mergeHydratedToolExecutions([live], [stale])).toEqual([live]);
  });

  it('does not merge identical owner and call ids from separate TaskRuns', () => {
    const first = taskRuntimeToolExecutions(run, runtimeEvents())[0];
    const secondRun = { ...run, run_id: 'run-2' } as TaskRun;
    const secondEvents = runtimeEvents().map((event) => ({ ...event, run_id: 'run-2' }));
    const second = taskRuntimeToolExecutions(secondRun, secondEvents)[0];

    expect(first).toBeDefined();
    expect(second).toBeDefined();
    expect(mergeHydratedToolExecutions(first ? [first] : [], second ? [second] : [])).toHaveLength(
      2
    );
  });

  it('hydrates one conversation without deleting live tools from another conversation', () => {
    const background: ToolExecution = {
      id: 'background-tool',
      workspace_id: 'workspace-background',
      call_id: 'call-background',
      owner: { kind: 'chat', message_id: 'message-background' },
      conversation_id: 'conversation-background',
      run_id: null,
      name: 'shell',
      args_preview: 'npm test',
      status: 'running',
      started_at: 100,
      finished_at: null,
      duration_ms: null,
      detail_ref: 'background-tool',
    };
    const restored: ToolExecution = {
      id: 'restored-tool',
      workspace_id: 'workspace-1',
      call_id: 'call-restored',
      owner: { kind: 'chat', message_id: 'message-restored' },
      conversation_id: 'conversation-1',
      run_id: null,
      name: 'read_file',
      args_preview: 'src/main.rs',
      status: 'succeeded',
      started_at: 200,
      finished_at: 300,
      duration_ms: 100,
      detail_ref: 'restored-tool',
    };
    useToolExecutionStore.getState().ingest(background);

    useToolExecutionStore
      .getState()
      .hydrateConversation('workspace-1', 'conversation-1', [restored]);

    expect(Object.values(useToolExecutionStore.getState().tools)).toEqual(
      expect.arrayContaining([background, restored])
    );
  });
});

describe('tool recovery settlement identity', () => {
  beforeEach(() => useToolExecutionStore.getState().clear());

  function summary(overrides: Partial<ToolExecution> = {}): ToolExecution {
    return {
      id: 'detail-live',
      workspace_id: 'workspace-1',
      conversation_id: 'conversation-1',
      run_id: null,
      owner: { kind: 'chat', message_id: 'message-1' },
      call_id: 'call-1',
      name: 'read_file',
      args_preview: 'src/main.rs',
      status: 'failed',
      started_at: 100,
      finished_at: 200,
      duration_ms: 100,
      detail_ref: 'detail-live',
      ...overrides,
    };
  }

  it('does not reopen a failed live tool after a late start event', () => {
    const terminal = summary();
    const store = useToolExecutionStore.getState();
    store.ingest(terminal);
    store.ingest({ ...terminal, status: 'running', finished_at: null, duration_ms: null });
    expect(Object.values(useToolExecutionStore.getState().tools)).toEqual([terminal]);
  });

  it('retains the newer terminal when an older terminal notification arrives late', () => {
    const terminal = summary({ finished_at: 300, duration_ms: 200 });
    const store = useToolExecutionStore.getState();
    store.ingest(terminal);
    store.ingest(summary({ status: 'succeeded' }));
    expect(Object.values(useToolExecutionStore.getState().tools)).toEqual([terminal]);
  });

  it.each(['running', 'failed'] as const)(
    'does not replace a known %s boundary with an unknown record without a finish',
    (status) => {
      const known = summary({
        status,
        finished_at: status === 'running' ? null : 200,
        duration_ms: status === 'running' ? null : 100,
      });
      const unknown = summary({ status: 'unknown', finished_at: null, duration_ms: null });
      expect(mergeHydratedToolExecutions([known], [unknown])).toEqual([known]);
      useToolExecutionStore.getState().ingest(known);
      useToolExecutionStore.getState().ingest(unknown);
      expect(Object.values(useToolExecutionStore.getState().tools)).toEqual([known]);
    }
  );

  it('settles a running orphan to the real backend Unknown terminal and rejects a late start', () => {
    const running = summary({ status: 'running', finished_at: null, duration_ms: null });
    const settled = summary({ status: 'unknown', finished_at: 300, duration_ms: 200 });
    useToolExecutionStore.getState().ingest(running);
    useToolExecutionStore.getState().ingest(settled);
    expect(Object.values(useToolExecutionStore.getState().tools)).toEqual([settled]);
    useToolExecutionStore.getState().ingest(running);
    expect(Object.values(useToolExecutionStore.getState().tools)).toEqual([settled]);
    expect(mergeHydratedToolExecutions([running], [settled])).toEqual([settled]);
    const actualResult = summary({ status: 'succeeded', finished_at: 400, duration_ms: 300 });
    useToolExecutionStore.getState().ingest(actualResult);
    expect(Object.values(useToolExecutionStore.getState().tools)).toEqual([actualResult]);
  });

  it('does not replace an actual failure with a flattened orphan Unknown terminal', () => {
    const failed = summary();
    const orphan = summary({ status: 'unknown', finished_at: 400, duration_ms: 300 });
    expect(mergeHydratedToolExecutions([failed], [orphan])).toEqual([failed]);
  });

  it('keeps duplicate canonical detail identities ambiguous after cold hydration', () => {
    const first = summary();
    const second = summary({
      id: 'other-detail',
      detail_ref: 'other-detail',
      status: 'succeeded',
      finished_at: 300,
      duration_ms: 200,
    });
    const transcript = {
      id: 'record-1',
      workspaceId: 'workspace-1',
      conversationId: 'conversation-1',
      messageId: 'message-1',
      callId: 'call-1',
      name: 'read_file',
    } as TranscriptToolRecord;
    useToolExecutionStore.getState().ingest(first);
    useToolExecutionStore.getState().ingest(second);
    expect(
      transcriptToolExecution(transcript, useToolExecutionStore.getState().tools)
    ).toBeUndefined();
    useToolExecutionStore.getState().clear();
    useToolExecutionStore
      .getState()
      .hydrateConversation('workspace-1', 'conversation-1', [first, second]);
    expect(Object.values(useToolExecutionStore.getState().tools)).toEqual([first, second]);
    expect(
      transcriptToolExecution(transcript, useToolExecutionStore.getState().tools)
    ).toBeUndefined();
    expect(mergeHydratedToolExecutions([first], [second])).toEqual([first, second]);
  });

  it('does not let a runtime fallback select one of several persisted Subagent details', () => {
    const [boundary] = taskRuntimeToolExecutions(run, runtimeEvents());
    if (!boundary) throw new Error('runtime boundary fixture is missing');
    const first = {
      ...boundary,
      id: 'detail-1',
      detail_ref: 'detail-1',
      status: 'failed' as const,
    };
    const second = {
      ...first,
      id: 'detail-2',
      detail_ref: 'detail-2',
      status: 'succeeded' as const,
    };
    expect(mergeTaskRuntimeToolExecutions([first, second], [boundary])).toEqual([first, second]);
    useToolExecutionStore.getState().replaceAll([first, second]);
    ingestTaskRuntimeToolExecutions(run, runtimeEvents());
    expect(Object.values(useToolExecutionStore.getState().tools)).toEqual([first, second]);
  });

  it('preserves interleaved persisted tool order and appends only runtime-only records', () => {
    const [boundary] = taskRuntimeToolExecutions(run, runtimeEvents());
    if (!boundary) throw new Error('runtime boundary fixture is missing');
    const first = { ...boundary, id: 'detail-a1', detail_ref: 'detail-a1' };
    const middle = { ...boundary, id: 'detail-b', detail_ref: 'detail-b', call_id: 'call-b' };
    const last = { ...first, id: 'detail-a2', detail_ref: 'detail-a2' };
    const runtimeOnly = { ...boundary, id: 'runtime-c', call_id: 'call-c' };
    expect(mergeTaskRuntimeToolExecutions([first, middle, last], [])).toEqual([
      first,
      middle,
      last,
    ]);
    expect(mergeTaskRuntimeToolExecutions([first, middle, last], [boundary, runtimeOnly])).toEqual([
      first,
      middle,
      last,
      runtimeOnly,
    ]);
  });

  it('keeps an orphan unknown record unknown without inventing a running or terminal state', () => {
    const unknown = summary({ status: 'unknown', finished_at: null, duration_ms: null });
    useToolExecutionStore.getState().ingest(unknown);
    expect(Object.values(useToolExecutionStore.getState().tools)).toEqual([unknown]);
  });

  it('deduplicates repeated notifications only for the same original detail id', () => {
    const terminal = summary();
    const store = useToolExecutionStore.getState();
    store.ingest(terminal);
    store.ingest({
      ...terminal,
      status: 'running',
      finished_at: null,
      duration_ms: null,
    });
    expect(Object.values(useToolExecutionStore.getState().tools)).toEqual([terminal]);
    expect(useToolExecutionStore.getState().idsByOwner['chat:message-1']).toEqual(['detail-live']);
  });

  it('preserves different detail ids with duplicate call ids for the original ambiguity check', () => {
    const first = summary();
    const second = summary({ id: 'other-detail', detail_ref: 'other-detail' });
    useToolExecutionStore.getState().ingest(first);
    useToolExecutionStore.getState().ingest(second);
    expect(Object.values(useToolExecutionStore.getState().tools)).toEqual([first, second]);
  });

  it('removes the old owner index when the same detail id gains a different original owner', () => {
    const first = summary();
    const rebound = summary({ owner: { kind: 'chat', message_id: 'message-2' } });
    useToolExecutionStore.getState().ingest(first);
    const sameOwnerIndex = useToolExecutionStore.getState().idsByOwner;
    useToolExecutionStore.getState().ingest({ ...first, status: 'succeeded' });
    expect(useToolExecutionStore.getState().idsByOwner).toBe(sameOwnerIndex);
    useToolExecutionStore.getState().ingest(rebound);
    expect(useToolExecutionStore.getState().idsByOwner['chat:message-1']).toEqual([]);
    expect(useToolExecutionStore.getState().idsByOwner['chat:message-2']).toEqual(['detail-live']);
    expect(useToolExecutionStore.getState().tools['detail-live']).toEqual(rebound);
  });

  it.each([{ conversation_id: 'conversation-2' }, { run_id: 'other-run' }])(
    'does not merge reused chat message/call ids across scope %j',
    (scope) => {
      const first = summary();
      const second = summary({ ...scope, id: 'other-detail', detail_ref: 'other-detail' });
      expect(mergeHydratedToolExecutions([first], [second])).toEqual([first, second]);
    }
  );

  it('does not reopen a completed Subagent tool during out-of-order durable replay', () => {
    const [start, finish] = runtimeEvents();
    expect(start).toBeDefined();
    expect(finish).toBeDefined();
    if (!start || !finish) throw new Error('runtime event fixture is incomplete');
    expect(taskRuntimeToolExecutions(run, [finish, start])).toEqual([
      expect.objectContaining({ status: 'succeeded', duration_ms: 1500 }),
    ]);
    ingestTaskRuntimeToolExecutions(run, [finish, start]);
    expect(Object.values(useToolExecutionStore.getState().tools)).toEqual([
      expect.objectContaining({ status: 'succeeded', duration_ms: 1500 }),
    ]);
    useToolExecutionStore.getState().clear();
    ingestTaskRuntimeToolExecutions(run, [finish]);
    ingestTaskRuntimeToolExecutions(run, [start]);
    expect(Object.values(useToolExecutionStore.getState().tools)).toEqual([
      expect.objectContaining({ status: 'succeeded', duration_ms: 1500 }),
    ]);
  });

  it('hydrates only the requested conversation and preserves its foreign live owner', () => {
    const current = summary();
    const foreign = summary({
      id: 'foreign-detail',
      conversation_id: 'conversation-2',
      detail_ref: 'foreign-detail',
    });
    const staleForeign = { ...foreign, status: 'succeeded' as const };
    const otherWorkspace = summary({ id: 'other-workspace', workspace_id: 'workspace-2' });
    useToolExecutionStore.getState().ingest(foreign);
    useToolExecutionStore
      .getState()
      .hydrateConversation('workspace-1', 'conversation-1', [
        current,
        staleForeign,
        otherWorkspace,
      ]);
    expect(Object.values(useToolExecutionStore.getState().tools)).toEqual([foreign, current]);
  });
});
