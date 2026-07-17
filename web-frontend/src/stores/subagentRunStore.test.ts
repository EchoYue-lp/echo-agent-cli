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
      terminal_status: 'timed_out',
      contract_version: 1,
      summary: 'worker timed out after writing a partial report',
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
    expect(run?.summary).toBe(event.summary);
    expect(run?.artifacts).toEqual(event.artifacts);
    expect(run?.verification).toEqual(event.verification);
    expect(run?.remainingWork).toEqual(event.remaining_work);
    expect(run?.touchedFiles).toEqual(event.touched_files);
  });
});
