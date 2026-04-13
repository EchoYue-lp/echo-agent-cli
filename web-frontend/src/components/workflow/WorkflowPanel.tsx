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
        <h3 className="text-sm font-semibold text-gray-700">Workflows ({workflows.length})</h3>
      </div>

      {/* Create form */}
      <div className="space-y-2 rounded border bg-gray-50 p-3">
        <input
          value={name}
          onChange={(e) => setName(e.target.value)}
          className="w-full rounded border px-2 py-1 text-sm"
          placeholder="Workflow name"
        />
        <textarea
          value={yaml}
          onChange={(e) => setYaml(e.target.value)}
          className="w-full rounded border px-2 py-1 text-xs font-mono"
          rows={5}
          placeholder="YAML definition..."
        />
        <button onClick={create} className="w-full rounded bg-indigo-600 py-1.5 text-sm text-white hover:bg-indigo-700">
          <Plus size={14} className="inline mr-1" /> Create
        </button>
      </div>

      {/* List */}
      {workflows.map((wf) => (
        <div key={wf.id} className="rounded border border-gray-200 bg-white px-3 py-2">
          <div className="flex items-center gap-2">
            <GitBranch size={12} className="text-blue-500" />
            <span className="text-xs font-medium">{wf.name || wf.id}</span>
            <span className="rounded bg-gray-100 px-1.5 py-0.5 text-[10px] text-gray-500">{wf.status}</span>
            <div className="ml-auto flex gap-1">
              <button onClick={() => execute(wf.id)} className="rounded p-1 hover:bg-green-50" title="Execute">
                <Play size={12} className="text-green-600" />
              </button>
              <button onClick={() => remove(wf.id)} className="rounded p-1 hover:bg-red-50" title="Delete">
                <Trash2 size={12} className="text-gray-400 hover:text-red-500" />
              </button>
            </div>
          </div>
        </div>
      ))}
    </div>
  );
}
