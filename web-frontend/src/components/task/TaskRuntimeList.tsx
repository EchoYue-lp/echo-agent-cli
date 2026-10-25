import { useMemo, useState, type ReactNode } from 'react';
import {
  ArrowRight,
  Check,
  ChevronLeft,
  ChevronRight,
  FileText,
  ListFilter,
  Plus,
  Search,
  X,
} from 'lucide-react';
import { Modal } from '../common/Modal';
import { useTaskRuntimeStore } from '../../stores/taskRuntimeStore';
import { useContextPaneStore } from '../../stores/contextPaneStore';
import type {
  PlanRevision,
  RecoveryBlocker,
  RuntimeArtifact,
  TaskRun,
  TodoItem,
} from '../../generated';
import type { SubagentRunState } from '../../stores/subagentRunStore';

const TASK_KIND_LABEL: Record<string, string> = {
  read_only_review: '审查',
  investigation: '调研',
  test_plan: '测试规划',
  implementation: '实现',
  debugging: '调试',
  review: '复核',
  summary: '总结',
  verification: '验证',
};
const PAGE_SIZE = 20;
const FILTERS = [
  ['all', '全部'],
  ['active', '待执行 / 执行中'],
  ['attention', '需要关注'],
  ['finished', '已结束'],
] as const;
type TaskFilter = (typeof FILTERS)[number][0];

export interface TaskActionReceipt {
  runId: string;
  taskId?: string;
  text: string;
  failed: boolean;
}

export type TaskAction = (
  label: string,
  operation: () => Promise<void>,
  taskId?: string
) => Promise<boolean>;

interface TaskRuntimeListProps {
  run: TaskRun;
  plan: PlanRevision | null;
  todos: TodoItem[];
  traceRuns: SubagentRunState[];
  artifacts: RuntimeArtifact[];
  recoveryBlockers: RecoveryBlocker[];
  pendingAction: string | null;
  actionReceipt: TaskActionReceipt | null;
  onAction: TaskAction;
  describeTodo: (todo: TodoItem) => string;
  renderStatus: (todo: TodoItem) => ReactNode;
}

function matchesFilter(todo: TodoItem, filter: TaskFilter) {
  if (filter === 'active') return ['pending', 'running'].includes(todo.status);
  if (filter === 'attention') return ['blocked', 'failed', 'timed_out'].includes(todo.status);
  if (filter === 'finished') return ['completed', 'skipped', 'cancelled'].includes(todo.status);
  return true;
}

export function taskTimestamp(value: string | null): string {
  if (!value || !Number.isFinite(Date.parse(value))) return '未记录';
  return new Intl.DateTimeFormat('zh-CN', {
    month: '2-digit',
    day: '2-digit',
    hour: '2-digit',
    minute: '2-digit',
    hour12: false,
  }).format(new Date(value));
}

