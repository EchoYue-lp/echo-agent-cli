import { useEffect, useState, useCallback, useRef } from 'react';
import {
  tasksApi,
  type BackgroundTask,
  type SubmitTaskRequest,
} from '../../api/endpoints';
import {
  Play,
  XCircle,
  RefreshCw,
  Plus,
  ChevronDown,
  ChevronUp,
  HandMetal,
} from 'lucide-react';

const STATUS_COLORS: Record<string, string> = {
  pending: 'var(--color-warning)',
  in_progress: 'var(--color-info)',
  completed: 'var(--color-success)',
  failed: 'var(--color-error)',
  cancelled: 'var(--text-tertiary)',
  blocked: 'var(--color-warning)',
  timed_out: 'var(--color-error)',
  retrying: 'var(--color-info)',
  waiting_for_human: 'var(--color-purple, #a855f7)',
};

const STATUS_LABELS: Record<string, string> = {
  pending: '等待中',
  in_progress: '运行中',
  completed: '已完成',
  failed: '失败',
  cancelled: '已取消',
  blocked: '已阻塞',
  timed_out: '超时',
  retrying: '重试中',
  waiting_for_human: '等待审批',
};

const KIND_LABELS: Record<string, string> = {
  'bg:kind:agent_chat': '对话',
  'bg:kind:cron': '定时',
  'bg:kind:workflow': '工作流',
  'bg:kind:research': '研究',
};

// Extended task with progress/phase data (from SSE)
interface TaskProgress {
  percentage?: number;
  phase?: string;
  phases?: string[];
  currentPhaseIdx?: number;
  eta?: string;
}

