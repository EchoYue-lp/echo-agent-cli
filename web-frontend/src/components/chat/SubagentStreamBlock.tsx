import { useEffect, useRef, useState, memo, useMemo } from 'react';
import {
  Loader2,
  CheckCircle2,
  AlertCircle,
  Circle,
  ChevronDown,
  ChevronRight,
  Brain,
  Files,
} from 'lucide-react';
import type { SubagentRunState, ExecutionEvent } from '../../stores/subagentRunStore';
import MarkdownContent from '../common/MarkdownContent';
import { InlineToolCall } from './InlineToolCall';
import { isSubagentDispatchTool } from './tools/toolRenderers';
import { SubagentResultView } from '../subagent/SubagentResultView';
import {
  computeSubagentProgress,
  progressSummary,
  statusLabel,
} from '../../utils/subagentProgress';
import { subagentResultPresentation, withoutPromotedThinking } from '../../utils/subagentResult';

interface SubagentStreamBlockProps {
  run: SubagentRunState;
  /** All execution traces in this run (for recursive child lookup). */
  allRuns: SubagentRunState[];
  taskTitle?: string;
}

type SubagentTab = 'task' | 'process' | 'result';

/** Reconstruct a subagent's thinking+tool loop from raw events. */
interface SubagentStep {
  type: 'thinking' | 'tool';
  content?: string;
  toolStart?: ExecutionEvent;
  toolResult?: ExecutionEvent;
}

function reconstructSteps(events: ExecutionEvent[]): {
  steps: SubagentStep[];
  thinkingTotal: number;
} {
  const steps: SubagentStep[] = [];
  let thinkingTotal = 0;
  const currentThinking: string[] = [];
  const pendingTools: ExecutionEvent[] = [];

  const flushThinking = () => {
    if (currentThinking.length > 0) {
      const content = currentThinking.join('').trim();
      if (content) {
        steps.push({ type: 'thinking', content });
        thinkingTotal++;
      }
      currentThinking.length = 0;
    }
  };

  for (const e of events) {
    if (e.event === 'thinking_delta') {
      if (e.content) currentThinking.push(e.content);
    } else if (e.event === 'thinking_ended') {
      flushThinking();
    } else if (e.event === 'tool_started') {
      flushThinking();
      pendingTools.push(e);
    } else if (e.event === 'tool_completed') {
      const name = String(e.name ?? '');
      const idx = pendingTools.findIndex((p) => String(p.name ?? '') === name);
      const start = idx >= 0 ? pendingTools.splice(idx, 1)[0] : undefined;
      steps.push({ type: 'tool', toolStart: start, toolResult: e });
    }
  }
  flushThinking();
  for (const start of pendingTools) {
    steps.push({ type: 'tool', toolStart: start });
  }
  return { steps, thinkingTotal };
}