export function TaskRuntimeList({
  run,
  plan,
  todos,
  traceRuns,
  artifacts,
  recoveryBlockers,
  pendingAction,
  actionReceipt,
  onAction,
  describeTodo,
  renderStatus,
}: TaskRuntimeListProps) {
  const [filter, setFilter] = useState<TaskFilter>('all');
  const [query, setQuery] = useState('');
  const [page, setPage] = useState(0);
  const [selectedTaskId, setSelectedTaskId] = useState<string | null>(null);
  const [adding, setAdding] = useState(false);
  const [newTitle, setNewTitle] = useState('');
  const filtered = useMemo(() => {
    const needle = query.trim().toLocaleLowerCase();
    return todos.filter(
      (todo) =>
        matchesFilter(todo, filter) &&
        (!needle ||
          `${todo.title} ${todo.task_id} ${todo.owner_agent ?? ''}`
            .toLocaleLowerCase()
            .includes(needle))
    );
  }, [todos, filter, query]);
  // Bound rendered cards without changing the backend query or inventing a second task store.
  const lastPage = Math.max(0, Math.ceil(filtered.length / PAGE_SIZE) - 1);
  const currentPage = Math.min(page, lastPage);
  const visible = filtered.slice(currentPage * PAGE_SIZE, (currentPage + 1) * PAGE_SIZE);
  const selected = todos.find((todo) => todo.task_id === selectedTaskId);
  const attentionCount = todos.filter((todo) => matchesFilter(todo, 'attention')).length;

  const addTask = async () => {
    const title = newTitle.trim();
    if (!title || !plan) return;
    const succeeded = await onAction('新增任务', () =>
      useTaskRuntimeStore.getState().insertTask(plan.tasks.at(-1)?.id ?? null, {
        id: `task_${Date.now()}`,
        title,
        description: `执行任务：${title}`,
        kind: 'implementation',
        agent_role: 'general-purpose',
        domain_profile: plan.domain_profile,
        // Preserve the displayed binding; the original revision service resolves the role.
        domain_binding: plan.domain_binding,
        domain_role: null,
        depends_on: [],
        parallel_group: null,
        execution_target: null,
        files: [],
        allowed_tools: [],
        required_artifacts: [],
        execution_checks: [],
        acceptance_criteria: [],
        max_retries: 3,
        sort_order: plan.tasks.reduce((max, task) => Math.max(max, task.sort_order), 0) + 10,
      })
    );
    if (succeeded) {
      setAdding(false);
      setNewTitle('');
    }
  };

  return (
    <section className="eko-task-list" aria-label="计划任务列表">
      <div className="eko-task-section-heading">
        <div>
          <h3>
            执行计划 <span>{todos.length}</span>
          </h3>
          <p>
            {plan ? `计划 r${plan.revision}` : '等待任务计划'}
            {attentionCount > 0 ? ` · ${attentionCount} 项需要关注` : ' · 以任务运行记录为准'}
          </p>
        </div>
        {plan && !['completed', 'cancelled'].includes(run.status) && (
          <button
            type="button"
            className="eko-task-icon-button"
            aria-label="新增计划任务"
            title="新增计划任务"
            onClick={() => setAdding(!adding)}
          >
            <Plus size={16} />
          </button>
        )}
      </div>
      {adding && (
        <form
          className="eko-task-add"
          onSubmit={(event) => {
            event.preventDefault();
            void addTask();
          }}
        >
          <label className="sr-only" htmlFor="new-plan-task-title">
            新任务标题
          </label>
          <input
            id="new-plan-task-title"
            autoFocus
            value={newTitle}
            onChange={(event) => setNewTitle(event.target.value)}
            placeholder="下一项要完成什么？"
            disabled={!!pendingAction}
          />
          <button
            type="submit"
            className="eko-task-icon-button"
            aria-label="保存新任务"
            disabled={!newTitle.trim() || !!pendingAction}
          >
            <Check size={16} />
          </button>
          <button
            type="button"
            className="eko-task-icon-button"
            aria-label="取消新增任务"
            onClick={() => setAdding(false)}
          >
            <X size={16} />
          </button>
        </form>
      )}
      <label className="eko-task-search">
        <Search size={15} aria-hidden="true" />
        <span className="sr-only">搜索计划任务</span>
        <input
          value={query}
          onChange={(event) => {
            setQuery(event.target.value);
            setPage(0);
          }}
          placeholder="搜索任务、编号或 Subagent"
        />
      </label>
      <div className="eko-task-filters" aria-label="任务状态筛选">
        {FILTERS.map(([value, label]) => (
          <button
            key={value}
            type="button"
            aria-pressed={filter === value}
            onClick={() => {
              setFilter(value);
              setPage(0);
            }}
          >
            {label}
          </button>
        ))}
      </div>
      <div className="eko-task-cards">
        {visible.map((todo, index) => {
          const task = plan?.tasks.find((item) => item.id === todo.task_id);
          const blocker = recoveryBlockers.find((item) => item.task_id === todo.task_id);
          return (
            <button
              type="button"
              key={todo.id}
              className="eko-task-card"
              onClick={() => setSelectedTaskId(todo.task_id)}
              aria-label={`查看任务：${todo.title}`}
            >
              <span className="eko-task-card-marker">{renderStatus(todo)}</span>
              <span className="eko-task-card-content">
                <span className="eko-task-card-kicker">
                  {String(currentPage * PAGE_SIZE + index + 1).padStart(2, '0')}
                  <span>{todo.owner_agent ?? task?.agent_role ?? '尚未分配'}</span>
                  {task && <span>{TASK_KIND_LABEL[task.kind] ?? task.kind}</span>}
                  {task && task.depends_on.length > 0 && (
                    <span>依赖 {task.depends_on.length} 项</span>
                  )}
                </span>
                <span className="eko-task-card-title">{todo.title}</span>
                <span className="eko-task-card-status" data-state={todo.status}>
                  {describeTodo(todo)}
                </span>
                {(blocker?.reason || todo.summary) && (
                  <span className="eko-task-card-summary">{blocker?.reason ?? todo.summary}</span>
                )}
              </span>
              <ArrowRight size={15} className="eko-task-card-arrow" aria-hidden="true" />
            </button>
          );
        })}
        {visible.length === 0 && (
          <div className="eko-task-empty">
            <ListFilter size={26} strokeWidth={1.4} />
            <strong>{todos.length ? '没有符合条件的任务' : '计划尚未展开'}</strong>
            <p>
              {todos.length
                ? '试试其它状态，或缩短搜索关键词。'
                : '任务创建后，这里会展示每一步的真实执行状态。'}
            </p>
            {todos.length > 0 && (
              <button
                type="button"
                className="eko-task-button"
                onClick={() => {
                  setQuery('');
                  setFilter('all');
                  setPage(0);
                }}
              >
                清除筛选
              </button>
            )}
          </div>
        )}
      </div>
      {filtered.length > PAGE_SIZE && (
        <nav className="eko-task-pagination" aria-label="任务列表分页">
          <span>
            {currentPage * PAGE_SIZE + 1}–{Math.min((currentPage + 1) * PAGE_SIZE, filtered.length)}{' '}
            / {filtered.length}
          </span>
          <div>
            <button
              type="button"
              className="eko-task-icon-button"
              aria-label="上一页任务"
              disabled={currentPage === 0}
              onClick={() => setPage(currentPage - 1)}
            >
              <ChevronLeft size={16} />
            </button>
            <button
              type="button"
              className="eko-task-icon-button"
              aria-label="下一页任务"
              disabled={currentPage === lastPage}
              onClick={() => setPage(currentPage + 1)}
            >
              <ChevronRight size={16} />
            </button>
          </div>
        </nav>
      )}
      {selected && (
        <TaskDetail
          key={`${run.run_id}:${selected.task_id}`}
          {...{
            run,
            plan,
            traceRuns,
            artifacts,
            recoveryBlockers,
            pendingAction,
            actionReceipt,
            onAction,
            describeTodo,
            renderStatus,
          }}
          todo={selected}
          onClose={() => setSelectedTaskId(null)}
        />
      )}
    </section>
  );
}

