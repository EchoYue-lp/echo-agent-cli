import { useEffect, useState } from 'react';
import {
  AlertCircle,
  ArrowLeft,
  CheckCircle2,
  Circle,
  ClipboardList,
  Forward,
  Gauge,
  Loader2,
  MessageSquareMore,
  OctagonX,
  TerminalSquare,
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
import { subagentResultPresentation } from '../../utils/subagentResult';
import MarkdownContent from '../common/MarkdownContent';
import { InlineToolCall } from '../chat/InlineToolCall';
import { SubagentResultView } from '../subagent/SubagentResultView';
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

export function SubagentDetailView({ run, onBack }: SubagentDetailViewProps) {
  const [activeTab, setActiveTab] = useState<'task' | 'process' | 'result'>(
    run.status === 'running' ? 'process' : 'result'
  );
  const presentation = subagentResultPresentation(run);
  const addToast = useToastStore((state) => state.addToast);
  const [controlPending, setControlPending] = useState<'message' | 'followup' | 'interrupt' | null>(
    null
  );
  const cacheSummary = cacheUsageForRuns([run]);
  const workspaceId = useTaskRuntimeStore((state) => state.activeRun?.workspace_id ?? null);
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

  useEffect(() => {
    if (run.status !== 'running') setActiveTab('result');
  }, [run.status]);

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
      <div className="shrink-0 border-b border-[var(--border-primary)] px-5 py-3 sm:px-8">
        <button
          type="button"
          onClick={onBack}
          className="mb-3 flex items-center gap-2 text-xs text-[var(--text-secondary)]"
        >
          <ArrowLeft size={14} />
          返回对话
        </button>

        <div className="flex min-w-0 items-start justify-between gap-4">
          <div className="min-w-0">
            <div className="flex items-center gap-2">
              {statusIcon(run)}
              <h2 className="truncate text-lg font-semibold text-[var(--text-primary)]">
                {run.agent || run.subagentRunId}
              </h2>
            </div>
            <div className="mt-1 flex flex-wrap gap-x-3 text-xs text-[var(--text-tertiary)]">
              <span>{statusLabel(run.status)}</span>
              {toolIds.length > 0 && <span>{toolIds.length} 工具</span>}
              <span>started {formatTime(run.startedAt)}</span>
            </div>
          </div>
          <div className="flex shrink-0 items-start gap-2">
            {run.status === 'running' && (
              <div className="flex items-center gap-1">
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
              </div>
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
            <div className="hidden w-72 md:block">
              <CacheUsageCard summary={cacheSummary} compact />
            </div>
          </div>
        </div>

        <div className="mt-4 flex gap-1 border-b border-[var(--border-primary)]">
          {(
            [
              ['task', '提示词 / 任务', ClipboardList],
              ['process', '执行过程', TerminalSquare],
              ['result', '结果', Gauge],
            ] as const
          ).map(([id, label, Icon]) => (
            <button
              key={id}
              type="button"
              onClick={() => setActiveTab(id)}
              className="flex items-center gap-1.5 border-b-2 px-3 py-2 text-xs font-medium"
              style={{
                borderColor: activeTab === id ? 'var(--accent)' : 'transparent',
                color: activeTab === id ? 'var(--text-primary)' : 'var(--text-tertiary)',
              }}
            >
              <Icon size={13} />
              {label}
            </button>
          ))}
        </div>
      </div>

      <div className="min-h-0 flex-1 overflow-y-auto px-5 py-4 sm:px-8">
        {activeTab === 'task' && (
          <div className="mx-auto max-w-[880px]">
            <SectionTitle title="提示词 / 任务" subtitle="分派给 subagent 的完整任务输入" />
            {run.task ? (
              <MarkdownContent content={run.task} className="text-sm" />
            ) : (
              <EmptyState text="这个 subagent 暂时没有记录任务输入。" />
            )}
          </div>
        )}

        {activeTab === 'process' && (
          <div className="mx-auto max-w-[880px]">
            <SectionTitle title="执行过程" subtitle="工具调用按实际执行顺序排列" />
            {toolIds.length === 0 ? (
              <EmptyState text="暂无工具执行。" />
            ) : (
              <div className="space-y-1">
                {toolIds.map((toolId, index) => (
                  <InlineToolCall key={toolId} toolId={toolId} index={index} />
                ))}
              </div>
            )}
          </div>
        )}

        {activeTab === 'result' && (
          <div className="mx-auto max-w-[880px]">
            <SectionTitle title="结果" subtitle="subagent 返回的完整最终结果" />
            {run.result || presentation.text ? (
              <SubagentResultView result={run.result} content={presentation.text} />
            ) : (
              <EmptyState text="还没有可展示的结果。" />
            )}
          </div>
        )}
      </div>
    </div>
  );
}

function SectionTitle({ title, subtitle }: { title: string; subtitle: string }) {
  return (
    <div className="mb-4">
      <h3 className="text-sm font-semibold text-[var(--text-primary)]">{title}</h3>
      <p className="mt-1 text-xs text-[var(--text-tertiary)]">{subtitle}</p>
    </div>
  );
}

function EmptyState({ text }: { text: string }) {
  return <div className="py-8 text-center text-sm text-[var(--text-tertiary)]">{text}</div>;
}
