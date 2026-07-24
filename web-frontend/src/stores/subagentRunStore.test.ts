import { beforeEach, describe, expect, it } from 'vitest';
import { useSubagentRunStore, type ExecutionEvent } from './subagentRunStore';

describe('subagentRunStore terminal result', () => {
  beforeEach(() => {
    useSubagentRunStore.getState().clear();
  });

  it('preserves the complete timed-out result contract', () => {
    const event: ExecutionEvent = {
      kind: 'subagent',
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

    const run = useSubagentRunStore.getState().runs['task-1:1'];
    expect(run?.status).toBe('timed_out');
    expect(run?.finalOutput).toBe('partial report body');
    expect(run?.result?.summary).toBe(event.summary);
    expect(run?.result?.artifacts[0]?.path).toBe('/tmp/report.json');
    expect(run?.result?.artifacts[0]?.bytes).toBe(42n);
    expect(run?.result?.verification).toEqual(event.verification);
    expect(run?.result?.remaining_work).toEqual(event.remaining_work);
    expect(run?.result?.touched_files).toEqual(event.touched_files);
  });

  it('keeps a terminal Subagent terminal when a duplicate started event arrives', () => {
    const base = {
      kind: 'subagent' as const,
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

    const completed = useSubagentRunStore.getState().runs['task-merge:1'];
    expect(completed?.status).toBe('completed');
    expect(completed?.result?.summary).toBe('implementation finished');
  });

  it('keeps retry attempts in separate SubagentRun records', () => {
    const base = {
      kind: 'subagent' as const,
      run_id: 'run-retry',
      task_id: 'task-retry',
      agent: 'explorer',
      event: 'started' as const,
    };

    useSubagentRunStore.getState().ingest({ ...base, subagent_run_id: 'task-retry:1' });
    useSubagentRunStore.getState().ingest({ ...base, subagent_run_id: 'task-retry:2' });

    expect(Object.keys(useSubagentRunStore.getState().runs).sort()).toEqual([
      'task-retry:1',
      'task-retry:2',
    ]);
  });
});
