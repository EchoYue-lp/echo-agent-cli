import { useState, useEffect } from 'react';

interface PlanTask {
  id: string;
  title: string;
  description?: string;
  status?: string;
}

interface PlanEditorProps {
  /** Initial task list */
  initialTasks: PlanTask[];
  /** Save callback, receives the new JSON string */
  onSave: (tasksJson: string) => Promise<void> | void;
  onClose: () => void;
}

export function PlanEditor({ initialTasks, onSave, onClose }: PlanEditorProps) {
  const [tasks, setTasks] = useState<PlanTask[]>(initialTasks);
  const [rawJson, setRawJson] = useState(() => JSON.stringify(initialTasks, null, 2));
  const [error, setError] = useState<string | null>(null);
  const [mode, setMode] = useState<'form' | 'json'>('form');

  // Sync form edits to rawJson
  useEffect(() => {
    setRawJson(JSON.stringify(tasks, null, 2));
  }, [tasks]);

  const applyJson = () => {
    try {
      const parsed = JSON.parse(rawJson);
      if (!Array.isArray(parsed)) throw new Error('计划必须是任务数组');
      setTasks(parsed);
      setError(null);
    } catch (e) {
      setError(e instanceof Error ? e.message : 'JSON 解析失败');
    }
  };

  const updateTask = (id: string, patch: Partial<PlanTask>) => {
    setTasks((ts) => ts.map((t) => (t.id === id ? { ...t, ...patch } : t)));
  };

  const handleSave = async () => {
    await onSave(JSON.stringify(tasks));
    onClose();
  };

  return (
    <div
      className="fixed inset-0 z-50 flex items-center justify-center bg-black/40"
      onClick={onClose}
    >
      <div
        className="w-[720px] max-h-[80vh] rounded-lg flex flex-col"
        style={{ background: 'var(--bg-primary)', border: '1px solid var(--border-primary)' }}
        onClick={(e) => e.stopPropagation()}
      >
        <header
          className="flex items-center justify-between px-4 py-3 border-b"
          style={{ borderColor: 'var(--border-primary)' }}
        >
          <h2 className="font-medium" style={{ color: 'var(--text-primary)' }}>
            编辑任务计划
          </h2>
          <div className="flex gap-2 items-center">
            <button
              className="text-xs"
              style={{
                color: mode === 'form' ? 'var(--text-primary)' : 'var(--text-tertiary)',
                fontWeight: mode === 'form' ? 600 : 400,
              }}
              onClick={() => setMode('form')}
            >
              表单
            </button>
            <button
              className="text-xs"
              style={{
                color: mode === 'json' ? 'var(--text-primary)' : 'var(--text-tertiary)',
                fontWeight: mode === 'json' ? 600 : 400,
              }}
              onClick={() => setMode('json')}
            >
              JSON
            </button>
            <button
              onClick={onClose}
              className="text-xs ml-2"
              style={{ color: 'var(--text-tertiary)' }}
            >
              ✕
            </button>
          </div>
        </header>
        <div className="flex-1 overflow-y-auto p-4">
          {mode === 'form' ? (
            <div className="space-y-3">
              {tasks.map((t) => (
                <div
                  key={t.id}
                  className="space-y-1 rounded-md p-2"
                  style={{ border: '1px solid var(--border-secondary)' }}
                >
                  <input
                    className="w-full bg-transparent text-sm font-medium"
                    style={{ color: 'var(--text-primary)' }}
                    value={t.title}
                    onChange={(e) => updateTask(t.id, { title: e.target.value })}
                    placeholder="任务标题"
                  />
                  <textarea
                    className="w-full bg-transparent text-xs"
                    style={{ color: 'var(--text-secondary)' }}
                    rows={2}
                    value={t.description ?? ''}
                    onChange={(e) => updateTask(t.id, { description: e.target.value })}
                    placeholder="任务描述（可选）"
                  />
                </div>
              ))}
            </div>
          ) : (
            <div className="space-y-2">
              <textarea
                className="w-full font-mono text-xs p-2 rounded-md min-h-[300px]"
                style={{
                  color: 'var(--text-primary)',
                  background: 'var(--bg-secondary)',
                  border: '1px solid var(--border-secondary)',
                }}
                value={rawJson}
                onChange={(e) => setRawJson(e.target.value)}
              />
              <button
                onClick={applyJson}
                className="text-xs px-2 py-1 rounded-md"
                style={{
                  background: 'var(--bg-hover)',
                  color: 'var(--text-secondary)',
                  border: '1px solid var(--border-secondary)',
                }}
              >
                应用 JSON
              </button>
              {error && (
                <div className="text-xs" style={{ color: 'var(--color-error)' }}>
                  {error}
                </div>
              )}
            </div>
          )}
        </div>
        <footer
          className="flex justify-end gap-2 px-4 py-3 border-t"
          style={{ borderColor: 'var(--border-primary)' }}
        >
          <button
            onClick={onClose}
            className="px-3 py-1 text-xs rounded-md"
            style={{
              background: 'var(--bg-hover)',
              color: 'var(--text-secondary)',
              border: '1px solid var(--border-secondary)',
            }}
          >
            取消
          </button>
          <button
            onClick={handleSave}
            className="px-3 py-1 text-xs rounded-md font-medium"
            style={{ background: 'var(--accent)', color: 'var(--text-on-accent)' }}
          >
            保存
          </button>
        </footer>
      </div>
    </div>
  );
}
