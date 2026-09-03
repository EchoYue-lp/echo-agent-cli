import { useState } from 'react';
import {
  AlertCircle,
  ArrowLeft,
  CheckCircle2,
  Circle,
  Forward,
  Loader2,
  MessageSquareMore,
  OctagonX,
} from 'lucide-react';
import { taskRuntimeApi } from '../../api/endpoints';
import type { SubagentControlIdentity } from '../../generated';
import type { SubagentRunState } from '../../stores/subagentRunStore';
import { useToastStore } from '../../stores/toastStore';
import { useTaskRuntimeStore } from '../../stores/taskRuntimeStore';
import {
  toolExecutionIdsForOwner,
  toolExecutionOwnerKey,
  useToolExecutionStore,
} from '../../stores/toolExecutionStore';
import { statusLabel } from '../../utils/subagentProgress';
import { subagentOutcomePresentation } from '../../utils/subagentOutcome';
import { ExecutionProcessGroup } from '../chat/ExecutionProcessGroup';
import { InlineToolCall } from '../chat/InlineToolCall';
import { SubagentOutcomeView } from '../subagent/SubagentOutcomeView';
import { CacheUsageCard, cacheUsageForRuns } from './TaskRuntimePanel';

interface SubagentDetailViewProps {
  run: SubagentRunState;
  onBack: () => void;
}

function statusIcon(run: SubagentRunState) {
  if (run.status === 'running') {
    return <Loader2 size={16} className="animate-spin text-[var(--color-info)]" />;
  }
  if (run.status === 'completed') {
    return <CheckCircle2 size={16} className="text-[var(--color-success)]" />;
  }
  if (run.status === 'failed' || run.status === 'timed_out') {
    return <AlertCircle size={16} className="text-[var(--color-error)]" />;
  }
  return <Circle size={16} className="text-[var(--text-tertiary)]" />;
}

function formatTime(epochMs: number): string {
  const date = new Date(epochMs);
  if (Number.isNaN(date.getTime())) return 'unknown';
  return date.toLocaleTimeString([], { hour: '2-digit', minute: '2-digit', second: '2-digit' });
}

function commandId(action: string): string {
  const randomId = globalThis.crypto?.randomUUID?.();
  return randomId ?? `${action}-${Date.now()}-${Math.random().toString(16).slice(2)}`;
}

/**
 * Full subagent timeline, mounted in the right workspace panel.
 *
 * One vertical stream — task prompt, tool rows, final output — reusing the
 * exact row components of the main chat (InlineToolCall, ExecutionProcessGroup,
 * SubagentOutcomeView). Main agent and subagent share one rendering language;
 * the inline chat block (SubagentStreamBlock) is only a one-line status row.
 */
