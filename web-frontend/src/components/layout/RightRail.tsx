import { useEffect, useMemo, useState } from 'react';
import type { ReactNode } from 'react';
import { Bot, ChevronLeft, ChevronRight, FileText, ListTodo, RefreshCw } from 'lucide-react';
import { useConversationStore } from '../../stores/conversationStore';
import { useChangesStore } from '../../stores/changesStore';
import { useTaskRuntimeStore } from '../../stores/taskRuntimeStore';
import { useSubagentRunStore } from '../../stores/subagentRunStore';
import { deriveChangedFiles } from '../../utils/deriveChangedFiles';
import { useChatStore } from '../../stores/chatStore';
import { ChangesDrawer } from '../changes/ChangesDrawer';
import { TaskRuntimePanel } from '../task/TaskRuntimePanel';
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

function Section({
  icon,
  title,
  count,
  children,
}: {
  icon: ReactNode;
  title: string;
  count?: string;
  children: ReactNode;
}) {
  return (
    <section className="border-t border-[var(--border-primary)] pt-4">
      <div className="mb-2 flex items-center gap-2">
        <span className="text-[var(--text-tertiary)]">{icon}</span>
        <h2 className="min-w-0 flex-1 truncate text-xs font-semibold uppercase text-[var(--text-secondary)]">
          {title}
        </h2>
        {count && (
          <span className="text-[10px] tabular-nums text-[var(--text-tertiary)]">{count}</span>
        )}
      </div>
      {children}
    </section>
  );
}

function RailMetric({ icon, value }: { icon: ReactNode; value: number }) {
  return (
    <span className="flex flex-col items-center gap-1">
      {icon}
      <span className="text-[10px] font-medium tabular-nums text-[var(--text-secondary)]">
        {value}
      </span>
    </span>
  );
}

