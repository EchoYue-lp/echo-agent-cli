import { useEffect, useMemo } from 'react';
import { AlertCircle, ListTodo, RefreshCw } from 'lucide-react';
import { useTaskRuntimeStore, taskRuntimePresentation } from '../../stores/taskRuntimeStore';
import { useWorkspaceStore } from '../../stores/workspaceStore';
import { useConversationStore } from '../../stores/conversationStore';
import { productDataScope } from '../../lib/productDataScope';
import { TaskRuntimePanel } from '../task/TaskRuntimePanel';
import { taskTimestamp } from '../task/TaskRuntimeList';
import type { TaskRun } from '../../generated';
import { taskRunSourceLabel } from '../../lib/taskRunPresentation';

const STATUS: Record<TaskRun['status'], string> = {
  pending: '待执行',
  running: '执行中',
  paused: '已暂停',
  cancelled: '已取消',
  failed: '失败',
  completed: '已完成',
};

export function RightRail() {
  const workspace = useWorkspaceStore((state) => state.current);
  const conversationId = useConversationStore((state) => state.activeId);
  const scope = useMemo(() => productDataScope(workspace), [workspace]);
  const state = useTaskRuntimeStore();
  const { activeRun, error } = taskRuntimePresentation(state);
  const {
    selectedRunId,
    historyError,
    runPage,
    loadingRun,
    loadingRuns,
    runListError,
    nextRunContinuation,
    runStatuses,
    bindScope,
    loadRuns,
    loadByRun,
    followConversation,
  } = state;

  useEffect(() => {
    bindScope(scope);
    void loadRuns(scope);
    if (conversationId && !useTaskRuntimeStore.getState().activeRun) {
      void useTaskRuntimeStore.getState().loadByConversation(scope.workspaceId, conversationId);
    }
  }, [scope, conversationId, bindScope, loadRuns]);

  return (
    // 让绝对定位的读屏标签也归属内部滚动区域，避免撑出整个三栏布局的滚动范围。
    <div className="relative h-full min-h-0 overflow-y-auto">
      <section className="eko-task-history" aria-label="正式任务运行历史">
        <header>
          <div>
            <h2>运行历史</h2>
            <p>当前工作空间的正式任务</p>
          </div>
          <button
            type="button"
            className="eko-task-icon-button"
            aria-label="刷新运行列表"
            disabled={loadingRuns}
            onClick={() => void loadRuns(scope, runStatuses)}
          >
            <RefreshCw size={15} />
          </button>
        </header>
        <label className="eko-task-history-filter">
          运行状态
          <select
            aria-label="筛选运行状态"
            disabled={loadingRuns}
            value={runStatuses.at(0) ?? ''}
            onChange={(event) =>
              void loadRuns(
                scope,
                event.target.value ? [event.target.value as TaskRun['status']] : []
              )
            }
          >
            <option value="">全部状态</option>
            {Object.entries(STATUS).map(([status, label]) => (
              <option key={status} value={status}>
                {label}
              </option>
            ))}
          </select>
        </label>
        {runListError && (
          <p role="alert" className="eko-task-history-error">
            {runListError} · 已显示内容保留，可刷新重试。
          </p>
        )}
        {loadingRuns && <p role="status">正在读取运行列表…</p>}
        {!loadingRuns && !runPage.length && !runListError && (
          <p>尚无任务运行。IM、定时任务和对话的运行记录都会显示在这里。</p>
        )}
        <div className="eko-task-history-rows">
          {runPage.map((run) => (
            <button
              type="button"
              key={run.run_id}
              className="eko-task-history-row"
              aria-current={selectedRunId === run.run_id ? 'true' : undefined}
              onClick={() => void loadByRun(scope, run.run_id)}
            >
              <strong>{run.goal}</strong>
              <span>
                {STATUS[run.status]} · {taskRunSourceLabel(run)} · {taskTimestamp(run.updated_at)}
              </span>
            </button>
          ))}
        </div>
        <footer>
          <span>本页 {runPage.length} 项 · 实时查询</span>
          {nextRunContinuation && (
            <button
              type="button"
              className="eko-task-button"
              disabled={loadingRuns}
              onClick={() => void loadRuns(scope, runStatuses, true)}
            >
              下一页运行
            </button>
          )}
          {conversationId && (
            <button
              type="button"
              className="eko-task-button"
              onClick={() => void followConversation(scope.workspaceId, conversationId)}
            >
              当前会话运行
            </button>
          )}
        </footer>
      </section>
      {historyError && (
        <p role="alert" className="eko-task-history-notice">
          {historyError} · 已显示内容保留。
        </p>
      )}
      {loadingRun ? (
        <p className="eko-task-history-notice" role="status">
          正在读取所选运行…
        </p>
      ) : activeRun ? (
        <TaskRuntimePanel
          key={`${scope.workspaceId}:${scope.workspaceGeneration}:${activeRun.run_id}`}
        />
      ) : (
        <div className="flex min-h-40 flex-col items-center justify-center gap-2 px-8 py-8 text-center text-[var(--text-tertiary)]">
          {error ? (
            <AlertCircle size={28} strokeWidth={1.4} />
          ) : (
            <ListTodo size={28} strokeWidth={1.4} />
          )}
          <div className="text-sm font-medium text-[var(--text-secondary)]">
            {error ? '暂时无法读取任务' : '选择一项任务运行'}
          </div>
          <div className="max-w-[260px] text-xs leading-relaxed">
            {error ?? '选择已有运行查看计划、执行回执与产物；新建任务会话仍使用侧栏原入口。'}
          </div>
        </div>
      )}
    </div>
  );
}