export function SubagentDetailView({ run, onBack }: SubagentDetailViewProps) {
  const presentation = subagentOutcomePresentation(run);
  const addToast = useToastStore((state) => state.addToast);
  const [controlPending, setControlPending] = useState<'message' | 'followup' | 'interrupt' | null>(
    null
  );
  const cacheSummary = cacheUsageForRuns([run]);
  const focusedWorkspaceId = useTaskRuntimeStore((state) => state.activeRun?.workspace_id ?? null);
  const workspaceId = run.workspaceId ?? focusedWorkspaceId;
  const ownerKey = toolExecutionOwnerKey(
    {
      kind: 'subagent',
      subagent_run_id: run.subagentRunId,
    },
    run.runId
  );
  const toolIds = useToolExecutionStore((state) =>
    toolExecutionIdsForOwner(state.idsByOwner, ownerKey)
  );
  const terminal = run.status !== 'running';

  const controlIdentity = (
    action: 'message' | 'followup' | 'interrupt'
  ): SubagentControlIdentity | null => {
    if (!run.taskId || run.planRevision == null || run.attempt == null) return null;
    const attempt = action === 'followup' ? run.attempt + 1 : run.attempt;
    const executionId =
      action === 'followup'
        ? `pending:${run.runId}:${run.taskId}:${run.planRevision}:${attempt}`
        : run.subagentRunId;
    return {
      run_id: run.runId,
      task_id: run.taskId,
      execution_id: executionId,
      plan_revision: run.planRevision,
      attempt,
      command_id: commandId(action),
    };
  };

  const runControl = async (action: 'message' | 'followup' | 'interrupt') => {
    const identity = controlIdentity(action);
    if (!identity || !workspaceId) {
      addToast('error', 'Subagent identity is not available yet');
      return;
    }
    const instruction =
      action === 'interrupt' ? null : window.prompt(action === 'message' ? 'Message' : 'Follow-up');
    if (action !== 'interrupt' && !instruction?.trim()) return;
    setControlPending(action);
    try {
      const receipt =
        action === 'message'
          ? await taskRuntimeApi.sendSubagentMessage(workspaceId, identity, instruction ?? '')
          : action === 'followup'
            ? await taskRuntimeApi.queueSubagentGuidance(workspaceId, identity, instruction ?? '')
            : await taskRuntimeApi.interruptSubagent(workspaceId, identity);
      addToast(
        receipt.status === 'rejected' ? 'warning' : 'success',
        `Subagent command ${receipt.status}`
      );
    } catch (error) {
      addToast('error', error instanceof Error ? error.message : String(error));
    } finally {
      setControlPending(null);
    }
  };

  return (
    <div className="flex h-full min-h-0 flex-col bg-[var(--bg-primary)]">
      <div className="shrink-0 border-b border-[var(--border-primary)] px-4 py-3">
        <button
          type="button"
          onClick={onBack}
          className="mb-2 flex items-center gap-2 text-xs text-[var(--text-secondary)] hover:text-[var(--text-primary)]"
        >
          <ArrowLeft size={14} />
          返回
        </button>

        <div className="flex items-center gap-2">
          {statusIcon(run)}
          <h2 className="min-w-0 truncate text-base font-semibold text-[var(--text-primary)]">
            {run.agent || run.subagentRunId}
          </h2>
        </div>
        <div className="mt-1 flex flex-wrap gap-x-3 text-xs text-[var(--text-tertiary)]">
          <span>{statusLabel(run.status)}</span>
          {toolIds.length > 0 && <span>{toolIds.length} 工具</span>}
          <span>started {formatTime(run.startedAt)}</span>
        </div>

        <div className="mt-3 flex flex-wrap items-center gap-2">
          {run.status === 'running' && (
            <>
              <button
                type="button"
                title="Message Subagent"
                aria-label="Message Subagent"
                disabled={controlPending !== null}
                onClick={() => void runControl('message')}
                className="grid size-8 place-items-center rounded-md border border-[var(--border-primary)] text-[var(--text-secondary)] hover:bg-[var(--bg-hover)] disabled:opacity-50"
              >
                {controlPending === 'message' ? (
                  <Loader2 size={14} className="animate-spin" />
                ) : (
                  <MessageSquareMore size={14} />
                )}
              </button>
              <button
                type="button"
                title="Interrupt Subagent"
                aria-label="Interrupt Subagent"
                disabled={controlPending !== null}
                onClick={() => void runControl('interrupt')}
                className="grid size-8 place-items-center rounded-md border border-[var(--border-primary)] text-[var(--color-error)] hover:bg-[var(--bg-hover)] disabled:opacity-50"
              >
                {controlPending === 'interrupt' ? (
                  <Loader2 size={14} className="animate-spin" />
                ) : (
                  <OctagonX size={14} />
                )}
              </button>
            </>
          )}
          {run.status !== 'completed' && (
            <button
              type="button"
              title="Queue guidance for next attempt"
              aria-label="Queue guidance for next attempt"
              disabled={controlPending !== null}
              onClick={() => void runControl('followup')}
              className="grid size-8 place-items-center rounded-md border border-[var(--border-primary)] text-[var(--text-secondary)] hover:bg-[var(--bg-hover)] disabled:opacity-50"
            >
              {controlPending === 'followup' ? (
                <Loader2 size={14} className="animate-spin" />
              ) : (
                <Forward size={14} />
              )}
            </button>
          )}
          <div className="min-w-0 flex-1">
            <CacheUsageCard summary={cacheSummary} compact />
          </div>
        </div>
      </div>

      <div className="min-h-0 flex-1 overflow-y-auto px-4 py-4">
        <div className="space-y-4">
          {run.task && (
            <div className="rounded-2xl bg-[var(--bg-user-bubble)] px-4 py-3 text-sm leading-relaxed text-[var(--text-primary)]">
              <div className="whitespace-pre-wrap break-words">{run.task}</div>
            </div>
          )}

          {toolIds.length > 0 && (
            <ExecutionProcessGroup completed={terminal}>
              <div className="space-y-1">
                {toolIds.map((toolId, index) => (
                  <InlineToolCall key={toolId} toolId={toolId} index={index} />
                ))}
              </div>
            </ExecutionProcessGroup>
          )}

          {run.outcome || presentation.text ? (
            <SubagentOutcomeView outcome={run.outcome} content={presentation.text} />
          ) : run.status === 'running' ? (
            <div className="flex items-center gap-2 py-2 text-xs text-[var(--text-tertiary)]">
              <Loader2 size={12} className="animate-spin" />
              正在执行，工具调用会实时出现在上方…
            </div>
          ) : (
            <div className="py-8 text-center text-sm text-[var(--text-tertiary)]">
              还没有可展示的结果。
            </div>
          )}
        </div>
      </div>
    </div>
  );
}