export function RightRail() {
  const [collapsed, setCollapsed] = useState(false);
  const activeId = useConversationStore((s) => s.activeId);
  const messages = useChatStore((s) => s.messages);
  const { activeRun, todos, artifacts, refresh } = useTaskRuntimeStore();
  const subagentRuns = useSubagentRunStore((s) => s.runs);

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

  const visibleRuns = useMemo(() => {
    return Object.values(subagentRuns)
      .filter(
        (w) =>
          !activeRun ||
          w.runId === activeRun.run_id ||
          w.conversationId === activeRun.conversation_id
      )
      .sort((a, b) => a.startedAt - b.startedAt);
  }, [activeRun, subagentRuns]);

  const displayedChanges = changesFiles.slice(0, 12);
  const runningSubagents = visibleRuns.filter((run) => run.status === 'running').length;
  const completedTodos = todos.filter((todo) => todo.status === 'completed').length;
  const outputCount = changesFiles.length + artifacts.length;
  const hasActivity = Boolean(
    activeRun || visibleRuns.length > 0 || changesFiles.length > 0 || artifacts.length > 0
  );

  return (
    <aside
      className={`relative hidden h-full shrink-0 overflow-visible lg:block ${
        collapsed ? 'w-0' : 'w-[316px] border-l border-[var(--border-primary)] bg-[var(--bg-rail)]'
      }`}
    >
      {collapsed && (
        <div className="absolute right-3 top-4 z-30">
          <button
            onClick={() => setCollapsed(false)}
            className="flex flex-col items-center gap-2 rounded-full border border-[var(--border-primary)] bg-[var(--bg-primary)]/95 px-2 py-2 text-[var(--text-tertiary)] shadow-[var(--shadow-md)] backdrop-blur transition-colors hover:border-[var(--accent)] hover:text-[var(--text-primary)]"
            title="展开工作台"
            aria-label="展开工作台"
          >
            <ChevronLeft size={13} />
            <span
              className="h-1.5 w-1.5 rounded-full"
              style={{
                background: activeRun ? runStatusColor(activeRun.status) : 'var(--text-tertiary)',
              }}
            />
            <RailMetric icon={<ListTodo size={12} />} value={todos.length} />
            <RailMetric icon={<Bot size={12} />} value={visibleRuns.length} />
            <RailMetric icon={<FileText size={12} />} value={outputCount} />
          </button>
        </div>
      )}

      {!collapsed && (
        <div className="flex h-full flex-col overflow-y-auto px-4 py-4">
          <header className="pb-4">
            <div className="mb-3 flex items-center justify-between gap-3">
              <div className="min-w-0">
                <h2 className="truncate text-sm font-semibold text-[var(--text-primary)]">
                  工作台
                </h2>
                <p className="mt-0.5 truncate text-[11px] text-[var(--text-tertiary)]">
                  当前会话的任务、子代理和输出
                </p>
              </div>
              <button
                onClick={() => setCollapsed(true)}
                className="flex h-7 w-7 shrink-0 items-center justify-center rounded-md text-[var(--text-tertiary)] transition-colors hover:bg-[var(--bg-hover)] hover:text-[var(--text-primary)]"
                title="收起工作台"
                aria-label="收起工作台"
              >
                <ChevronRight size={14} />
              </button>
              {activeRun && (
                <button
                  onClick={() => refresh(activeRun.run_id)}
                  className="flex h-7 w-7 shrink-0 items-center justify-center rounded-md text-[var(--text-tertiary)] transition-colors hover:bg-[var(--bg-hover)] hover:text-[var(--text-primary)]"
                  title="刷新任务状态"
                >
                  <RefreshCw size={13} />
                </button>
              )}
            </div>

            {activeRun ? (
              <div className="space-y-2">
                <div className="flex items-center gap-2">
                  <span
                    className="shrink-0 rounded-md px-1.5 py-0.5 text-[10px] font-medium"
                    style={{
                      color: runStatusColor(activeRun.status),
                      background: 'var(--bg-hover)',
                    }}
                  >
                    {STATUS_LABEL[activeRun.status] ?? activeRun.status}
                  </span>
                  <span
                    className="min-w-0 flex-1 truncate text-xs text-[var(--text-secondary)]"
                    title={activeRun.goal}
                  >
                    {activeRun.goal}
                  </span>
                </div>
                <div className="grid grid-cols-3 gap-2">
                  <div className="rounded-md bg-[var(--bg-secondary)] px-2 py-1.5">
                    <div className="text-[10px] text-[var(--text-tertiary)]">任务</div>
                    <div className="text-xs font-medium tabular-nums text-[var(--text-primary)]">
                      {completedTodos}/{todos.length}
                    </div>
                  </div>
                  <div className="rounded-md bg-[var(--bg-secondary)] px-2 py-1.5">
                    <div className="text-[10px] text-[var(--text-tertiary)]">子代理</div>
                    <div className="text-xs font-medium tabular-nums text-[var(--text-primary)]">
                      {runningSubagents}/{visibleRuns.length}
                    </div>
                  </div>
                  <div className="rounded-md bg-[var(--bg-secondary)] px-2 py-1.5">
                    <div className="text-[10px] text-[var(--text-tertiary)]">输出</div>
                    <div className="text-xs font-medium tabular-nums text-[var(--text-primary)]">
                      {outputCount}
                    </div>
                  </div>
                </div>
              </div>
            ) : (
              <p className="rounded-md border border-dashed border-[var(--border-primary)] px-3 py-2 text-xs text-[var(--text-tertiary)]">
                {hasActivity ? '当前没有正在执行的任务' : '开始对话后，这里会显示任务状态和输出。'}
              </p>
            )}
          </header>

          <div className="space-y-4">
            <TaskRuntimePanel />

            {visibleRuns.length > 0 && (
              <Section
                icon={<Bot size={13} />}
                title="子代理"
                count={
                  runningSubagents > 0 ? `${runningSubagents} 运行中` : `${visibleRuns.length}`
                }
              >
                <SubagentPanel
                  subagents={Object.fromEntries(visibleRuns.map((run) => [run.subagentRunId, run]))}
                />
              </Section>
            )}

            <Section
              icon={<FileText size={13} />}
              title="输出"
              count={
                changesFiles.length || artifacts.length
                  ? `${changesFiles.length} 改动 / ${artifacts.length} 产物`
                  : undefined
              }
            >
              <div className="space-y-1">
                {displayedChanges.length === 0 && artifacts.length === 0 ? (
                  <p className="rounded-md border border-dashed border-[var(--border-primary)] px-3 py-2 text-xs text-[var(--text-tertiary)]">
                    暂无文件改动或产物
                  </p>
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
                          className="inline-flex h-4 w-4 shrink-0 items-center justify-center rounded-md text-[9px] font-bold"
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
            </Section>
          </div>
        </div>
      )}

      <ChangesDrawer />
    </aside>
  );
}
