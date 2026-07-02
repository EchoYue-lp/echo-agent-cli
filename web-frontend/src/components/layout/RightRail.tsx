import { useEffect, useMemo } from 'react';
import { RefreshCw, ListTodo, FileText, Gauge } from 'lucide-react';
import { useConversationStore } from '../../stores/conversationStore';
import { useChangesStore } from '../../stores/changesStore';
import { useTaskRuntimeStore } from '../../stores/taskRuntimeStore';
import { useWorkerTraceStore } from '../../stores/workerTraceStore';
import { useSubagentStore } from '../../stores/subagentStore';
import { deriveChangedFiles } from '../../utils/deriveChangedFiles';
import { useChatStore } from '../../stores/chatStore';
import { ChangesDrawer } from '../changes/ChangesDrawer';
import { TaskRuntimePanel, CacheUsageCard, cacheUsageForWorkers } from '../task/TaskRuntimePanel';
import { SubagentPanel } from '../subagent/SubagentCard';

const STATUS_LABEL: Record<string, string> = {
  pending: '待处理',
  running: '执行中',
  paused: '已暂停',
  cancelled: '已取消',
  failed: '失败',
  completed: '已完成',
};

function runStatusColor(status: string): string {
  if (['completed'].includes(status)) return 'var(--color-success)';
  if (['running'].includes(status)) return 'var(--color-info)';
  if (['failed', 'cancelled'].includes(status)) return 'var(--color-error)';
  if (['paused', 'blocked'].includes(status)) return 'var(--color-warning)';
  return 'var(--text-tertiary)';
}