export const SubagentStreamBlock = memo(function SubagentStreamBlock({
  run,
  allRuns,
  taskTitle,
}: SubagentStreamBlockProps) {
  const [expanded, setExpanded] = useState(run.status === 'running');
  const [activeTab, setActiveTab] = useState<SubagentTab>(
    run.status === 'running' ? 'process' : 'result'
  );
  const previousStatus = useRef(run.status);
  const userControlledExpansion = useRef(false);

  const progress = useMemo(() => computeSubagentProgress(run), [run.events, run.status]);
  const summary = progressSummary(progress);
  const presentation = useMemo(() => subagentResultPresentation(run), [run]);
  const { steps } = useMemo(() => reconstructSteps(run.events), [run.events]);
  const visibleSteps = useMemo(() => {
    const processSteps = withoutPromotedThinking(steps, presentation.promotedThinking);
    return processSteps.filter(
      (step) =>
        step.type === 'thinking' ||
        !isSubagentDispatchTool(String(step.toolStart?.name ?? step.toolResult?.name ?? 'tool'))
    );
  }, [presentation.promotedThinking, steps]);
  const children = useMemo(
    () => allRuns.filter((w) => w.parent === run.subagentRunId),
    [allRuns, run.subagentRunId]
  );

  useEffect(() => {
    if (previousStatus.current === 'running' && run.status !== 'running') {
      setActiveTab('result');
      if (!userControlledExpansion.current) setExpanded(false);
    }
    previousStatus.current = run.status;
  }, [run.status]);

  const toggleExpanded = () => {
    userControlledExpansion.current = true;
    setExpanded((value) => !value);
  };

  const statusIcon =
    run.status === 'running' ? (
      <Loader2 size={11} className="animate-spin" style={{ color: 'var(--color-info)' }} />
    ) : run.status === 'completed' ? (
      <CheckCircle2 size={11} style={{ color: 'var(--color-success)' }} />
    ) : run.status === 'failed' || run.status === 'timed_out' ? (
      <AlertCircle size={11} style={{ color: 'var(--color-error)' }} />
    ) : (
      <Circle size={11} style={{ color: 'var(--text-tertiary)' }} />
    );

  return (
    <div className="my-0.5 border-l border-[var(--border-primary)] pl-3">
      {/* Header (always visible): title + status + progress summary */}
      <div className="flex w-full items-center gap-1.5 text-[12px]">
        <button
          type="button"
          onClick={toggleExpanded}
          className="shrink-0 text-[var(--text-tertiary)] transition-colors hover:text-[var(--text-primary)]"
          aria-label={expanded ? '折叠 subagent' : '展开 subagent'}
        >
          {expanded ? <ChevronDown size={10} /> : <ChevronRight size={10} />}
        </button>
        <button
          type="button"
          onClick={toggleExpanded}
          className="flex min-w-0 flex-1 items-center gap-1.5 rounded-md px-1 py-0.5 text-left transition-colors hover:bg-[var(--bg-hover)]"
        >
          {statusIcon}
          <span className="shrink-0 font-medium text-[var(--text-primary)]">子智能体</span>
          <span className="shrink-0 text-[var(--text-secondary)]">
            {run.agent === 'explorer'
              ? 'Explore'
              : (run.agent || run.subagentRunId).replace(/^./, (value) => value.toUpperCase())}
          </span>
          {taskTitle ? (
            <span className="truncate font-medium text-[var(--text-primary)]">{taskTitle}</span>
          ) : null}
          {run.background ? (
            <span
              className="shrink-0 rounded px-1 text-[9px] font-medium uppercase tracking-wide"
              style={{ color: 'var(--text-tertiary)', border: '1px solid var(--border-primary)' }}
            >
              bg
            </span>
          ) : null}
          <span className="ml-auto shrink-0 text-[10px] text-[var(--text-tertiary)]">
            {statusLabel(progress.status)}
            {summary ? ` · ${summary}` : ''}
          </span>
        </button>
      </div>

      {expanded && (
        <div className="ml-4 mt-1.5 min-w-0">
          <div
            className="flex h-8 items-end gap-1 border-b border-[var(--border-primary)]"
            role="tablist"
            aria-label={`${run.agent || 'subagent'} 执行信息`}
          >
            {(
              [
                ['task', '提示词 / 任务'],
                ['process', '执行细节'],
                ['result', '结果'],
              ] as const
            ).map(([id, label]) => (
              <button
                key={id}
                type="button"
                role="tab"
                aria-selected={activeTab === id}
                onClick={() => setActiveTab(id)}
                className="h-8 border-b-2 px-2 text-[10px] font-medium transition-colors"
                style={{
                  borderColor: activeTab === id ? 'var(--accent)' : 'transparent',
                  color: activeTab === id ? 'var(--text-primary)' : 'var(--text-tertiary)',
                }}
              >
                {label}
              </button>
            ))}
          </div>

          <div className="min-w-0 py-2" role="tabpanel">
            {activeTab === 'task' &&
              (run.task ? (
                <MarkdownContent content={run.task} className="text-sm" maxHeight={320} />
              ) : (
                <div className="text-[11px] text-[var(--text-tertiary)]">暂无任务输入</div>
              ))}

            {activeTab === 'process' && (
              <div className="space-y-1.5">
                {visibleSteps.length === 0 && children.length === 0 && (
                  <div className="text-[11px] text-[var(--text-tertiary)]">暂无执行事件</div>
                )}
                {visibleSteps.map((step, i) => {
                  if (step.type === 'thinking') {
                    return (
                      <div key={i}>
                        <div className="mb-0.5 flex items-center gap-1">
                          <Brain size={9} className="text-[var(--color-purple)]" />
                          <span className="text-[9px] font-medium text-[var(--color-purple)]">
                            思考
                          </span>
                        </div>
                        <MarkdownContent content={step.content || ''} className="text-sm" />
                      </div>
                    );
                  }
                  const name = String(step.toolStart?.name ?? step.toolResult?.name ?? 'tool');
                  const args = step.toolStart?.args ?? {};
                  const resultStr = String(step.toolResult?.result ?? '');
                  const success = step.toolResult?.success !== false;
                  return (
                    <InlineToolCall
                      key={i}
                      toolCall={{
                        id: String(step.toolStart?.call_id ?? `subagent-tool-${i}`),
                        name,
                        args,
                        result: resultStr,
                        success,
                        status: step.toolResult ? (success ? 'succeeded' : 'failed') : 'running',
                        stdout: success ? resultStr : '',
                        stderr: success ? '' : resultStr,
                        log: '',
                        startedAt: Number(step.toolStart?.timestamp ?? Date.now()),
                        finishedAt: step.toolResult
                          ? Number(step.toolResult.timestamp ?? Date.now())
                          : undefined,
                      }}
                      index={i}
                    />
                  );
                })}
                {children.length > 0 && (
                  <div className="ml-2 border-l border-[var(--border-primary)] pl-2">
                    {children.map((child) => (
                      <SubagentStreamBlock
                        key={child.subagentRunId}
                        run={child}
                        allRuns={allRuns}
                      />
                    ))}
                  </div>
                )}
                {run.result &&
                  (run.result.touched_files.read.length > 0 ||
                    run.result.touched_files.written.length > 0) && (
                    <div className="space-y-1 border-t border-[var(--border-primary)] pt-2">
                      <div className="flex items-center gap-1 text-[9px] font-medium text-[var(--text-tertiary)]">
                        <Files size={9} />
                        文件访问
                      </div>
                      {run.result.touched_files.written.map((path) => (
                        <div key={`written-${path}`} className="break-all font-mono text-[9px]">
                          written · {path}
                        </div>
                      ))}
                      {run.result.touched_files.read.map((path) => (
                        <div key={`read-${path}`} className="break-all font-mono text-[9px]">
                          read · {path}
                        </div>
                      ))}
                    </div>
                  )}
              </div>
            )}

            {activeTab === 'result' &&
              (run.result || presentation.text ? (
                <SubagentResultView
                  result={run.result}
                  content={presentation.text}
                  maxHeight={400}
                />
              ) : (
                <div className="text-[11px] text-[var(--text-tertiary)]">
                  {run.status === 'running' ? '正在等待执行结果' : '暂无结果'}
                </div>
              ))}
          </div>
        </div>
      )}
    </div>
  );
});
