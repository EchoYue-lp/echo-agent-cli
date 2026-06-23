import { useMemo, memo } from 'react';
import { useTaskRuntimeStore } from '../../stores/taskRuntimeStore';
import { useWorkerTraceStore } from '../../stores/workerTraceStore';
import { WorkerStreamBlock } from './WorkerStreamBlock';

/**
 * The "并行执行" segment in the one-stream layout.
 * Shows top-level workers (parentWorkerId empty) for the active run.
 *
 * Rendered INLINE in the message stream — NOT as a standalone bordered card.
 * Workers flow as part of the conversation, just like thinking segments and
 * tool calls, matching the Cursor/Codex/Claude Code desktop pattern.
 */
export const ParallelExecutionBlock = memo(function ParallelExecutionBlock() {
  const activeRun = useTaskRuntimeStore((s) => s.activeRun);
  const workers = useWorkerTraceStore((s) => s.workers);

  const visibleWorkers = useMemo(() => {
    if (!activeRun) return [];
    return Object.values(workers)
      .filter(
        (w) =>
          w.runId === activeRun.run_id &&
          // Top-level workers: parentWorkerId is empty OR equals the run_id.
          // (TaskRuntime path emits top-level workers with parent_worker_id =
          // run_id via SubagentEvent bridge; agent_tool path leaves it empty.
          // Both are top-level — their parent is the run itself, not another worker.)
          !w.parentWorkerId || w.parentWorkerId === activeRun.run_id
      )
      .sort((a, b) => (a.startedAt ?? '').localeCompare(b.startedAt ?? ''));
  }, [activeRun, workers]);

  if (visibleWorkers.length === 0) return null;

  // Inline: workers flow directly in the stream. No outer card, no separate
  // "并行执行" header bar. Each worker is its own inline collapsible block
  // (handled by WorkerStreamBlock), visually continuous with the surrounding
  // thinking segments and tool calls.
  return (
    <>
      {visibleWorkers.map((w) => (
        <WorkerStreamBlock
          key={w.workerId}
          worker={w}
          allWorkers={Object.values(workers).filter((x) => x.runId === activeRun!.run_id)}
        />
      ))}
    </>
  );
});
