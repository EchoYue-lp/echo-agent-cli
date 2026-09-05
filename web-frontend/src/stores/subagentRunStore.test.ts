import { beforeEach, describe, expect, it } from 'vitest';
import type {
  ExecEvent,
  PlanRevision,
  RuntimeTaskEvent,
  SubagentEventMetadata,
  TaskRun,
} from '../generated';
import {
  ingestSubagentExecEvent,
  ingestTaskRuntimeSubagentEvents,
  latestSubagentRunsByTask,
  subagentRunStoreKey,
  useSubagentRunStore,
  type ExecutionEvent,
} from './subagentRunStore';

describe('subagentRunStore terminal result', () => {
  beforeEach(() => {
    useSubagentRunStore.getState().clear();
  });

  it('uses the generated ExecEvent contract for live and replayed timeline events', () => {
    const metadata = (sequence: number): SubagentEventMetadata => ({
      schema_version: 4,
      event_id: `evt-${sequence}`,
      content_hash: `sha256:${sequence}`,
      sequence,
      stream_id: 'stream-1',
      turn_id: 'message-1',
      message_id: 'message-1',
      execution_id: 'execution-1',
      parent_event_id: null,
      timestamp: `2026-09-05T00:00:0${sequence}Z`,
      parent_agent: 'primary',
      agent_name: 'explorer',
      parent_execution_id: null,
      agent_path: 'primary/explorer',
      task_id: null,
      attempt: 1,
      plan_revision: null,
    });
    const wire = (
      event: ExecEvent['event'],
      sequence: number,
      payload: ExecEvent['payload']
    ): ExecEvent => ({
      workspace_id: 'workspace-1',
      conversation_id: 'conversation-1',
      run_id: '',
      scope: 'subagent',
      task_id: null,
      subagent_run_id: 'execution-1',
      event,
      agent: 'explorer',
      payload,
      framework_event: metadata(sequence),
    });

    ingestSubagentExecEvent(wire('started', 1, { task: 'inspect', background: false }));
    ingestSubagentExecEvent(wire('thinking_delta', 2, { content: 'reasoning' }));
    ingestSubagentExecEvent(
      {
        ...wire('subagent_stream_gap', 3, {
          stream_id: 'stream-1',
          requested_after: 2,
          available_from: 5,
          latest_sequence: 6,
        }),
        framework_event: null,
      },
      {
        event_id: 'journal-gap-30',
        stream_id: 'conversation-stream',
        sequence: 30,
        timestamp: '2026-09-05T00:00:03Z',
      }
    );
    ingestSubagentExecEvent(
      wire('completed', 6, {
        output: 'done',
        duration_ms: 10,
        tokens_used: 4,
        outcome: {
          contract_version: 2,
          status: 'completed',
          summary: 'done',
          artifacts: [],
          evidence: [],
          verification: [],
          remaining_work: [],
          touched_files: { read: [], written: [] },
        },
      })
    );
    ingestSubagentExecEvent(wire('completed', 6, { output: 'duplicate' }));

    const run = useSubagentRunStore.getState().runs[subagentRunStoreKey('', 'execution-1')];
    expect(run?.status).toBe('completed');
    expect(run?.events.map((event) => event.event)).toEqual([
      'started',
      'thinking_delta',
      'subagent_stream_gap',
      'completed',
    ]);
    expect(run?.events[1]?.content).toBe('reasoning');
    expect(run?.events[2]?.available_from).toBe(5);
    expect(run?.events[2]?.stream_id).toBe('stream-1');
    expect(run?.events[2]?.sequence).toBeUndefined();
    expect(run?.finalOutput).toBe('done');
  });

  it('treats TaskRuntime terminal hydration as fallback while framework replay fills history', () => {
    const base = {
      kind: 'subagent' as const,
      workspace_id: 'workspace-1',
      conversation_id: 'conversation-1',
      run_id: 'run-1',
      subagent_run_id: 'execution-1',
      task_id: 'task-1',
      agent: 'explorer',
    };
    useSubagentRunStore.getState().ingest({
      ...base,
      event: 'completed',
      summary: 'durable fallback',
    });
    const metadata = (sequence: number): SubagentEventMetadata => ({
      schema_version: 4,
      event_id: `framework-${sequence}`,
      content_hash: `sha256:${sequence}`,
      sequence,
      stream_id: 'framework-stream',
      turn_id: 'message-1',
      message_id: 'message-1',
      execution_id: 'execution-1',
      parent_event_id: null,
      timestamp: `2026-09-05T00:00:0${sequence}Z`,
      parent_agent: 'primary',
      agent_name: 'explorer',
      parent_execution_id: null,
      agent_path: 'primary/explorer',
      task_id: 'task-1',
      attempt: 1,
      plan_revision: 1,
    });
    const wire = (event: ExecEvent['event'], sequence: number, payload: unknown): ExecEvent => ({
      workspace_id: 'workspace-1',
      conversation_id: 'conversation-1',
      run_id: 'run-1',
      scope: 'subagent',
      task_id: 'task-1',
      subagent_run_id: 'execution-1',
      event,
      agent: 'explorer',
      payload,
      framework_event: metadata(sequence),
    });
    ingestSubagentExecEvent(wire('started', 1, { task: 'inspect' }));
    ingestSubagentExecEvent(wire('thinking_delta', 2, { content: 'reasoning' }));
    ingestSubagentExecEvent(
      wire('completed', 3, {
        output: 'framework result',
        outcome: {
          contract_version: 2,
          status: 'completed',
          summary: 'framework result',
          artifacts: [],
          evidence: [],
          verification: [],
          remaining_work: [],
          touched_files: { read: [], written: [] },
        },
      })
    );
    useSubagentRunStore.getState().ingest({
      ...base,
      event: 'completed',
      summary: 'late synthetic result',
    });

    const run = useSubagentRunStore.getState().runs[subagentRunStoreKey('run-1', 'execution-1')];
    expect(run?.status).toBe('completed');
    expect(run?.finalOutput).toBe('framework result');
    expect(run?.outcome?.summary).toBe('framework result');
    expect(run?.events.map((event) => event.event)).toEqual([
      'completed',
      'started',
      'thinking_delta',
      'completed',
    ]);
  });

  it('preserves the complete timed-out result contract', () => {
    const event: ExecutionEvent = {
      kind: 'subagent',
      workspace_id: 'workspace-1',
      conversation_id: 'conversation-1',
      subagent_run_id: 'task-1:1',
      run_id: 'run-1',
      task_id: 'task-1',
      agent: 'implementer',
      event: 'timed_out',
      output: 'partial report body',
      terminal_status: 'timed_out',
      contract_version: 1,
      summary: 'subagent timed out after writing a partial report',
      artifacts: [
        {
          path: '/tmp/report.json',
          kind: 'report',
          bytes: 42,
          sha256: 'a'.repeat(64),
          producer_execution_id: 'task-1:1',
          available: true,
        },
      ],
      verification: [
        {
          check: 'cargo test --workspace',
          status: 'not_run',
          details: 'deadline reached',
          source: 'observed',
        },
      ],
      remaining_work: ['run the workspace tests'],
      touched_files: {
        read: ['src/lib.rs'],
        written: ['reports/report.json'],
      },
    };

    useSubagentRunStore.getState().ingest(event);

    const run = useSubagentRunStore.getState().runs[subagentRunStoreKey('run-1', 'task-1:1')];
    expect(run?.status).toBe('timed_out');
    expect(run?.finalOutput).toBe('partial report body');
    expect(run?.outcome?.summary).toBe(event.summary);
    expect(run?.outcome?.artifacts[0]?.path).toBe('/tmp/report.json');
    expect(run?.outcome?.artifacts[0]?.bytes).toBe(42n);
    expect(run?.outcome?.verification).toEqual(event.verification);
    expect(run?.outcome?.remaining_work).toEqual(event.remaining_work);
    expect(run?.outcome?.touched_files).toEqual(event.touched_files);
  });

  it('keeps a terminal Subagent terminal when a duplicate started event arrives', () => {
    const base = {
      kind: 'subagent' as const,
      workspace_id: 'workspace-1',
      conversation_id: 'conversation-1',
      subagent_run_id: 'task-merge:1',
      run_id: 'run-merge',
      task_id: 'task-merge',
      agent: 'implementer',
    };
    useSubagentRunStore.getState().ingest({
      ...base,
      event: 'completed',
      terminal_status: 'completed',
      contract_version: 1,
      summary: 'implementation finished',
      artifacts: [],
      verification: [],
      remaining_work: [],
      touched_files: { read: [], written: ['src/main.rs'] },
    });
    useSubagentRunStore.getState().ingest({ ...base, event: 'started' });

    const completed =
      useSubagentRunStore.getState().runs[subagentRunStoreKey('run-merge', 'task-merge:1')];
    expect(completed?.status).toBe('completed');
    expect(completed?.outcome?.summary).toBe('implementation finished');
  });

  it('keeps retry attempts in separate SubagentRun records', () => {
    const base = {
      kind: 'subagent' as const,
      workspace_id: 'workspace-1',
      conversation_id: 'conversation-1',
      run_id: 'run-retry',
      task_id: 'task-retry',
      agent: 'explorer',
      event: 'started' as const,
    };

    useSubagentRunStore.getState().ingest({ ...base, subagent_run_id: 'task-retry:1' });
    useSubagentRunStore.getState().ingest({ ...base, subagent_run_id: 'task-retry:2' });

    expect(Object.keys(useSubagentRunStore.getState().runs).sort()).toEqual([
      subagentRunStoreKey('run-retry', 'task-retry:1'),
      subagentRunStoreKey('run-retry', 'task-retry:2'),
    ]);
  });

  it('orders physical claim ids by their exact projected attempt', () => {
    const base = {
      kind: 'subagent' as const,
      workspace_id: 'workspace-1',
      conversation_id: 'conversation-1',
      run_id: 'run-retry',
      task_id: 'task-retry',
      agent: 'explorer',
      event: 'started' as const,
    };
    useSubagentRunStore.getState().ingest({
      ...base,
      subagent_run_id: 'run-retry:task-retry:4:1:claim-newer-timestamp',
      plan_revision: 4,
      attempt: 1,
      started_at: 20,
    });
    useSubagentRunStore.getState().ingest({
      ...base,
      subagent_run_id: 'run-retry:task-retry:4:2:claim-older-timestamp',
      plan_revision: 4,
      attempt: 2,
      started_at: 10,
    });
    const latest = latestSubagentRunsByTask(Object.values(useSubagentRunStore.getState().runs));
    expect(latest).toHaveLength(1);
    expect(latest[0]?.attempt).toBe(2);
  });

  it('isolates the same legacy execution id across separate TaskRuns', () => {
    const base = {
      kind: 'subagent' as const,
      workspace_id: 'workspace-1',
      conversation_id: 'conversation-1',
      subagent_run_id: 'task-shared:1:1',
      task_id: 'task-shared',
      agent: 'explorer',
    };
    useSubagentRunStore.getState().ingest({
      ...base,
      run_id: 'run-old',
      event: 'completed',
      summary: 'old result',
    });
    useSubagentRunStore.getState().ingest({
      ...base,
      run_id: 'run-new',
      event: 'started',
    });

    expect(
      useSubagentRunStore.getState().runs[subagentRunStoreKey('run-old', 'task-shared:1:1')]?.status
    ).toBe('completed');
    expect(
      useSubagentRunStore.getState().runs[subagentRunStoreKey('run-new', 'task-shared:1:1')]?.status
    ).toBe('running');
  });

  it('restores the existing Subagent card state from durable TaskRuntime events', () => {
    const taskRun = {
      run_id: 'run-restored',
      workspace_id: 'workspace-1',
      conversation_id: 'conversation-restored',
      root_message_id: 'message-restored',
    } as TaskRun;
    const plan = {
      tasks: [
        {
          id: 'task-analysis',
          title: 'CLI 层架构分析',
          description: '分析 CLI 目录与入口',
          agent_role: 'explorer',
        },
      ],
    } as PlanRevision;
    const events = [
      {
        seq: '12',
        run_id: 'run-restored',
        task_id: 'task-analysis',
        step_id: 'run-restored:task-analysis:4:1',
        event_type: 'subagent_assigned',
        payload: {
          execution_id: 'run-restored:task-analysis:4:1',
          agent_name: 'explorer',
          attempt: 1,
        },
        timestamp: '2026-07-30T01:02:03Z',
      },
      {
        seq: '13',
        run_id: 'run-restored',
        task_id: 'task-analysis',
        step_id: 'run-restored:task-analysis:4:1',
        event_type: 'subagent_released',
        payload: {
          execution_id: 'run-restored:task-analysis:4:1',
          status: 'completed',
          full_output: 'CLI analysis complete',
          outcome: {
            contract_version: 1,
            status: 'completed',
            summary: 'CLI analysis complete',
            artifacts: [],
            verification: [],
            remaining_work: [],
            touched_files: { read: ['src/cli'], written: [] },
          },
        },
        timestamp: '2026-07-30T01:03:03Z',
      },
    ] as unknown as RuntimeTaskEvent[];

    ingestTaskRuntimeSubagentEvents(taskRun, plan, events);

    const restored =
      useSubagentRunStore.getState().runs[
        subagentRunStoreKey('run-restored', 'run-restored:task-analysis:4:1')
      ];
    expect(restored).toMatchObject({
      runId: 'run-restored',
      workspaceId: 'workspace-1',
      taskId: 'task-analysis',
      agent: 'explorer',
      task: '分析 CLI 目录与入口',
      conversationId: 'conversation-restored',
      messageId: 'message-restored',
      status: 'completed',
      startedAt: Date.parse('2026-07-30T01:02:03Z'),
      finalOutput: 'CLI analysis complete',
    });
    expect(restored?.outcome?.summary).toBe('CLI analysis complete');
    expect(restored?.outcome?.touched_files.read).toEqual(['src/cli']);
  });
});
