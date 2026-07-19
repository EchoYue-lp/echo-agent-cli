import { useMemo, memo } from 'react';
import { useTaskRuntimeStore } from '../../stores/taskRuntimeStore';
import { useSubagentRunStore } from '../../stores/subagentRunStore';
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
    const isLatestAssistant = messageId === lastAssistantMessageId;
    const allRuns = Object.values(runs);
    return allRuns
      .filter(
        (w) =>
          // Skip the synthetic "main" run — the main agent's thinking/tool
          // stream is already rendered via `chat://event` (ChatPanel), so
          // showing it again as a SubagentStreamBlock would duplicate. The
          // "main" entry is still kept in the store for cache diagnostics.
          w.subagentRunId !== 'main' &&
          (!activeRun ||
            w.runId === activeRun.run_id ||
            w.conversationId === activeRun.conversation_id) &&
          (w.messageId === messageId || (!w.messageId && isLatestAssistant)) &&
          // Top-level subagents: parent is empty OR equals the run_id.
          (!w.parent || w.parent === w.runId)
      )
      .sort((a, b) => a.startedAt - b.startedAt);
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
