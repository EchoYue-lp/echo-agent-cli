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

describe('canonical native Subagent observation projection', () => {
  const envelope = (
    sequence: number,
    phase: 'commentary' | 'reasoning' | 'partial_answer' | 'final_answer' = 'commentary'
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
      type: 'assistant_message',
      data: {
        block: {
          provider_source: {
            protocol: 'responses',
            request_id: 'request-1',
            item_id: 'item-1',
            output_index: 0,
            content_kind: 'output_text',
          },
          block_id: `block-${sequence}`,
          request_id: 'request-1',
          sequence,
          phase,
          delta: `content-${sequence}`,
          complete: true,
          provider: 'fixture',
          reasoning:
            phase === 'reasoning'
              ? { type: 'signed', thinking: '原生推理', signature: 'original-signature' }
              : null,
        },
      },
    },
  });
  const wire = (native: ReturnType<typeof envelope>): ExecEvent => ({
    workspace_id: 'workspace-1',
    conversation_id: 'conversation-1',
    run_id: 'run-1',
    scope: 'subagent',
    task_id: 'task-1',
    subagent_run_id: 'execution-1',
    agent: 'primary',
    event: 'note',
    payload: {
      kind: 'native_agent_event',
      execution_id: 'execution-1',
      agent: 'primary',
      agent_envelope: native,
    },
    framework_event: null,
  });
  beforeEach(() => useSubagentRunStore.getState().clear());
  it('keeps phase, signature, provenance and duplicate identity without creating a terminal', () => {
    const original = [
      envelope(1),
      envelope(2, 'reasoning'),
      envelope(3, 'partial_answer'),
      envelope(4, 'final_answer'),
    ];
    original.forEach((event) => ingestSubagentExecEvent(wire(event)));
    ingestSubagentExecEvent(wire(original[0]));
    const run = useSubagentRunStore.getState().runs[subagentRunStoreKey('run-1', 'execution-1')];
    expect(run?.events.map((event) => event.agent_envelope)).toEqual(original);
    expect(run?.status).toBe('running');
    expect(run?.finalOutput).toBeUndefined();
  });
  it('rejects a canonical envelope for a different execution owner', () => {
    ingestSubagentExecEvent(wire({ ...envelope(1), execution_id: 'foreign-execution' }));
    expect(Object.keys(useSubagentRunStore.getState().runs)).toHaveLength(0);
  });
  it('fills native history after terminal hydration without reopening the task', () => {
    useSubagentRunStore.getState().ingest({
      kind: 'subagent',
      workspace_id: 'workspace-1',
      conversation_id: 'conversation-1',
      run_id: 'run-1',
      subagent_run_id: 'execution-1',
      agent: 'primary',
      event: 'failed',
      error: 'original failure',
    });
    const taskRun: TaskRun = {
      run_id: 'run-1',
      workspace_id: 'workspace-1',
      conversation_id: 'conversation-1',
      root_message_id: 'message-1',
      domain_profile: 'general',
      domain_binding: null,
      status: 'running',
      goal: 'original',
      goal_revision: 1,
      goal_sha256: 'sha',
      plan_id: null,
      route: '',
      attended_mode: 'attended',
      created_at: '2026-10-28T00:00:00Z',
      updated_at: '2026-10-28T00:00:00Z',
    };
    const native = envelope(1, 'reasoning');
    const events: RuntimeTaskEvent[] = [
      {
        seq: '1',
        run_id: 'run-1',
        task_id: 'task-1',
        step_id: 'execution-1',
        event_type: 'note',
        payload: {
          kind: 'native_agent_event',
          execution_id: 'execution-1',
          agent: 'primary',
          agent_envelope: native,
        },
        timestamp: '2026-10-28T00:00:00Z',
      },
    ];
    ingestTaskRuntimeSubagentEvents(taskRun, null, events);
    ingestSubagentExecEvent(wire(native));
    const run = useSubagentRunStore.getState().runs[subagentRunStoreKey('run-1', 'execution-1')];
    expect(run?.status).toBe('failed');
    expect(
      run?.events.filter((event) => event.event === 'note').map((event) => event.agent_envelope)
    ).toEqual([native]);
  });
  it('restores unseen early native events and tool positions after late live/terminal observations', () => {
    ingestSubagentExecEvent(wire(envelope(5)));
    useSubagentRunStore.getState().ingest({
      kind: 'subagent',
      workspace_id: 'workspace-1',
      conversation_id: 'conversation-1',
      run_id: 'run-1',
      subagent_run_id: 'execution-1',
      agent: 'primary',
      event: 'failed',
      error: 'original failure',
    });
    const taskRun: TaskRun = {
      run_id: 'run-1',
      workspace_id: 'workspace-1',
      conversation_id: 'conversation-1',
      root_message_id: 'message-1',
      domain_profile: 'general',
      domain_binding: null,
      status: 'running',
      goal: 'original',
      goal_revision: 1,
      goal_sha256: 'sha',
      plan_id: null,
      route: '',
      attended_mode: 'attended',
      created_at: '2026-10-28T00:00:00Z',
      updated_at: '2026-10-28T00:00:00Z',
    };
    const notes = [1, 3, 5].map((sequence) => ({
      seq: String(sequence),
      run_id: 'run-1',
      task_id: 'task-1',
      step_id: 'execution-1',
      event_type: 'note' as const,
      payload: {
        kind: 'native_agent_event',
        execution_id: 'execution-1',
        agent: 'primary',
        agent_envelope: envelope(sequence),
      },
      timestamp: '2026-10-28T00:00:00Z',
    }));
    const tool: RuntimeTaskEvent = {
      seq: '2',
      run_id: 'run-1',
      task_id: 'task-1',
      step_id: 'original-call',
      event_type: 'tool_started',
      payload: { execution_id: 'execution-1', call_id: 'original-call', tool_name: 'read_file' },
      timestamp: '2026-10-28T00:00:00Z',
    };
    ingestTaskRuntimeSubagentEvents(taskRun, null, [notes[0], tool, notes[1], notes[2]]);
    const run = useSubagentRunStore.getState().runs[subagentRunStoreKey('run-1', 'execution-1')];
    expect(run?.status).toBe('failed');
    expect(
      run?.events.filter((event) => event.runtime_sequence != null).map((event) => event.event_id)
    ).toEqual(['native-1', 'runtime:2', 'native-3', 'native-5']);
  });
  it('keeps Settled as an observation and represents malformed payloads as unreadable', () => {
    const status = {
      ...envelope(1),
      payload: {
        type: 'execution_status',
        data: {
          execution_id: 'execution-1',
          request_id: null,
          sequence: 1,
          phase: 'Terminal',
          status: 'settled',
          reason: null,
        },
      },
    };
    const valid = wire(envelope(1));
    valid.payload = {
      kind: 'native_agent_event',
      execution_id: 'execution-1',
      agent: 'primary',
      agent_envelope: status,
    };
    ingestSubagentExecEvent(valid);
    const malformed = wire(envelope(2));
    malformed.payload = {
      kind: 'native_agent_event',
      execution_id: 'execution-1',
      agent_envelope: { ...envelope(2), payload: { type: 'error' } },
    };
    ingestSubagentExecEvent(malformed);
    const run = useSubagentRunStore.getState().runs[subagentRunStoreKey('run-1', 'execution-1')];
    expect(run?.status).toBe('running');
    expect(run?.events.at(-1)?.native_decode_error).toContain('不可读');
  });
  it('does not move later live output ahead when only a suffix gets durable coordinates', () => {
    const before = envelope(1),
      after = envelope(3);
    ingestSubagentExecEvent(wire(before));
    ingestSubagentExecEvent({
      ...wire(before),
      event: 'tool_started',
      payload: { call_id: 'call-between' },
    });
    ingestSubagentExecEvent(wire(after));
    const taskRun: TaskRun = {
      run_id: 'run-1',
      workspace_id: 'workspace-1',
      conversation_id: 'conversation-1',
      root_message_id: 'message-1',
      domain_profile: 'general',
      domain_binding: null,
      status: 'running',
      goal: 'original',
      goal_revision: 1,
      goal_sha256: 'sha',
      plan_id: null,
      route: '',
      attended_mode: 'attended',
      created_at: '2026-10-28T00:00:00Z',
      updated_at: '2026-10-28T00:00:00Z',
    };
    ingestTaskRuntimeSubagentEvents(taskRun, null, [
      {
        seq: '3',
        run_id: 'run-1',
        task_id: 'task-1',
        step_id: 'execution-1',
        event_type: 'note',
        payload: {
          kind: 'native_agent_event',
          execution_id: 'execution-1',
          agent: 'primary',
          agent_envelope: after,
        },
        timestamp: '2026-10-28T00:00:00Z',
      },
    ]);
    const run = useSubagentRunStore.getState().runs[subagentRunStoreKey('run-1', 'execution-1')];
    expect(run?.events.map((event) => event.agent_envelope?.event_id ?? event.call_id)).toEqual([
      'native-1',
      'call-between',
      'native-3',
    ]);
  });
  it('repositions replay-only tools after later pages provide their surrounding anchors', () => {
    ingestSubagentExecEvent(wire(envelope(1)));
    ingestSubagentExecEvent(wire(envelope(3)));
    const taskRun: TaskRun = {
      run_id: 'run-1',
      workspace_id: 'workspace-1',
      conversation_id: 'conversation-1',
      root_message_id: 'message-1',
      domain_profile: 'general',
      domain_binding: null,
      status: 'running',
      goal: 'original',
      goal_revision: 1,
      goal_sha256: 'sha',
      plan_id: null,
      route: '',
      attended_mode: 'attended',
      created_at: '2026-10-28T00:00:00Z',
      updated_at: '2026-10-28T00:00:00Z',
    };
    const tool: RuntimeTaskEvent = {
      seq: '2',
      run_id: 'run-1',
      task_id: 'task-1',
      step_id: 'middle-call',
      event_type: 'tool_started',
      payload: { execution_id: 'execution-1', call_id: 'middle-call' },
      timestamp: '2026-10-28T00:00:00Z',
    };
    ingestTaskRuntimeSubagentEvents(taskRun, null, [tool]);
    for (const sequence of [3, 1]) {
      ingestTaskRuntimeSubagentEvents(taskRun, null, [
        {
          seq: String(sequence),
          run_id: 'run-1',
          task_id: 'task-1',
          step_id: 'execution-1',
          event_type: 'note',
          payload: {
            kind: 'native_agent_event',
            execution_id: 'execution-1',
            agent: 'primary',
            agent_envelope: envelope(sequence),
          },
          timestamp: '2026-10-28T00:00:00Z',
        },
      ]);
    }
    const run = useSubagentRunStore.getState().runs[subagentRunStoreKey('run-1', 'execution-1')];
    expect(run?.events.map((event) => event.agent_envelope?.event_id ?? event.call_id)).toEqual([
      'native-1',
      'middle-call',
      'native-3',
    ]);
  });
  it('does not freeze shared chat replay arrival order over canonical native order', () => {
    ingestSubagentExecEvent(wire(envelope(5)));
    ingestSubagentExecEvent(wire(envelope(1)), {
      event_id: 'chat-1',
      stream_id: 'chat-stream',
      sequence: 1,
      timestamp: '2026-10-28T00:00:00Z',
    });
    ingestSubagentExecEvent(wire(envelope(5)), {
      event_id: 'chat-5',
      stream_id: 'chat-stream',
      sequence: 5,
      timestamp: '2026-10-28T00:00:00Z',
    });
    const run = useSubagentRunStore.getState().runs[subagentRunStoreKey('run-1', 'execution-1')];
    expect(run?.events.map((event) => event.agent_envelope?.event_id)).toEqual([
      'native-1',
      'native-5',
    ]);
  });

  it('retains exact durable sequence strings above the JavaScript integer boundary', () => {
    const taskRun: TaskRun = {
      run_id: 'run-1',
      workspace_id: 'workspace-1',
      conversation_id: 'conversation-1',
      root_message_id: 'message-1',
      domain_profile: 'general',
      domain_binding: null,
      status: 'running',
      goal: 'original',
      goal_revision: 1,
      goal_sha256: 'sha',
      plan_id: null,
      route: '',
      attended_mode: 'attended',
      created_at: '2026-10-28T00:00:00Z',
      updated_at: '2026-10-28T00:00:00Z',
    };
    for (const [seq, ordinal] of [
      ['9007199254740993', 3],
      ['9007199254740992', 1],
    ] as const) {
      ingestTaskRuntimeSubagentEvents(taskRun, null, [
        {
          seq,
          run_id: 'run-1',
          task_id: 'task-1',
          step_id: 'execution-1',
          event_type: 'note',
          payload: {
            kind: 'native_agent_event',
            execution_id: 'execution-1',
            agent: 'primary',
            agent_envelope: envelope(ordinal),
          },
          timestamp: '2026-10-28T00:00:00Z',
        },
      ]);
    }
    const run = useSubagentRunStore.getState().runs[subagentRunStoreKey('run-1', 'execution-1')];
    expect(run?.events.map((event) => event.runtime_sequence)).toEqual([
      '9007199254740992',
      '9007199254740993',
    ]);
  });
});
