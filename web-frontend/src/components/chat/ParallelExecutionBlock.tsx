import { useMemo, memo } from 'react';
import { useTaskRuntimeStore } from '../../stores/taskRuntimeStore';
import { useSubagentRunStore } from '../../stores/subagentRunStore';
import { useChatStore } from '../../stores/chatStore';
import { WorkerStreamBlock } from './WorkerStreamBlock';

/**
 * The "并行执行" segment in the one-stream layout.
 * Shows top-level subagents (parent empty) for the active run.
 *
 * Rendered INLINE in the message stream — NOT as a standalone bordered card.
 * Subagents flow as part of the conversation, just like thinking segments and
 * tool calls, matching the Cursor/Codex/Claude Code desktop pattern.
 *
 * Message association: each SubagentRun carries a `messageId` (populated from
 * the run's root_message_id by the Phase 3a backend). If a run has no
 * messageId (non-chat path), it falls back to the latest assistant message.
 */
interface ParallelExecutionBlockProps {
  messageId: string;
}

export const ParallelExecutionBlock = memo(function ParallelExecutionBlock({
  messageId,
}: ParallelExecutionBlockProps) {
  const activeRun = useTaskRuntimeStore((s) => s.activeRun);
  const runs = useSubagentRunStore((s) => s.runs);
  const lastAssistantMessageId = useChatStore((s) => {
    for (let i = s.messages.length - 1; i >= 0; i -= 1) {
      if (s.messages[i]?.role === 'assistant') return s.messages[i]?.id ?? null;
    }
    return null;
  });

  const visibleWorkers = useMemo(() => {
    const isLatestAssistant = messageId === lastAssistantMessageId;
    const allRuns = Object.values(runs);
    return allRuns
      .filter(
        (w) =>
          (!activeRun || w.runId === activeRun.run_id) &&
          (w.messageId === messageId || (!w.messageId && isLatestAssistant)) &&
          // Top-level subagents: parent is empty OR equals the run_id.
          (!w.parent || w.parent === w.runId)
      )
      .sort((a, b) => a.startedAt - b.startedAt);
  }, [activeRun, runs, messageId, lastAssistantMessageId]);

  if (visibleWorkers.length === 0) return null;

  return (
    <>
      {visibleWorkers.map((w) => (
        <WorkerStreamBlock
          key={w.subagentRunId}
          worker={w}
          allWorkers={Object.values(runs).filter((x) => x.runId === w.runId)}
        />
      ))}
    </>
  );
});
