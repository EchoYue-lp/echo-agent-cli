import { beforeEach, describe, expect, it } from 'vitest';
import type { RuntimeTaskEvent, TaskRun } from '../generated';
import type { ToolExecution } from '../types/api';
import {
  ingestTaskRuntimeToolExecutions,
  mergeHydratedToolExecutions,
  mergeTaskRuntimeToolExecutions,
  taskRuntimeToolExecutions,
  useToolExecutionStore,
} from './toolExecutionStore';

const run = {
  run_id: 'run-1',
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
      id: 'detail-stale',
      status: 'succeeded',
      finished_at: 200,
      duration_ms: 100,
      detail_ref: 'detail-stale',
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

    useToolExecutionStore.getState().hydrateConversation('conversation-1', [restored]);

    expect(Object.values(useToolExecutionStore.getState().tools)).toEqual(
      expect.arrayContaining([background, restored])
    );
  });
});
