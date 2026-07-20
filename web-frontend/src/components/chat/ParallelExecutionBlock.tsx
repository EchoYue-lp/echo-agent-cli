import { useMemo, memo } from 'react';
import type { TaskRun } from '../../generated';
import { useTaskRuntimeStore } from '../../stores/taskRuntimeStore';
import { useSubagentRunStore, type SubagentRunState } from '../../stores/subagentRunStore';
import { useChatStore } from '../../stores/chatStore';
import { SubagentStreamBlock } from './SubagentStreamBlock';

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
 *
 * `parent` means parent **subagent_run_id** (nested subagent), NOT the parent
 * agent name. Top-level agent_tool / plan_execute subagents omit parent.
 */
interface ParallelExecutionBlockProps {
  messageId: string;
}

export function visibleSubagentRuns(
  runs: readonly SubagentRunState[],
  activeRun: Pick<TaskRun, 'run_id' | 'conversation_id'> | null,
  messageId: string,
  lastAssistantMessageId: string | null
): SubagentRunState[] {
  const isLatestAssistant = messageId === lastAssistantMessageId;
  return runs
    .filter((run) => {
      if (run.subagentRunId === 'main' || (run.parent && run.parent !== run.runId)) {
        return false;
      }
      if (run.messageId === messageId) {
        return true;
      }
      if (run.messageId || !isLatestAssistant) {
        return false;
      }
      return (
        !activeRun ||
        run.runId === activeRun.run_id ||
        run.conversationId === activeRun.conversation_id
      );
    })
    .sort((a, b) => a.startedAt - b.startedAt);
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

  const visibleRuns = useMemo(() => {
    return visibleSubagentRuns(Object.values(runs), activeRun, messageId, lastAssistantMessageId);
  }, [activeRun, runs, messageId, lastAssistantMessageId]);

  if (visibleRuns.length === 0) return null;

  return (
    <>
      {visibleRuns.map((w) => (
        <SubagentStreamBlock
          key={w.subagentRunId}
          run={w}
          allRuns={Object.values(runs).filter((x) => x.runId === w.runId)}
        />
      ))}
    </>
  );
});
