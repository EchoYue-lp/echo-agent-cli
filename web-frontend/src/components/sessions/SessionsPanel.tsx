import { useEffect, useState } from 'react';
import { Save, RotateCcw, Plus, Clock } from 'lucide-react';
import { sessionApi } from '../../api/endpoints';
import type { SnapshotInfo } from '../../types/api';

export function SessionsPanel() {
  const [snapshots, setSnapshots] = useState<SnapshotInfo[]>([]);
  const [loading, setLoading] = useState(false);
  const [msg, setMsg] = useState<string | null>(null);

  const load = async () => {
    setLoading(true);
    try {
      const data = await sessionApi.listCheckpoints();
      setSnapshots(data);
    } catch (e) {
      console.error(e);
    }
    setLoading(false);
  };

  useEffect(() => { load(); }, []);

  const create = async () => {
    setLoading(true);
    setMsg(null);
    try {
      const res = await sessionApi.createCheckpoint();
      setMsg(res.success ? `检查点已创建：${res.snapshot_id?.slice(0, 8)}...` : '失败');
      await load();
    } catch (e: unknown) {
      setMsg(`错误：${e instanceof Error ? e.message : '未知'}`);
    }
    setLoading(false);
  };

  const restore = async (id: string) => {
    setLoading(true);
    setMsg(null);
    try {
      const res = await sessionApi.restoreCheckpoint(id);
      setMsg(res.success ? `已恢复到 ${res.restored_to?.slice(0, 8)}...` : '恢复失败');
    } catch (e: unknown) {
      setMsg(`错误：${e instanceof Error ? e.message : '未知'}`);
    }
    setLoading(false);
  };

  const formatTime = (ts: number) => {
    const d = new Date(ts * 1000);
    return d.toLocaleString();
  };

  return (
    <div className="p-3 space-y-3">
      <div className="flex items-center justify-between">
        <h3 className="text-sm font-semibold" style={{ color: 'var(--text-primary)' }}>
          会话检查点 ({snapshots.length})
        </h3>
        <button
          onClick={create}
          disabled={loading}
          className="flex items-center gap-1 rounded-lg px-2.5 py-1 text-xs font-medium transition-colors"
          style={{ background: 'var(--accent)', color: 'white' }}
        >
          <Plus size={12} />
          创建
        </button>
      </div>

      {msg && (
        <div className="rounded-lg px-3 py-2 text-xs" style={{
          background: 'var(--accent-bg)',
          color: 'var(--accent)',
        }}>
          {msg}
        </div>
      )}

      {loading && snapshots.length === 0 && (
        <div className="py-8 text-center text-xs" style={{ color: 'var(--text-tertiary)' }}>
          <div className="spinner mx-auto mb-2" />
          加载中...
        </div>
      )}

      {snapshots.length === 0 && !loading && (
        <div className="py-8 text-center text-xs" style={{ color: 'var(--text-tertiary)' }}>
          <Save size={24} className="mx-auto mb-2" />
          暂无检查点
          <br />
          点击"创建"保存当前会话状态
        </div>
      )}

      {snapshots.map((s) => (
        <div
          key={s.id}
          className="rounded-lg border p-3 transition-colors"
          style={{
            borderColor: 'var(--border-primary)',
            background: 'var(--bg-primary)',
          }}
        >
          <div className="flex items-center justify-between">
            <div className="min-w-0 flex-1">
              <div className="flex items-center gap-2">
                <Save size={12} style={{ color: 'var(--accent)' }} />
                <span className="truncate font-mono text-xs font-medium" style={{ color: 'var(--text-primary)' }}>
                  {s.id.slice(0, 12)}...
                </span>
              </div>
              <div className="mt-1 flex items-center gap-3 text-[11px]" style={{ color: 'var(--text-tertiary)' }}>
                <span className="flex items-center gap-1">
                  <Clock size={10} />
                  {formatTime(s.created_at)}
                </span>
                <span>迭代：{s.iteration}</span>
              </div>
            </div>
            <button
              onClick={() => restore(s.id)}
              disabled={loading}
              className="flex items-center gap-1 rounded-md px-2 py-1 text-[11px] font-medium transition-colors"
              style={{ border: '1px solid var(--border-primary)', color: 'var(--text-secondary)' }}
              onMouseEnter={(e) => { e.currentTarget.style.borderColor = 'var(--accent)'; e.currentTarget.style.color = 'var(--accent)'; }}
              onMouseLeave={(e) => { e.currentTarget.style.borderColor = 'var(--border-primary)'; e.currentTarget.style.color = 'var(--text-secondary)'; }}
            >
              <RotateCcw size={10} />
              恢复
            </button>
          </div>
        </div>
      ))}
    </div>
  );
}