function TaskDetail({
  run,
  plan,
  todo,
  traceRuns,
  artifacts,
  recoveryBlockers,
  pendingAction,
  actionReceipt,
  onAction,
  describeTodo,
  renderStatus,
  onClose,
}: Omit<TaskRuntimeListProps, 'todos'> & { todo: TodoItem; onClose: () => void }) {
  const task = plan?.tasks.find((item) => item.id === todo.task_id);
  const execution = traceRuns
    .filter((item) => item.taskId === todo.task_id)
    .sort((a, b) => b.startedAt - a.startedAt)
    .at(0);
  const blocker = recoveryBlockers.find((item) => item.task_id === todo.task_id);
  const taskArtifacts = artifacts.filter((item) => item.task_id === todo.task_id);
  const [editing, setEditing] = useState(false);
  const [title, setTitle] = useState(todo.title);
  const [confirmSkip, setConfirmSkip] = useState(false);
  const canPatch = ['pending', 'blocked'].includes(todo.status);
  const canRetry =
    ['blocked', 'failed', 'timed_out'].includes(todo.status) &&
    ['paused', 'failed'].includes(run.status) &&
    todo.retry_count < todo.max_retries;
  const detailReceipt =
    actionReceipt?.runId === run.run_id && actionReceipt.taskId === todo.task_id
      ? actionReceipt
      : null;
  return (
    <Modal
      ariaLabel="计划任务详情"
      onClose={onClose}
      className="eko-task-detail"
      overlayClassName="!z-[80] p-3 sm:p-6"
    >
      <header className="eko-task-detail-header">
        <div className="eko-task-detail-eyebrow">
          任务详情{' '}
          <span>
            {renderStatus(todo)}
            {describeTodo(todo)}
          </span>
        </div>
        <button
          type="button"
          className="eko-task-icon-button"
          aria-label="关闭任务详情"
          onClick={onClose}
        >
          <X size={18} />
        </button>
      </header>
      <div className="eko-task-detail-body">
        <h2>{todo.title}</h2>
        {task?.description && <p className="eko-task-description">{task.description}</p>}
        <dl className="eko-task-facts">
          <div>
            <dt>负责人</dt>
            <dd>{todo.owner_agent ?? task?.agent_role ?? '尚未分配'}</dd>
          </div>
          <div>
            <dt>尝试次数</dt>
            <dd>
              {todo.retry_count + 1} / {todo.max_retries + 1}
            </dd>
          </div>
          <div>
            <dt>开始时间</dt>
            <dd>{taskTimestamp(todo.started_at)}</dd>
          </div>
          <div>
            <dt>结束时间</dt>
            <dd>{taskTimestamp(todo.completed_at)}</dd>
          </div>
        </dl>
        {(blocker || todo.summary || execution?.error) && (
          <section
            className={`eko-task-detail-note ${blocker || execution?.error ? 'is-warning' : ''}`}
          >
            <h3>{blocker ? '恢复前需要确认' : execution?.error ? '执行错误' : '任务回执'}</h3>
            <p>{blocker?.reason ?? execution?.error ?? todo.summary}</p>
            {blocker?.tool_name && <code>{blocker.tool_name}</code>}
          </section>
        )}
        {task && task.depends_on.length > 0 && (
          <section className="eko-task-detail-section">
            <h3>前置任务</h3>
            <ul>
              {task.depends_on.map((id) => (
                <li key={id}>{plan?.tasks.find((item) => item.id === id)?.title ?? id}</li>
              ))}
            </ul>
          </section>
        )}
        {task && task.acceptance_criteria.length > 0 && (
          <section className="eko-task-detail-section">
            <h3>验收标准</h3>
            <ul>
              {task.acceptance_criteria.map((criterion, index) => (
                <li key={`${index}:${criterion}`}>{criterion}</li>
              ))}
            </ul>
          </section>
        )}
        {task && task.files.length > 0 && (
          <section className="eko-task-detail-section">
            <h3>关联文件</h3>
            <ul className="eko-task-paths">
              {task.files.map((file) => (
                <li key={file}>
                  <FileText size={14} />
                  <code>{file}</code>
                </li>
              ))}
            </ul>
          </section>
        )}
        {taskArtifacts.length > 0 && (
          <section className="eko-task-detail-section">
            <h3>
              任务产物 <span>{taskArtifacts.length}</span>
            </h3>
            <ul className="eko-task-paths">
              {taskArtifacts.map((artifact) => (
                <li key={artifact.id}>
                  <FileText size={14} />
                  <span>
                    {artifact.title}
                    {artifact.path && <code>{artifact.path}</code>}
                  </span>
                </li>
              ))}
            </ul>
          </section>
        )}
        {execution && (
          <button
            type="button"
            className="eko-task-execution-link"
            onClick={() => {
              onClose();
              useContextPaneStore.getState().openSubagent(execution.runId, execution.subagentRunId);
            }}
          >
            <span>
              查看 Subagent 执行
              <small>
                {execution.agent} · {execution.subagentRunId}
              </small>
            </span>
            <ArrowRight size={17} />
          </button>
        )}
        <details className="eko-task-identities">
          <summary>运行身份</summary>
          <dl>
            <dt>TaskRun</dt>
            <dd>{run.run_id}</dd>
            <dt>PlanTask</dt>
            <dd>{todo.task_id}</dd>
            <dt>计划版本</dt>
            <dd>{plan ? `r${plan.revision}` : '未记录'}</dd>
            <dt>运行路由</dt>
            <dd>{run.route || '未记录'}</dd>
          </dl>
        </details>
        {editing && (
          <form
            className="eko-task-edit"
            onSubmit={(event) => {
              event.preventDefault();
              if (!canPatch || !title.trim()) return;
              void onAction(
                '更新任务标题',
                () =>
                  useTaskRuntimeStore.getState().updateTask(todo.task_id, { title: title.trim() }),
                todo.task_id
              ).then((succeeded) => {
                if (succeeded) setEditing(false);
              });
            }}
          >
            <label htmlFor="edit-plan-task-title">任务标题</label>
            <textarea
              id="edit-plan-task-title"
              rows={3}
              autoFocus
              value={title}
              onChange={(event) => setTitle(event.target.value)}
              disabled={!!pendingAction}
            />
            <div>
              <button type="button" className="eko-task-button" onClick={() => setEditing(false)}>
                返回
              </button>
              <button
                type="submit"
                className="eko-task-button is-primary"
                disabled={!canPatch || !title.trim() || !!pendingAction}
              >
                保存标题
              </button>
            </div>
          </form>
        )}
        {confirmSkip && (
          <section className="eko-task-detail-note is-warning">
            <h3>跳过这项任务？</h3>
            <p>这会向原任务图提交跳过请求。Goal 验收仍以原验收记录为准。</p>
            <div className="eko-task-inline-actions">
              <button
                type="button"
                className="eko-task-button"
                onClick={() => setConfirmSkip(false)}
              >
                保留任务
              </button>
              <button
                type="button"
                className="eko-task-button"
                disabled={!canPatch || !!pendingAction}
                onClick={() =>
                  void onAction(
                    '跳过任务',
                    () => useTaskRuntimeStore.getState().skipTask(todo.task_id),
                    todo.task_id
                  ).then((succeeded) => {
                    if (succeeded) setConfirmSkip(false);
                  })
                }
              >
                确认跳过
              </button>
            </div>
          </section>
        )}
      </div>
      {detailReceipt && (
        <div
          role={detailReceipt.failed ? 'alert' : 'status'}
          className={`eko-task-action-receipt ${detailReceipt.failed ? 'is-error' : ''}`}
        >
          {detailReceipt.text}
        </div>
      )}
      <footer className="eko-task-detail-footer">
        <span>{pendingAction ? '当前运行有操作请求处理中…' : '状态由任务运行记录更新'}</span>
        <div>
          {canRetry && (
            <button
              type="button"
              className="eko-task-button is-primary"
              disabled={!!pendingAction}
              onClick={() =>
                void onAction(
                  '重试任务',
                  () => useTaskRuntimeStore.getState().retryBlockedTask(todo.task_id),
                  todo.task_id
                )
              }
            >
              重试
            </button>
          )}
          {canPatch && (
            <>
              <button
                type="button"
                className="eko-task-button"
                disabled={!!pendingAction}
                onClick={() => {
                  setEditing(!editing);
                  setConfirmSkip(false);
                }}
              >
                编辑标题
              </button>
              <button
                type="button"
                className="eko-task-button"
                disabled={!!pendingAction}
                onClick={() => {
                  setConfirmSkip(!confirmSkip);
                  setEditing(false);
                }}
              >
                跳过
              </button>
            </>
          )}
        </div>
      </footer>
    </Modal>
  );
}
