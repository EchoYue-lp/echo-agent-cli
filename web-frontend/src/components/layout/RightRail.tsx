import { useEffect, useMemo, useState } from 'react';
import {
  Activity,
  CheckCircle2,
  Circle,
  Clock3,
  ShieldAlert,
  XCircle,
} from 'lucide-react';
import {
  tasksApi,
  type BackgroundTask,
} from '../../api/endpoints';
import { useChatStore } from '../../stores/chatStore';
import { useConversationStore } from '../../stores/conversationStore';
import { useChangesStore } from '../../stores/changesStore';
import { deriveChangedFiles } from '../../utils/deriveChangedFiles';
import { ChangesDrawer } from '../changes/ChangesDrawer';

const TERMINAL_TASK_STATUSES = new Set(['completed', 'failed', 'cancelled', 'timed_out']);

function statusMeta(status: string) {
  const normalized = status.toLowerCase();
  if (normalized.includes('complete'))
    return { icon: CheckCircle2, label: '已完成', color: 'var(--color-success)' };
  if (normalized.includes('fail'))
    return { icon: XCircle, label: '失败', color: 'var(--color-error)' };
  if (normalized.includes('cancel'))
    return { icon: XCircle, label: '已取消', color: 'var(--text-tertiary)' };
  if (normalized.includes('progress') || normalized.includes('running')) {
    return { icon: Activity, label: '进行中', color: 'var(--accent)' };
  }
  return { icon: Circle, label: '等待中', color: 'var(--text-tertiary)' };
}

function shortTime(value?: string) {
  if (!value) return '';
  const date = new Date(value);
  if (Number.isNaN(date.getTime())) return '';
  return date.toLocaleTimeString([], { hour: '2-digit', minute: '2-digit' });
}

