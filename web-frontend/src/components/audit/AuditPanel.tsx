import { useEffect, useState } from 'react';
import { auditApi } from '../../api/endpoints';
import type { AuditLog } from '../../types/api';
import { ShieldCheck, Trash2 } from 'lucide-react';
import { StatusBadge } from '../common/StatusBadge';

export function AuditPanel() {
  const [logs, setLogs] = useState<AuditLog[]>([]);
  const [stats, setStats] = useState<{ total: number; allowed: number; denied: number; asked: number } | null>(null);

  useEffect(() => {
    auditApi.logs().then((res) => setLogs(res.logs)).catch(console.error);
    auditApi.stats().then(setStats).catch(console.error);
  }, []);

  const clear = async () => {
    try {
      await auditApi.clear();
      setLogs([]);
      setStats(null);
    } catch (e) { console.error(e); }
  };

  const s = {
    text: 'var(--text-primary)',
    textSec: 'var(--text-secondary)',
    textTer: 'var(--text-tertiary)',
    border: 'var(--border-primary)',
    bg: 'var(--bg-primary)',
    bgHover: 'var(--bg-hover)',
  };

  return (
    <div className="p-3 space-y-3">
      <div className="flex items-center justify-between">
        <h3 className="text-sm font-semibold" style={{ color: s.text }}>审计日志</h3>
        <button onClick={clear} className="rounded p-1 transition-colors" style={{ color: s.textTer }}>
          <Trash2 size={14} />
        </button>
      </div>

      {stats && (
        <div className="grid grid-cols-4 gap-2">
          <Stat label="总计" value={stats.total} />
          <Stat label="已允许" value={stats.allowed} color="var(--color-success)" />
          <Stat label="已拒绝" value={stats.denied} color="var(--color-error)" />
          <Stat label="已询问" value={stats.asked} color="var(--color-warning)" />
        </div>
      )}

      {logs.map((log) => (
        <div key={log.id} className="rounded-lg border px-3 py-2"
          style={{ borderColor: s.border, background: s.bg }}>
          <div className="flex items-center gap-2">
            <ShieldCheck size={12} style={{ color: s.textTer }} />
            <span className="text-xs font-mono" style={{ color: s.text }}>{log.tool_name}</span>
            <span className="ml-auto">
              <StatusBadge
                status={log.decision === 'allow' ? 'success' : log.decision === 'deny' ? 'error' : 'warning'}
                label={log.decision === 'allow' ? '已允许' : log.decision === 'deny' ? '已拒绝' : '已询问'}
                size="sm"
              />
            </span>
          </div>
          <p className="mt-1 text-[10px]" style={{ color: s.textTer }}>{log.timestamp}</p>
          {log.reason && <p className="mt-1 text-xs" style={{ color: s.textSec }}>{log.reason}</p>}
        </div>
      ))}

      {logs.length === 0 && !stats && (
        <div className="py-8 text-center text-xs" style={{ color: s.textTer }}>
          暂无审计日志
        </div>
      )}
    </div>
  );
}

function Stat({ label, value, color }: { label: string; value: number; color?: string }) {
  return (
    <div className="rounded-lg p-2 text-center" style={{ background: 'var(--bg-hover)' }}>
      <div className="text-lg font-semibold" style={{ color: color || 'var(--text-primary)' }}>{value}</div>
      <div className="text-[10px]" style={{ color: 'var(--text-tertiary)' }}>{label}</div>
    </div>
  );
}
