import { useEffect, useState } from 'react';
import { Minimize2, BarChart3, Zap } from 'lucide-react';
import { compressApi } from '../../api/endpoints';
import type { CompressionStats, CompressResponse } from '../../types/api';

export function CompressPanel() {
  const [stats, setStats] = useState<CompressionStats | null>(null);
  const [lastCompress, setLastCompress] = useState<CompressResponse | null>(null);
  const [compressing, setCompressing] = useState(false);
  const [msg, setMsg] = useState<string | null>(null);

  const loadStats = async () => {
    try {
      const data = await compressApi.getStats();
      setStats(data);
    } catch (e) {
      console.error(e);
    }
  };

  useEffect(() => { loadStats(); }, []);

  const compress = async () => {
    setCompressing(true);
    setMsg(null);
    try {
      const res = await compressApi.trigger();
      if (res.success) {
        setLastCompress(res);
        setMsg(`Compressed: ${res.messages_before} → ${res.messages_after} messages, saved ${res.tokens_saved} tokens`);
        await loadStats();
      } else {
        setMsg(res.message || 'Compression returned no result');
      }
    } catch (e: unknown) {
      setMsg(`Error: ${e instanceof Error ? e.message : 'Unknown'}`);
    }
    setCompressing(false);
  };

  const usagePct = stats && stats.token_limit > 0
    ? (stats.current_tokens / stats.token_limit * 100)
    : 0;

  return (
    <div className="p-3 space-y-3">
      <div className="flex items-center justify-between">
        <h3 className="text-sm font-semibold" style={{ color: 'var(--text-primary)' }}>
          Context Compression
        </h3>
        <button onClick={loadStats} className="text-[10px]" style={{ color: 'var(--accent)' }}>
          Refresh
        </button>
      </div>

      {/* Context stats */}
      {stats && (
        <div className="rounded-lg border p-3" style={{ borderColor: 'var(--border-primary)' }}>
          <div className="flex items-center gap-2 mb-2">
            <BarChart3 size={14} style={{ color: 'var(--accent)' }} />
            <span className="text-xs font-medium" style={{ color: 'var(--text-primary)' }}>Current Context</span>
          </div>
          <div className="space-y-1.5 text-xs" style={{ color: 'var(--text-secondary)' }}>
            <div className="flex justify-between">
              <span>Messages</span>
              <span style={{ color: 'var(--text-primary)' }}>{stats.message_count}</span>
            </div>
            <div className="flex justify-between">
              <span>Tokens</span>
              <span style={{ color: 'var(--text-primary)' }}>{stats.current_tokens} / {stats.token_limit}</span>
            </div>
            <div className="mt-2">
              <div className="flex justify-between mb-1">
                <span>Usage</span>
                <span style={{ color: usagePct > 80 ? '#ef4444' : usagePct > 50 ? '#f59e0b' : '#10b981' }}>
                  {usagePct.toFixed(1)}%
                </span>
              </div>
              <div className="h-2 rounded-full overflow-hidden" style={{ background: 'var(--bg-hover)' }}>
                <div
                  className="h-full rounded-full transition-all duration-500"
                  style={{
                    width: `${Math.min(usagePct, 100)}%`,
                    background: usagePct > 80 ? '#ef4444' : usagePct > 50 ? '#f59e0b' : '#10b981',
                  }}
                />
              </div>
            </div>
            {stats.needs_compression && (
              <div className="mt-1 rounded px-2 py-1 text-[10px] font-medium" style={{ background: '#f59e0b18', color: '#f59e0b' }}>
                Compression recommended
              </div>
            )}
          </div>
        </div>
      )}

      {/* Compress button */}
      <button
        onClick={compress}
        disabled={compressing}
        className="flex w-full items-center justify-center gap-2 rounded-lg py-2.5 text-xs font-medium transition-colors"
        style={{
          background: compressing ? 'var(--border-primary)' : 'var(--accent)',
          color: compressing ? 'var(--text-tertiary)' : 'white',
        }}
      >
        {compressing ? (
          <><div className="spinner" /> Compressing...</>
        ) : (
          <><Zap size={12} /> Compress Context</>
        )}
      </button>

      {msg && (
        <div className="rounded-lg px-3 py-2 text-xs" style={{
          background: 'var(--accent-bg)',
          color: 'var(--accent)',
        }}>
          {msg}
        </div>
      )}

      {/* Last compression result */}
      {lastCompress && (
        <div className="rounded-lg border p-3" style={{ borderColor: 'var(--border-primary)' }}>
          <div className="flex items-center gap-2 mb-2">
            <Minimize2 size={14} style={{ color: 'var(--accent)' }} />
            <span className="text-xs font-medium" style={{ color: 'var(--text-primary)' }}>Last Compression</span>
          </div>
          <div className="grid grid-cols-2 gap-2 text-xs">
            <div className="rounded-lg p-2 text-center" style={{ background: 'var(--bg-hover)' }}>
              <div style={{ color: 'var(--text-tertiary)' }}>Messages</div>
              <div className="font-medium" style={{ color: 'var(--text-primary)' }}>
                {lastCompress.messages_before} → {lastCompress.messages_after}
              </div>
            </div>
            <div className="rounded-lg p-2 text-center" style={{ background: 'var(--bg-hover)' }}>
              <div style={{ color: 'var(--text-tertiary)' }}>Tokens Saved</div>
              <div className="font-medium" style={{ color: '#10b981' }}>
                {lastCompress.tokens_saved}
              </div>
            </div>
          </div>
        </div>
      )}

      {!stats && (
        <div className="py-8 text-center text-xs" style={{ color: 'var(--text-tertiary)' }}>
          <Minimize2 size={24} className="mx-auto mb-2" />
          Send a message to see context stats
        </div>
      )}
    </div>
  );
}