export function RightRail() {
  const [tasks, setTasks] = useState<BackgroundTask[]>([]);
  const [taskError, setTaskError] = useState<string | null>(null);
  const messages = useChatStore((s) => s.messages);
  const isStreaming = useChatStore((s) => s.isStreaming);
  const approvalRequest = useChatStore((s) => s.approvalRequest);
  const inputRequest = useChatStore((s) => s.inputRequest);
  const selectionRequest = useChatStore((s) => s.selectionRequest);
  const pendingToolCalls = useChatStore((s) => s.pendingToolCalls);

  useEffect(() => {
    let cancelled = false;
    const load = async () => {
      try {
        const taskList = await tasksApi.list();
        if (!cancelled) {
          setTasks(taskList);
          setTaskError(null);
        }
      } catch (e) {
        if (!cancelled) setTaskError(e instanceof Error ? e.message : '无法读取任务');
      }
    };
    load();
    const timer = window.setInterval(load, 5000);
    return () => {
      cancelled = true;
      window.clearInterval(timer);
    };
  }, []);

  const activeTasks = useMemo(
    () => tasks.filter((task) => !TERMINAL_TASK_STATUSES.has(task.status)),
    [tasks]
  );
  const runningTasks = useMemo(
    () =>
      tasks.filter((task) =>
        ['in_progress', 'retrying', 'waiting_for_human', 'blocked'].includes(task.status)
      ),
    [tasks]
  );
  const foregroundHumanRequests =
    (approvalRequest ? 1 : 0) + (inputRequest ? 1 : 0) + (selectionRequest ? 1 : 0);
  const totalHumanRequests = foregroundHumanRequests;
  const isRuntimeBusy = isStreaming || activeTasks.length > 0;
  const displayedTasks = tasks.slice(0, 6);

  const activeId = useConversationStore((s) => s.activeId);
  const changesFiles = useChangesStore((s) => s.files);
  const setSelected = useChangesStore((s) => s.setSelected);

  // 会话切换检测:activeId 变化时清空改动列表
  useEffect(() => {
    useChangesStore.getState().checkSessionChange(activeId);
  }, [activeId]);

  // 从 messages 派生改动文件
  useEffect(() => {
    useChangesStore.getState().setFiles(deriveChangedFiles(messages));
  }, [messages]);

  const displayedChanges = changesFiles.slice(0, 12);

  return (
    <aside className="hidden h-full w-[300px] shrink-0 border-l border-[var(--border-primary)] bg-[var(--bg-rail)] px-4 py-5 xl:block">
      <div className="flex h-full flex-col gap-5">
        <section>
          <div className="mb-3 flex items-center justify-between">
            <h2 className="text-sm font-semibold text-[var(--text-primary)]">进度</h2>
            <span className="text-xs text-[var(--text-tertiary)]">
              {activeTasks.length ? `${activeTasks.length} 活动` : tasks.length || '实时'}
            </span>
          </div>
          <div className="space-y-2">
            {tasks.length === 0 && (
              <div className="rounded-lg border border-[var(--border-primary)] px-3 py-3 text-xs text-[var(--text-tertiary)]">
                {taskError ? '任务服务暂不可用' : isStreaming ? '当前会话正在执行' : '暂无后台任务'}
              </div>
            )}
            {displayedTasks.map((task) => {
              const meta = statusMeta(task.status);
              const Icon = meta.icon;
              const progress = Math.max(0, Math.min(100, task.progress_pct ?? task.progress ?? 0));
              return (
                <div
                  key={task.id}
                  className="rounded-lg border border-[var(--border-primary)] bg-[var(--bg-secondary)] px-3 py-2.5"
                >
                  <div className="flex items-start gap-2">
                    <Icon size={15} className="mt-0.5 shrink-0" style={{ color: meta.color }} />
                    <div className="min-w-0 flex-1">
                      <div className="truncate text-xs font-medium text-[var(--text-primary)]">
                        {task.description}
                      </div>
                      <div className="mt-1 flex items-center gap-2 text-[11px] text-[var(--text-tertiary)]">
                        <span>{meta.label}</span>
                        {task.progress_phase && <span>{task.progress_phase}</span>}
                        <span>{shortTime(task.updated_at)}</span>
                      </div>
                      {task.progress_message && (
                        <div className="mt-1 truncate text-[11px] text-[var(--text-tertiary)]">
                          {task.progress_message}
                        </div>
                      )}
                      {progress > 0 && (
                        <div className="mt-2 h-1 overflow-hidden rounded-full bg-[var(--border-secondary)]">
                          <div
                            className="h-full rounded-full bg-[var(--accent)]"
                            style={{ width: `${progress}%` }}
                          />
                        </div>
                      )}
                    </div>
                  </div>
                </div>
              );
            })}
            {tasks.length > displayedTasks.length && (
              <div className="px-2 text-[11px] text-[var(--text-tertiary)]">
                另有 {tasks.length - displayedTasks.length} 个任务
              </div>
            )}
          </div>
        </section>

        <section>
          <div className="mb-3 flex items-center justify-between">
            <h2 className="text-sm font-semibold text-[var(--text-primary)]">输出</h2>
            <span className="text-xs text-[var(--text-tertiary)]">
              {changesFiles.length ? `${changesFiles.length} 改动` : ''}
            </span>
          </div>
          <div className="space-y-1">
            {displayedChanges.length === 0 ? (
              <div className="rounded-lg border border-dashed border-[var(--border-primary)] px-3 py-3 text-xs text-[var(--text-tertiary)]">
                本会话暂无文件改动
              </div>
            ) : (
              displayedChanges.map((file) => {
                const statusMeta =
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
                        background: `color-mix(in srgb, ${statusMeta.color} 18%, transparent)`,
                        color: statusMeta.color,
                      }}
                    >
                      {statusMeta.label}
                    </span>
                    <span className="min-w-0 flex-1 truncate text-xs text-[var(--text-secondary)]">
                      <span className="text-[var(--text-primary)]">{file.basename}</span>
                      {file.dir && (
                        <span className="text-[var(--text-tertiary)]"> · {file.dir}</span>
                      )}
                    </span>
                    {file.toolCount > 1 && (
                      <span className="shrink-0 text-[10px] text-[var(--text-tertiary)]">
                        ×{file.toolCount}
                      </span>
                    )}
                  </button>
                );
              })
            )}
            {changesFiles.length > displayedChanges.length && (
              <div className="px-2 text-[11px] text-[var(--text-tertiary)]">
                另有 {changesFiles.length - displayedChanges.length} 个改动
              </div>
            )}
          </div>
        </section>

        <section className="mt-auto space-y-2 border-t border-[var(--border-primary)] pt-4">
          <div className="flex items-center gap-2 text-xs text-[var(--text-secondary)]">
            <Clock3
              size={14}
              className={isRuntimeBusy ? 'text-[var(--accent)]' : 'text-[var(--text-tertiary)]'}
            />
            <span>
              {isRuntimeBusy
                ? `${isStreaming ? '前台执行中' : '前台空闲'} · ${activeTasks.length} 个后台活动`
                : '全局空闲'}
            </span>
          </div>
          <div className="flex items-center gap-2 text-xs text-[var(--text-secondary)]">
            <Activity
              size={14}
              className={
                pendingToolCalls.length || runningTasks.length
                  ? 'text-[var(--accent)]'
                  : 'text-[var(--text-tertiary)]'
              }
            />
            <span>
              {pendingToolCalls.length || runningTasks.length
                ? `前台工具 ${pendingToolCalls.length} · 后台执行 ${runningTasks.length}`
                : '无前台工具调用 / 无后台执行任务'}
            </span>
          </div>
          <div className="flex items-center gap-2 text-xs text-[var(--text-secondary)]">
            <ShieldAlert
              size={14}
              className={
                totalHumanRequests ? 'text-[var(--color-warning)]' : 'text-[var(--text-tertiary)]'
              }
            />
            <span>
              {totalHumanRequests ? `等待人工处理 ${totalHumanRequests} 个` : '无人工处理请求'}
            </span>
          </div>
        </section>
      </div>
      <ChangesDrawer />
    </aside>
  );
}
