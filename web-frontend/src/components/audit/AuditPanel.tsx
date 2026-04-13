import { useEffect, useState } from 'react';
import { auditApi } from '../../api/endpoints';
import type { AuditLog } from '../../types/api';
import { ShieldCheck, Trash2 } from 'lucide-react';

export function AuditPanel() {
  const [logs, setLogs] = useState<AuditLog[]>([]);
  const [stats, setStats] = useState<{ total: number; allowed: number; denied: number; asked: number } | null>(null);

  useEffect(() => {
    auditApi.logs().then(setLogs).catch(console.error);
    auditApi.stats().then(setStats).catch(console.error);
  }, []);

  const clear = async () => {
    try {
      await auditApi.clear();
      setLogs([]);
      setStats(null);
    } catch (e) {
      console.error(e);
    }
  };

  return (
    <div className="p-3 space-y-3">
      <div className="flex items-center justify-between">
        <h3 className="text-sm font-semibold text-gray-700">Audit Log</h3>
        <button onClick={clear} className="rounded p-1 text-gray-400 hover:bg-gray-100 hover:text-red-500">
          <Trash2 size={14} />
        </button>
      </div>

      {stats && (
        <div className="grid grid-cols-4 gap-2">
          <Stat label="Total" value={stats.total} />
          <Stat label="Allowed" value={stats.allowed} color="text-green-600" />
          <Stat label="Denied" value={stats.denied} color="text-red-600" />
          <Stat label="Asked" value={stats.asked} color="text-amber-600" />
        </div>
      )}

      {logs.map((log) => (
        <div key={log.id} className="rounded border border-gray-200 bg-white px-3 py-2">
          <div className="flex items-center gap-2">
            <ShieldCheck size={12} className="text-gray-400" />
            <span className="text-xs font-mono text-gray-700">{log.tool_name}</span>
            <span className={`ml-auto rounded px-1.5 py-0.5 text-[10px] ${
              log.decision === 'allow' ? 'bg-green-50 text-green-600' :
              log.decision === 'deny' ? 'bg-red-50 text-red-600' :
              'bg-amber-50 text-amber-600'
            }`}>
              {log.decision}
            </span>
          </div>
          <p className="mt-1 text-[10px] text-gray-400">{log.timestamp}</p>
          {log.reason && <p className="mt-1 text-xs text-gray-500">{log.reason}</p>}
        </div>
      ))}
    </div>
  );
}

function Stat({ label, value, color }: { label: string; value: number; color?: string }) {
  return (
    <div className="rounded bg-gray-50 p-2 text-center">
      <div className={`text-lg font-semibold ${color || ''}`}>{value}</div>
      <div className="text-[10px] text-gray-400">{label}</div>
    </div>
  );
}
