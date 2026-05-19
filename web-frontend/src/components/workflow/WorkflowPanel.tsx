import { useEffect, useState } from 'react';
import { workflowApi } from '../../api/endpoints';
import type { WorkflowInfo } from '../../types/api';
import { GitBranch, Play, Plus, Trash2 } from 'lucide-react';

export function WorkflowPanel() {
  const [workflows, setWorkflows] = useState<WorkflowInfo[]>([]);
  const [yaml, setYaml] = useState('');
  const [name, setName] = useState('');

  useEffect(() => {
    workflowApi.list().then(setWorkflows).catch(console.error);
  }, []);

  const create = async () => {
    try {
      await workflowApi.create(yaml, name || undefined);
      setYaml('');
      setName('');
      workflowApi.list().then(setWorkflows);
    } catch (e) {
      console.error(e);
    }
  };

  const execute = async (id: string) => {
    try {
      await workflowApi.execute(id);
    } catch (e) {
      console.error(e);
    }
  };

  const remove = async (id: string) => {
    try {
      await workflowApi.delete(id);
      workflowApi.list().then(setWorkflows);
    } catch (e) {
      console.error(e);
    }
  };

  return (
    <div className="p-3 space-y-3">
      <div className="flex items-center justify-between">
        <h3 className="text-sm font-semibold text-[var(--text-primary)]">
          工作流 ({workflows.length})
        </h3>
      </div>

      {/* Create form */}
      <div className="space-y-2 rounded-lg border border-[var(--border-primary)] bg-[var(--bg-hover)] p-3">
        <input
          value={name}
          onChange={(e) => setName(e.target.value)}
          className="input w-full"
          placeholder="工作流名称"
        />
        <textarea
          value={yaml}
          onChange={(e) => setYaml(e.target.value)}
          className="input w-full font-mono text-xs"
          rows={5}
          placeholder="YAML 定义..."
        />
        <button
          onClick={create}
          className="btn btn-primary w-full justify-center py-1.5 text-sm"
        >
          <Plus size={14} /> 创建
        </button>
      </div>

      {/* List */}
      {workflows.map((wf) => (
        <div key={wf.id} className="card px-3 py-2">
          <div className="flex items-center gap-2">
            <GitBranch size={12} style={{ color: 'var(--text-link)' }} />
            <span className="text-xs font-medium text-[var(--text-primary)]">{wf.name || wf.id}</span>
            <span className="badge text-[10px]">{wf.status}</span>
            <div className="ml-auto flex gap-1">
              <button
                onClick={() => execute(wf.id)}
                className="rounded p-1 transition-colors hover:bg-[var(--bg-hover)]"
                title="执行"
              >
                <Play size={12} style={{ color: 'var(--accent)' }} />
              </button>
              <button
                onClick={() => remove(wf.id)}
                className="rounded p-1 transition-colors hover:bg-[var(--bg-hover)]"
                title="删除"
              >
                <Trash2 size={12} className="text-[var(--text-tertiary)] hover:text-red-500" />
              </button>
            </div>
          </div>
        </div>
      ))}
    </div>
  );
}