export function TasksPanel() {
  const [tasks, setTasks] = useState<BackgroundTask[]>([]);
  const [loading, setLoading] = useState(true);
  const [expandedId, setExpandedId] = useState<string | null>(null);
  const [showSubmit, setShowSubmit] = useState(false);
  const [submitKind, setSubmitKind] = useState('agent_chat');
  const [submitDesc, setSubmitDesc] = useState('');
  const [submitParams, setSubmitParams] = useState('');

  // Progress data keyed by task ID
  const [progressMap, setProgressMap] = useState<Record<string, TaskProgress>>({});

  // SSE event sources keyed by task ID
  const eventSourcesRef = useRef<Record<string, EventSource>>({});

  const fetchTasks = useCallback(async () => {
    try {
      const data = await tasksApi.list();
      setTasks(data);
    } catch (e) {
      console.error('Failed to fetch tasks:', e);
    } finally {
      setLoading(false);
    }
  }, []);

  useEffect(() => {
    fetchTasks();
    const interval = setInterval(() => {
      fetchTasks();
    }, 5000);
    return () => clearInterval(interval);
  }, [fetchTasks]);

  // Subscribe to SSE for active tasks
  useEffect(() => {
    const activeTasks = tasks.filter(
      (t) => !['completed', 'failed', 'cancelled', 'timed_out'].includes(t.status)
    );

    const currentIds = new Set(activeTasks.map((t) => t.id));
    const existingIds = new Set(Object.keys(eventSourcesRef.current));

    // Close SSE for completed tasks
    existingIds.forEach((id) => {
      if (!currentIds.has(id)) {
        eventSourcesRef.current[id]?.close();
        delete eventSourcesRef.current[id];
      }
    });

    // Open SSE for new active tasks
    activeTasks.forEach((task) => {
      if (eventSourcesRef.current[task.id]) return;

      const protocol = window.location.protocol === 'https:' ? 'https:' : 'http:';
      const url = `${protocol}//${window.location.host}/api/tasks/${task.id}/events`;
      try {
        const es = new EventSource(url);
        eventSourcesRef.current[task.id] = es;

        es.addEventListener('progress', (e) => {
          try {
            const data = JSON.parse(e.data) as TaskProgress;
            setProgressMap((prev) => ({ ...prev, [task.id]: data }));
          } catch {
            /* ignore */
          }
        });

        es.addEventListener('phase', (e) => {
          try {
            const data = JSON.parse(e.data) as {
              phase: string;
              phases?: string[];
              current_phase_idx?: number;
            };
            setProgressMap((prev) => ({
              ...prev,
              [task.id]: {
                ...prev[task.id],
                phase: data.phase,
                phases: data.phases,
                currentPhaseIdx: data.current_phase_idx,
              },
            }));
          } catch {
            /* ignore */
          }
        });

        es.addEventListener('done', () => {
          es.close();
          delete eventSourcesRef.current[task.id];
          fetchTasks();
        });

        es.addEventListener('error', () => {
          // EventSource will auto-reconnect, but if the task is done it won't
          // We'll let the polling handle the final state
        });

        es.onerror = () => {
          es.close();
          delete eventSourcesRef.current[task.id];
        };
      } catch {
        // EventSource not supported or URL invalid
      }
    });

    return () => {
      // Cleanup on unmount
      Object.values(eventSourcesRef.current).forEach((es) => es.close());
    };
  }, [tasks, fetchTasks]);

  const handleSubmit = async () => {
    if (!submitDesc.trim()) return;
    try {
      let params: Record<string, unknown> = {};
      if (submitParams.trim()) {
        try {
          params = JSON.parse(submitParams);
        } catch {
          params = { prompt: submitParams };
        }
      }
      const req: SubmitTaskRequest = {
        kind: submitKind,
        description: submitDesc,
        params: submitKind === 'agent_chat' ? { prompt: submitDesc, ...params } : params,
      };
      await tasksApi.submit(req);
      setSubmitDesc('');
      setSubmitParams('');
      setShowSubmit(false);
      fetchTasks();
    } catch (e) {
      console.error('Failed to submit task:', e);
    }
  };

  const handleCancel = async (id: string) => {
    try {
      await tasksApi.cancel(id);
      fetchTasks();
    } catch (e) {
      console.error('Failed to cancel task:', e);
    }
  };

  const s = {
    text: 'var(--text-primary)',
    textSec: 'var(--text-secondary)',
    textTer: 'var(--text-tertiary)',
    border: 'var(--border-primary)',
    bg: 'var(--bg-primary)',
    bgHover: 'var(--bg-hover)',
    bgCard: 'var(--bg-secondary)',
  };

  if (loading) {
    return (
      <div className="p-3">
        <p className="text-xs" style={{ color: s.textTer }}>
          加载中...
        </p>
      </div>
    );
  }

  return (
    <div className="p-3 space-y-3">
      <div className="flex items-center justify-between">
        <h3 className="text-sm font-semibold" style={{ color: s.text }}>
          后台任务 ({tasks.length})
        </h3>
        <div className="flex gap-1">
          <button
            onClick={() => {
              fetchTasks();
            }}
            className="rounded p-1 transition-colors"
            style={{ color: s.textTer }}
          >
            <RefreshCw size={14} />
          </button>
          <button
            onClick={() => setShowSubmit(!showSubmit)}
            className="rounded p-1 transition-colors"
            style={{ color: s.textTer }}
          >
            <Plus size={14} />
          </button>
        </div>
      </div>

      {showSubmit && (
        <div
          className="rounded-lg border p-3 space-y-2"
          style={{ borderColor: s.border, background: s.bgCard }}
        >
          <select
            value={submitKind}
            onChange={(e) => setSubmitKind(e.target.value)}
            className="w-full rounded border px-2 py-1 text-xs"
            style={{ borderColor: s.border, background: s.bg, color: s.text }}
          >
            <option value="agent_chat">对话任务</option>
            <option value="research">研究任务</option>
            <option value="cron">定时任务</option>
            <option value="workflow">工作流</option>
          </select>
          <input
            type="text"
            value={submitDesc}
            onChange={(e) => setSubmitDesc(e.target.value)}
            placeholder="任务描述 / 研究主题 / 对话内容"
            className="w-full rounded border px-2 py-1 text-xs"
            style={{ borderColor: s.border, background: s.bg, color: s.text }}
            onKeyDown={(e) => e.key === 'Enter' && handleSubmit()}
          />
          {submitKind !== 'agent_chat' && (
            <textarea
              value={submitParams}
              onChange={(e) => setSubmitParams(e.target.value)}
              placeholder='参数 (JSON): {"topic": "...", "max_papers": 20}'
              className="w-full rounded border px-2 py-1 text-xs"
              rows={2}
              style={{ borderColor: s.border, background: s.bg, color: s.text }}
            />
          )}
          <button
            onClick={handleSubmit}
            className="flex items-center gap-1 rounded px-2 py-1 text-xs font-medium transition-colors"
            style={{ background: 'var(--color-primary)', color: '#fff' }}
          >
            <Play size={12} /> 提交任务
          </button>
        </div>
      )}

      {tasks.length === 0 ? (
        <p className="text-xs py-4 text-center" style={{ color: s.textTer }}>
          暂无后台任务
        </p>
      ) : (
        <div className="space-y-2">
          {tasks.map((task) => {
            const progress = progressMap[task.id];
            const isExpanded = expandedId === task.id;

            return (
              <div
                key={task.id}
                className="rounded-lg border px-3 py-2 transition-colors"
                style={{ borderColor: s.border, background: s.bg }}
              >
                <div
                  className="flex items-center gap-2 cursor-pointer"
                  onClick={() => setExpandedId(isExpanded ? null : task.id)}
                >
                  <span
                    className="inline-block w-2 h-2 rounded-full flex-shrink-0"
                    style={{ background: STATUS_COLORS[task.status] || s.textTer }}
                  />
                  <span className="text-xs font-medium truncate flex-1" style={{ color: s.text }}>
                    {task.description}
                  </span>
                  {/* Inline progress percentage */}
                  {progress?.percentage != null && task.status === 'in_progress' && (
                    <span
                      className="text-[10px] font-mono flex-shrink-0"
                      style={{ color: 'var(--color-info)' }}
                    >
                      {progress.percentage}%
                    </span>
                  )}
                  {task.kind && (
                    <span
                      className="text-[10px] px-1.5 py-0.5 rounded flex-shrink-0"
                      style={{ background: s.bgHover, color: s.textSec }}
                    >
                      {KIND_LABELS[task.kind] || task.kind}
                    </span>
                  )}
                  {/* Human gate indicator */}
                  {task.status === 'waiting_for_human' && (
                    <HandMetal
                      size={12}
                      className="flex-shrink-0"
                      style={{ color: 'var(--color-purple, #a855f7)' }}
                    />
                  )}
                  <span className="text-[10px] flex-shrink-0" style={{ color: s.textTer }}>
                    {STATUS_LABELS[task.status] || task.status}
                  </span>
                  {isExpanded ? (
                    <ChevronUp size={12} style={{ color: s.textTer }} />
                  ) : (
                    <ChevronDown size={12} style={{ color: s.textTer }} />
                  )}
                </div>

                {/* Progress bar (inline, always visible when progress exists) */}
                {progress?.percentage != null && task.status === 'in_progress' && (
                  <div className="mt-1.5">
                    <div
                      className="h-1 rounded-full overflow-hidden"
                      style={{ background: s.bgHover }}
                    >
                      <div
                        className="h-full rounded-full transition-all duration-300"
                        style={{
                          width: `${Math.min(100, progress.percentage)}%`,
                          background: 'var(--color-info)',
                        }}
                      />
                    </div>
                    <div
                      className="flex items-center justify-between mt-0.5 text-[10px]"
                      style={{ color: s.textTer }}
                    >
                      {progress.phase && <span>{progress.phase}</span>}
                      {progress.eta && <span>ETA: {progress.eta}</span>}
                    </div>
                  </div>
                )}

                {/* Phase visualization */}
                {progress?.phases && progress.phases.length > 1 && isExpanded && (
                  <div className="mt-2">
                    <div className="flex items-center gap-0.5">
                      {progress.phases.map((phase, idx) => {
                        const isCurrent = idx === progress.currentPhaseIdx;
                        const isDone =
                          progress.currentPhaseIdx != null && idx < progress.currentPhaseIdx;
                        return (
                          <div key={idx} className="flex-1 relative">
                            <div
                              className="h-1.5 rounded-full transition-all duration-300"
                              style={{
                                background: isDone
                                  ? 'var(--color-success)'
                                  : isCurrent
                                    ? 'var(--color-info)'
                                    : s.bgHover,
                                opacity: isDone || isCurrent ? 1 : 0.5,
                              }}
                            />
                            <span
                              className="mt-0.5 block text-[9px] truncate text-center"
                              style={{
                                color: isCurrent ? 'var(--color-info)' : s.textTer,
                                fontWeight: isCurrent ? 600 : 400,
                              }}
                            >
                              {phase}
                            </span>
                          </div>
                        );
                      })}
                    </div>
                  </div>
                )}

                {isExpanded && (
                  <div
                    className="mt-2 pt-2 space-y-2"
                    style={{ borderTop: `1px solid ${s.border}` }}
                  >
                    <div className="text-[10px] space-y-1" style={{ color: s.textSec }}>
                      <p>ID: {task.id}</p>
                      <p>创建: {new Date(task.created_at).toLocaleString()}</p>
                      <p>更新: {new Date(task.updated_at).toLocaleString()}</p>
                      {task.progress != null && <p>进度: {task.progress}%</p>}
                    </div>
                    {task.result && (
                      <div
                        className="rounded p-2 text-xs"
                        style={{ background: s.bgCard, color: s.text }}
                      >
                        <p className="font-medium mb-1">结果:</p>
                        <pre
                          className="whitespace-pre-wrap text-[10px]"
                          style={{ color: s.textSec }}
                        >
                          {task.result.slice(0, 2000)}
                          {task.result.length > 2000 && '...'}
                        </pre>
                      </div>
                    )}
                    {task.error && (
                      <div
                        className="rounded p-2 text-xs"
                        style={{ background: 'rgba(239,68,68,0.1)', color: 'var(--color-error)' }}
                      >
                        <p className="font-medium mb-1">错误:</p>
                        <pre className="whitespace-pre-wrap text-[10px]">{task.error}</pre>
                      </div>
                    )}
                    {!['completed', 'failed', 'cancelled', 'timed_out'].includes(task.status) && (
                      <button
                        onClick={() => handleCancel(task.id)}
                        className="flex items-center gap-1 rounded px-2 py-1 text-xs transition-colors"
                        style={{ background: 'rgba(239,68,68,0.1)', color: 'var(--color-error)' }}
                      >
                        <XCircle size={12} /> 取消任务
                      </button>
                    )}
                  </div>
                )}
              </div>
            );
          })}
        </div>
      )}
    </div>
  );
}
