import { useMemo, useState } from 'react';
import {
  AlertCircle,
  CheckCircle2,
  Circle,
  Loader2,
  OctagonX,
  PanelRightClose,
  SendHorizontal,
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
import { AgentComposerFrame } from '../chat/AgentComposerFrame';
import { AgentPane } from '../chat/AgentPane';
import { ExecutionProcessGroup } from '../chat/ExecutionProcessGroup';
import { InlineToolCall } from '../chat/InlineToolCall';
import { ThinkingSegment } from '../chat/ThinkingSegment';
import { SubagentOutcomeView } from '../subagent/SubagentOutcomeView';

interface SubagentDetailViewProps {
  run: SubagentRunState;
  onBack: () => void;
}

function statusIcon(run: SubagentRunState) {
  if (run.status === 'running') {
    return <Loader2 size={15} className="animate-spin text-[var(--color-info)]" />;
  }
  if (run.status === 'completed') {
    return <CheckCircle2 size={15} className="text-[var(--color-success)]" />;
  }
  if (run.status === 'failed' || run.status === 'timed_out') {
    return <AlertCircle size={15} className="text-[var(--color-error)]" />;
  }
  return <Circle size={15} className="text-[var(--text-tertiary)]" />;
}

function formatTime(epochMs: number): string {
  const date = new Date(epochMs);
  if (Number.isNaN(date.getTime())) return '未知时间';
  return date.toLocaleTimeString([], { hour: '2-digit', minute: '2-digit' });
}

function commandId(action: string): string {
  const randomId = globalThis.crypto?.randomUUID?.();
  return randomId ?? `${action}-${Date.now()}-${Math.random().toString(16).slice(2)}`;
}

export type SubagentTimelineEntry =
  | { kind: 'thinking'; key: string; content: string }
  | { kind: 'text'; key: string; content: string }
  | { kind: 'tool'; key: string; toolId: string }
  | { kind: 'gap'; key: string; from?: number; to?: number };

export function subagentTimelineEntries(
  run: SubagentRunState,
  toolIds: readonly string[],
  tools: Record<string, { call_id: string }>
): SubagentTimelineEntry[] {
  const toolByCall = new Map<string, string>();
  for (const toolId of toolIds) {
    const callId = tools[toolId]?.call_id;
    if (callId) toolByCall.set(callId, toolId);
  }
  const usedTools = new Set<string>();
  const entries: SubagentTimelineEntry[] = [];
  const appendText = (kind: 'thinking' | 'text', key: string, content: string) => {
    if (!content) return;
    const previous = entries.at(-1);
    if (previous?.kind === kind) {
      previous.content += content;
    } else {
      entries.push({ kind, key, content });
    }
  };

  run.events.forEach((event, index) => {
    const key = event.event_id ?? `${event.event}-${index}`;
    if (event.event === 'thinking_delta') {
      appendText('thinking', key, typeof event.content === 'string' ? event.content : '');
    } else if (event.event === 'token_delta') {
      appendText('text', key, typeof event.content === 'string' ? event.content : '');
    } else if (event.event === 'tool_started' && typeof event.call_id === 'string') {
      const toolId = toolByCall.get(event.call_id);
      if (toolId) {
        usedTools.add(toolId);
        entries.push({ kind: 'tool', key, toolId });
      }
    } else if (event.event === 'subagent_stream_gap') {
      entries.push({
        kind: 'gap',
        key,
        from: event.requested_after,
        to: event.available_from != null ? event.available_from - 1 : event.latest_sequence,
      });
    }
  });
  for (const toolId of toolIds) {
    if (!usedTools.has(toolId)) entries.push({ kind: 'tool', key: toolId, toolId });
  }
  return entries;
}

export function visibleSubagentTimelineEntries(
  terminal: boolean,
  finalOutput: string,
  entries: SubagentTimelineEntry[]
): SubagentTimelineEntry[] {
  const normalizedOutput = finalOutput.replace(/\s+/g, ' ').trim();
  const normalizedStream = entries
    .filter(
      (entry): entry is Extract<SubagentTimelineEntry, { kind: 'text' }> => entry.kind === 'text'
    )
    .map((entry) => entry.content)
    .join('')
    .replace(/\s+/g, ' ')
    .trim();
  const terminalCoversStream =
    normalizedOutput.length > 0 &&
    normalizedStream.length > 0 &&
    normalizedOutput.includes(normalizedStream);
  return terminal && terminalCoversStream
    ? entries.filter((entry) => entry.kind !== 'text')
    : entries;
}

export function SubagentDetailView({ run, onBack }: SubagentDetailViewProps) {
  const presentation = subagentOutcomePresentation(run);
  const addToast = useToastStore((state) => state.addToast);
  const [instruction, setInstruction] = useState('');
  const [controlPending, setControlPending] = useState<'submit' | 'interrupt' | null>(null);
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
  const tools = useToolExecutionStore((state) => state.tools);
  const timeline = useMemo(
    () => subagentTimelineEntries(run, toolIds, tools),
    [run, toolIds, tools]
  );
  const terminal = run.status !== 'running';
  const visibleTimeline = useMemo(
    () => visibleSubagentTimelineEntries(terminal, run.finalOutput ?? '', timeline),
    [run.finalOutput, terminal, timeline]
  );
  const thinkingCount = visibleTimeline.filter((entry) => entry.kind === 'thinking').length;
  const canControl = Boolean(run.taskId && run.planRevision != null && run.attempt != null);

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

  const submitInstruction = async () => {
    const text = instruction.trim();
    const action = terminal ? 'followup' : 'message';
    const identity = controlIdentity(action);
    if (!text || !identity || !workspaceId) return;
    setControlPending('submit');
    try {
      const receipt = terminal
        ? await taskRuntimeApi.queueSubagentGuidance(workspaceId, identity, text)
        : await taskRuntimeApi.sendSubagentMessage(workspaceId, identity, text);
      const rejected = receipt.status === 'rejected';
      addToast(rejected ? 'warning' : 'success', `Subagent command ${receipt.status}`);
      if (!rejected) setInstruction('');
    } catch (error) {
      addToast('error', error instanceof Error ? error.message : String(error));
    } finally {
      setControlPending(null);
    }
  };

  const interrupt = async () => {
    const identity = controlIdentity('interrupt');
    if (!identity || !workspaceId) {
      addToast('error', 'Subagent identity is not available yet');
      return;
    }
    setControlPending('interrupt');
    try {
      const receipt = await taskRuntimeApi.interruptSubagent(workspaceId, identity);
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
    <AgentPane
      ariaLabel={`${run.agent || 'Subagent'}会话`}
      header={
        <>
          {statusIcon(run)}
          <div className="min-w-0 flex-1">
            <div className="flex min-w-0 items-center gap-2">
              <h2 className="truncate text-sm font-medium text-[var(--text-primary)]">
                {run.agent || 'Subagent'}
              </h2>
              {run.attempt != null && (
                <span className="shrink-0 text-[10px] text-[var(--text-tertiary)]">
                  attempt {run.attempt}
                </span>
              )}
            </div>
            <div className="flex min-w-0 items-center gap-2 text-[10px] text-[var(--text-tertiary)]">
              <span>{statusLabel(run.status)}</span>
              {toolIds.length > 0 && <span>{toolIds.length} 工具</span>}
              <span>{formatTime(run.startedAt)}</span>
            </div>
          </div>
          {run.status === 'running' && canControl && (
            <button
              type="button"
              title="中断 Subagent"
              aria-label="中断 Subagent"
              disabled={controlPending !== null}
              onClick={() => void interrupt()}
              className="flex h-7 w-7 shrink-0 items-center justify-center rounded-md text-[var(--text-tertiary)] hover:bg-[var(--color-error-bg)] hover:text-[var(--color-error)] disabled:opacity-40"
            >
              {controlPending === 'interrupt' ? (
                <Loader2 size={13} className="animate-spin" />
              ) : (
                <OctagonX size={13} />
              )}
            </button>
          )}
          <button
            type="button"
            title="关闭 Subagent 分栏"
            aria-label="关闭 Subagent 分栏"
            autoFocus={!canControl || !workspaceId}
            onClick={onBack}
            className="flex h-7 w-7 shrink-0 items-center justify-center rounded-md text-[var(--text-tertiary)] hover:bg-[var(--bg-hover)] hover:text-[var(--text-primary)]"
          >
            <PanelRightClose size={14} />
          </button>
        </>
      }
      footer={
        <div className="border-t border-[var(--border-secondary)] px-3 pb-3 pt-2">
          <AgentComposerFrame>
            <textarea
              aria-label={terminal ? 'Subagent 后续任务' : 'Subagent 消息'}
              value={instruction}
              onChange={(event) => setInstruction(event.target.value)}
              onKeyDown={(event) => {
                if (event.key === 'Enter' && !event.shiftKey) {
                  event.preventDefault();
                  void submitInstruction();
                }
              }}
              rows={1}
              autoFocus={canControl && Boolean(workspaceId)}
              disabled={!canControl || !workspaceId || controlPending !== null}
              placeholder={
                canControl && workspaceId
                  ? terminal
                    ? '安排后续任务'
                    : '向 Subagent 发送消息'
                  : '当前执行缺少可用控制身份'
              }
              className="max-h-32 min-h-7 flex-1 resize-none bg-transparent px-1 py-1 text-sm leading-5 text-[var(--text-primary)] outline-none placeholder:text-[var(--text-tertiary)] disabled:cursor-not-allowed"
            />
            <button
              type="button"
              title={terminal ? '发送后续任务' : '发送消息'}
              aria-label={terminal ? '发送 Subagent 后续任务' : '发送 Subagent 消息'}
              disabled={
                !instruction.trim() || !canControl || !workspaceId || controlPending !== null
              }
              onClick={() => void submitInstruction()}
              className="ml-2 flex h-8 w-8 shrink-0 items-center justify-center rounded-full bg-[var(--accent)] text-[var(--text-on-accent)] disabled:opacity-20"
            >
              {controlPending === 'submit' ? (
                <Loader2 size={14} className="animate-spin" />
              ) : (
                <SendHorizontal size={14} />
              )}
            </button>
          </AgentComposerFrame>
        </div>
      }
    >
      <div className="mx-auto w-full max-w-[760px] space-y-4 px-4 py-4">
        {run.task && (
          <div className="ml-auto max-w-[92%] rounded-lg bg-[var(--bg-user-bubble)] px-3 py-2.5 text-sm leading-relaxed text-[var(--text-primary)]">
            <div className="whitespace-pre-wrap break-words">{run.task}</div>
          </div>
        )}

        {visibleTimeline.length > 0 && (
          <ExecutionProcessGroup completed={terminal}>
            <div className="space-y-1">
              {visibleTimeline.map((entry, index) => {
                if (entry.kind === 'thinking') {
                  const thinkingIndex = visibleTimeline
                    .slice(0, index + 1)
                    .filter((item) => item.kind === 'thinking').length;
                  return (
                    <ThinkingSegment
                      key={entry.key}
                      index={thinkingIndex}
                      total={thinkingCount}
                      content={entry.content}
                      isStreaming={!terminal}
                    />
                  );
                }
                if (entry.kind === 'text') {
                  return (
                    <div
                      key={entry.key}
                      className="whitespace-pre-wrap break-words py-1 text-sm leading-relaxed text-[var(--text-primary)]"
                    >
                      {entry.content}
                    </div>
                  );
                }
                if (entry.kind === 'gap') {
                  return (
                    <div
                      key={entry.key}
                      role="status"
                      className="border-l-2 border-[var(--color-warning)] py-1 pl-2 text-xs text-[var(--text-secondary)]"
                    >
                      部分实时过程未保留
                      {entry.from != null && entry.to != null
                        ? `（sequence ${entry.from + 1}-${entry.to}）`
                        : ''}
                    </div>
                  );
                }
                return <InlineToolCall key={entry.key} toolId={entry.toolId} index={index} />;
              })}
            </div>
          </ExecutionProcessGroup>
        )}

        {run.outcome || presentation.text ? (
          <SubagentOutcomeView
            outcome={run.outcome}
            content={presentation.text}
            error={run.error}
          />
        ) : run.status === 'running' ? (
          <div className="flex items-center gap-2 py-2 text-xs text-[var(--text-tertiary)]">
            <Loader2 size={12} className="animate-spin" />
            正在执行，工具调用会实时出现在上方
          </div>
        ) : (
          <div className="py-8 text-center text-sm text-[var(--text-tertiary)]">
            还没有可展示的结果
          </div>
        )}
      </div>
    </AgentPane>
  );
}
