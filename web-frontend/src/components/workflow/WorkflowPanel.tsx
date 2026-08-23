import { useCallback, useEffect, useState } from 'react';
import { AlertCircle, CheckCircle2, GitBranch, Play, Plus, Trash2 } from 'lucide-react';
import { workflowApi } from '../../api/endpoints';
import type { StoredWorkflow, WorkflowExecution } from '../../generated';

export function WorkflowPanel() {
  const [workflows, setWorkflows] = useState<StoredWorkflow[]>([]);
  const [definition, setDefinition] = useState('');
  const [name, setName] = useState('');
  const [busy, setBusy] = useState<string | null>('load');
  const [error, setError] = useState<string | null>(null);
  const [notice, setNotice] = useState<string | null>(null);
  const [execution, setExecution] = useState<WorkflowExecution | null>(null);

  const refresh = useCallback(async () => {
    try {
      setError(null);
      setWorkflows(await workflowApi.list());
    } catch (cause) {
      setError(errorMessage(cause, '加载工作流失败'));
    } finally {
      setBusy((value) => (value === 'load' ? null : value));
    }
  }, []);

  useEffect(() => {
    void refresh();
  }, [refresh]);

  const create = async () => {
    if (!definition.trim() || busy) return;
    setBusy('create');
    setError(null);
    setNotice(null);
    try {
      const created = await workflowApi.create(definition, name.trim() || undefined);
      setDefinition('');
      setName('');
      setNotice(`已创建 ${created.name}`);
      await refresh();
    } catch (cause) {
      setError(errorMessage(cause, '创建工作流失败'));
    } finally {
      setBusy(null);
    }
  };

  const execute = async (workflow: StoredWorkflow) => {
    if (busy) return;
    setBusy(`run:${workflow.id}`);
    setError(null);
    setNotice(null);
    setExecution(null);
    try {
      const result = await workflowApi.execute(workflow.id);
      setExecution(result);
      setNotice(`${workflow.name} 已完成 ${result.steps} 个步骤`);
    } catch (cause) {
      setError(errorMessage(cause, `执行 ${workflow.name} 失败`));
    } finally {
      setBusy(null);
    }
  };

  const remove = async (workflow: StoredWorkflow) => {
    if (busy || !window.confirm(`删除工作流“${workflow.name}”？`)) return;
    setBusy(`delete:${workflow.id}`);
    setError(null);
    setNotice(null);
    try {
      await workflowApi.delete(workflow.id);
      setNotice(`已删除 ${workflow.name}`);
      if (execution?.workflow_id === workflow.id) setExecution(null);
      await refresh();
    } catch (cause) {
      setError(errorMessage(cause, `删除 ${workflow.name} 失败`));
    } finally {
      setBusy(null);
    }
  };

  return (
    <div className="space-y-3 p-3">
      <div className="flex items-center justify-between">
        <h3 className="text-sm font-semibold text-[var(--text-primary)]">
          工作流 ({workflows.length})
        </h3>
        {busy === 'load' && <span className="text-xs text-[var(--text-tertiary)]">加载中...</span>}
      </div>

      <div className="space-y-2 border-b border-[var(--border-secondary)] pb-3">
        <input
          value={name}
          onChange={(event) => setName(event.target.value)}
          className="input w-full"
          placeholder="工作流名称（可从定义读取）"
        />
        <textarea
          value={definition}
          onChange={(event) => setDefinition(event.target.value)}
          className="input w-full font-mono text-xs"
          rows={7}
          placeholder="YAML 或 JSON 定义"
        />
        <button
          type="button"
          onClick={() => void create()}
          disabled={!definition.trim() || busy !== null}
          className="btn btn-primary flex w-full items-center justify-center gap-1.5 py-1.5 text-sm disabled:cursor-not-allowed disabled:opacity-50"
        >
          <Plus size={14} /> {busy === 'create' ? '创建中...' : '创建'}
        </button>
      </div>

      {(error || notice) && (
        <div
          className="flex items-start gap-2 border-l-[3px] px-3 py-2 text-xs"
          style={{
            borderColor: error ? 'var(--color-error)' : 'var(--color-success)',
            background: error ? 'var(--color-error-bg)' : 'var(--color-success-bg)',
            color: error ? 'var(--color-error)' : 'var(--color-success)',
          }}
          role={error ? 'alert' : 'status'}
        >
          {error ? <AlertCircle size={14} /> : <CheckCircle2 size={14} />}
          <span>{error ?? notice}</span>
        </div>
      )}

      {workflows.length === 0 && busy !== 'load' ? (
        <p className="py-4 text-center text-xs text-[var(--text-tertiary)]">暂无工作流</p>
      ) : (
        workflows.map((workflow) => (
          <div key={workflow.id} className="card px-3 py-2">
            <div className="flex items-center gap-2">
              <GitBranch size={13} className="shrink-0 text-[var(--accent)]" />
              <div className="min-w-0 flex-1">
                <p className="truncate text-xs font-medium text-[var(--text-primary)]">
                  {workflow.name}
                </p>
                <p className="text-[10px] text-[var(--text-tertiary)]">
                  {workflow.node_count} 节点 · {workflow.edge_count} 连线
                </p>
              </div>
              <button
                type="button"
                onClick={() => void execute(workflow)}
                disabled={busy !== null}
                className="flex h-7 w-7 items-center justify-center rounded-md text-[var(--action-run)] hover:bg-[var(--action-run-bg)] disabled:opacity-40"
                title="执行"
                aria-label={`执行 ${workflow.name}`}
              >
                <Play size={13} />
              </button>
              <button
                type="button"
                onClick={() => void remove(workflow)}
                disabled={busy !== null}
                className="flex h-7 w-7 items-center justify-center rounded-md text-[var(--text-tertiary)] hover:bg-[var(--bg-hover)] hover:text-[var(--color-error)] disabled:opacity-40"
                title="删除"
                aria-label={`删除 ${workflow.name}`}
              >
                <Trash2 size={13} />
              </button>
            </div>
          </div>
        ))
      )}

      {execution && (
        <div>
          <p className="mb-1 text-[10px] font-medium uppercase text-[var(--text-tertiary)]">
            最近执行 · {execution.path.join(' -> ')}
          </p>
          <pre className="max-h-56 overflow-auto bg-[var(--bg-code)] p-3 text-[11px] text-[var(--color-code-text)]">
            {JSON.stringify(execution.state, null, 2)}
          </pre>
        </div>
      )}
    </div>
  );
}

function errorMessage(cause: unknown, fallback: string): string {
  return cause instanceof Error && cause.message ? cause.message : fallback;
}
