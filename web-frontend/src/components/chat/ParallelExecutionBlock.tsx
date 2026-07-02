import { useMemo, memo } from 'react';
import { useTaskRuntimeStore } from '../../stores/taskRuntimeStore';
import { useWorkerTraceStore } from '../../stores/workerTraceStore';
import { useChatStore } from '../../stores/chatStore';
import { WorkerStreamBlock } from './WorkerStreamBlock';

/**
 * The "并行执行" segment in the one-stream layout.
 * Shows top-level workers (parentWorkerId empty) for the active run.
 *
 * Rendered INLINE in the message stream — NOT as a standalone bordered card.
 * Workers flow as part of the conversation, just like thinking segments and
 * tool calls, matching the Cursor/Codex/Claude Code desktop pattern.
 */
interface ParallelExecutionBlockProps {
  messageId: string;
}

export const ParallelExecutionBlock = memo(function ParallelExecutionBlock({
  messageId,
}: ParallelExecutionBlockProps) {
  const activeRun = useTaskRuntimeStore((s) => s.activeRun);
  const workers = useWorkerTraceStore((s) => s.workers);
  const lastAssistantMessageId = useChatStore((s) => {
    for (let i = s.messages.length - 1; i >= 0; i -= 1) {
      if (s.messages[i]?.role === 'assistant') return s.messages[i]?.id ?? null;
    }
    return null;
  });

  const visibleWorkers = useMemo(() => {
    const isLatestAssistant = messageId === lastAssistantMessageId;
    const allWorkers = Object.values(workers);
    return allWorkers
      .filter(
        (w) =>
          (!activeRun || w.runId === activeRun.run_id) &&
          (w.messageId === messageId || (!w.messageId && isLatestAssistant)) &&
          // Top-level workers: parentWorkerId is empty OR equals the run_id.
          (!w.parentWorkerId || w.parentWorkerId === w.runId)
      )
      .sort((a, b) => (a.startedAt ?? '').localeCompare(b.startedAt ?? ''));
  }, [activeRun, workers, messageId, lastAssistantMessageId]);

  if (visibleWorkers.length === 0) return null;

  return (
    <>
      {visibleWorkers.map((w) => (
        <WorkerStreamBlock
          key={w.workerId}
          worker={w}
          allWorkers={Object.values(workers).filter((x) => x.runId === w.runId)}
        />
      ))}
    </>
  );
});