export function RightRail() {
  const activeId = useConversationStore((s) => s.activeId);
  const messages = useChatStore((s) => s.messages);
  const { activeRun, todos: _todos, artifacts, refresh } = useTaskRuntimeStore();
  const traceWorkers = useWorkerTraceStore((s) => s.workers);
  const subagents = useSubagentStore((s) => s.subagents);

  const changesFiles = useChangesStore((s) => s.files);
  const setSelected = useChangesStore((s) => s.setSelected);

  // Session change detection
  useEffect(() => {
    useChangesStore.getState().checkSessionChange(activeId);
  }, [activeId]);

  // Derive changed files from messages on tool-call fingerprint
  const toolCallCount = useMemo(() => {
    let n = 0;
    for (const m of messages) n += (m.toolCalls ?? []).length;
    return n;
  }, [messages]);
  useEffect(() => {
    useChangesStore.getState().setFiles(deriveChangedFiles(messages));
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [toolCallCount]);

  const visibleWorkers = useMemo(() => {
    return Object.values(traceWorkers)
      .filter((w) => !activeRun || w.runId === activeRun.run_id)
      .sort((a, b) => (a.startedAt ?? '').localeCompare(b.startedAt ?? ''));
  }, [activeRun, traceWorkers]);

  const displayedChanges = changesFiles.slice(0, 12);
  const usageSummary = cacheUsageForWorkers(visibleWorkers);

  return (
    <aside className="hidden h-full w-[300px] shrink-0 border-l border-[var(--border-primary)] bg-[var(--bg-rail)] px-4 py-5 xl:block">
      <div className="flex h-full flex-col gap-5 overflow-y-auto">
        {/* ── 任务运行(TaskRuntimePanel: todos + cache) ── */}
        <TaskRuntimePanel />

        {Object.keys(subagents).length > 0 && (
          <section>
            <SubagentPanel subagents={subagents} />
          </section>
        )}

        {/* ── 任务执行进度(审批等) ── */}
        <section>
          <div className="mb-3 flex items-center justify-between">
            <div className="flex items-center gap-1.5">
              <ListTodo size={13} style={{ color: 'var(--accent)' }} />
              <h2 className="text-sm font-semibold text-[var(--text-primary)]">任务执行进度</h2>
            </div>
            {activeRun && (
              <button
                onClick={() => refresh(activeRun.run_id)}
                className="text-[var(--text-tertiary)]"
              >
                <RefreshCw size={11} />
              </button>
            )}
          </div>
          {activeRun ? (
            <>
              <div
                className="mb-2 truncate rounded-md px-2 py-1.5 text-[11px]"
                style={{ background: 'var(--bg-secondary)', color: 'var(--text-secondary)' }}
                title={activeRun.goal}
              >
                {activeRun.goal}
              </div>
              <div className="mb-2">
                <span
                  className="rounded px-1.5 py-0.5 text-[10px] font-medium"
                  style={{ color: runStatusColor(activeRun.status), background: 'var(--bg-hover)' }}
                >
                  {STATUS_LABEL[activeRun.status] ?? activeRun.status}
                </span>
              </div>
            </>
          ) : (
            <div className="rounded-lg border border-dashed border-[var(--border-primary)] px-3 py-3 text-xs text-[var(--text-tertiary)]">
              暂无运行中的任务
            </div>
          )}
        </section>

        {/* ── 输出产物 ── */}
        <section>
          <div className="mb-3 flex items-center gap-1.5">
            <FileText size={13} style={{ color: 'var(--accent)' }} />
            <h2 className="text-sm font-semibold text-[var(--text-primary)]">输出产物</h2>
            <span className="ml-auto text-xs text-[var(--text-tertiary)]">
              {changesFiles.length || artifacts.length
                ? `${changesFiles.length} 改动 / ${artifacts.length} 产物`
                : ''}
            </span>
          </div>
          <div className="space-y-1">
            {displayedChanges.length === 0 && artifacts.length === 0 ? (
              <div className="rounded-lg border border-dashed border-[var(--border-primary)] px-3 py-3 text-xs text-[var(--text-tertiary)]">
                本会话暂无输出产物
              </div>
            ) : (
              displayedChanges.map((file) => {
                const meta =
                  file.status === 'added'
                    ? { label: 'A', color: 'var(--color-success, #22c55e)' }
                    : file.status === 'deleted'
                      ? { label: 'D', color: 'var(--color-error, #ef4444)' }
                      : { label: 'M', color: 'var(--color-warning, #f59e0b)' };
                return (
                  <button
                    key={file.path}
                    onClick={() => setSelected(file.path)}
                    className="flex w-full items-center gap-2 rounded-md px-2 py-1.5 text-left transition-colors hover:bg-[var(--bg-hover)]"
                    title={file.path}
                  >
                    <span
                      className="inline-flex h-4 w-4 shrink-0 items-center justify-center rounded text-[9px] font-bold"
                      style={{
                        background: `color-mix(in srgb, ${meta.color} 18%, transparent)`,
                        color: meta.color,
                      }}
                    >
                      {meta.label}
                    </span>
                    <span className="min-w-0 flex-1 truncate text-xs text-[var(--text-secondary)]">
                      <span className="text-[var(--text-primary)]">{file.basename}</span>
                      {file.dir && (
                        <span className="text-[var(--text-tertiary)]"> · {file.dir}</span>
                      )}
                    </span>
                  </button>
                );
              })
            )}
            {/* Artifacts */}
            {artifacts.length > 0 && (
              <div className="mt-2 space-y-0.5">
                {artifacts.map((a) => (
                  <div
                    key={a.id}
                    className="flex items-center gap-1 truncate px-1 py-0.5 text-[10px] text-[var(--text-secondary)]"
                    title={a.path ?? a.title}
                  >
                    <FileText size={10} className="text-[var(--text-tertiary)]" />
                    <span className="truncate">{a.title}</span>
                  </div>
                ))}
              </div>
            )}
          </div>
        </section>

        {/* ── Token / Cache ── */}
        <section>
          <div className="mb-3 flex items-center gap-1.5">
            <Gauge size={13} style={{ color: 'var(--accent)' }} />
            <h2 className="text-sm font-semibold text-[var(--text-primary)]">Token / Cache</h2>
          </div>
          {usageSummary.calls > 0 ? (
            <CacheUsageCard summary={usageSummary} compact />
          ) : (
            <div className="rounded-lg border border-dashed border-[var(--border-primary)] px-3 py-3 text-xs text-[var(--text-tertiary)]">
              暂无 LLM 调用数据
            </div>
          )}
        </section>
      </div>
      <ChangesDrawer />
    </aside>
  );
}
