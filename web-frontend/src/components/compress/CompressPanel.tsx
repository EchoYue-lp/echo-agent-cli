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

  useEffect(() => {
    loadStats();
  }, []);

  const compress = async () => {
    setCompressing(true);
    setMsg(null);
    try {
      const res = await compressApi.trigger();
      if (res.success) {
        setLastCompress(res);
        setMsg(
          `已压缩：${res.messages_before} → ${res.messages_after} 条消息，节省 ${res.tokens_saved} 个令牌`
        );
        await loadStats();
      } else {
        setMsg(res.message || '压缩未返回结果');
      }
    } catch (e: unknown) {
      setMsg(`错误：${e instanceof Error ? e.message : '未知'}`);
    }
    setCompressing(false);
  };

  const usagePct =
    stats && stats.token_limit > 0 ? (stats.current_tokens / stats.token_limit) * 100 : 0;

  return (
    <div className="p-3 space-y-3">
      <div className="flex items-center justify-between">
        <h3 className="text-sm font-semibold" style={{ color: 'var(--text-primary)' }}>
          上下文压缩
        </h3>
        <button onClick={loadStats} className="text-[10px]" style={{ color: 'var(--accent)' }}>
          刷新
        </button>
      </div>

      {/* Context stats */}
      {stats && (
        <div className="rounded-lg border p-3" style={{ borderColor: 'var(--border-primary)' }}>
          <div className="flex items-center gap-2 mb-2">
            <BarChart3 size={14} style={{ color: 'var(--accent)' }} />
            <span className="text-xs font-medium" style={{ color: 'var(--text-primary)' }}>
              当前上下文
            </span>
          </div>
          <div className="space-y-1.5 text-xs" style={{ color: 'var(--text-secondary)' }}>
            <div className="flex justify-between">
              <span>消息数</span>
              <span style={{ color: 'var(--text-primary)' }}>{stats.message_count}</span>
            </div>
            <div className="flex justify-between">
              <span>令牌数</span>
              <span style={{ color: 'var(--text-primary)' }}>
                {stats.current_tokens} / {stats.token_limit}
              </span>
            </div>
            <div className="mt-2">
              <div className="flex justify-between mb-1">
                <span>使用率</span>
                <span
                  style={{
                    color:
                      usagePct > 80
                        ? 'var(--color-error)'
                        : usagePct > 50
                          ? 'var(--color-warning)'
                          : 'var(--color-success)',
                  }}
                >
                  {usagePct.toFixed(1)}%
                </span>
              </div>
              <div
                className="h-2 rounded-full overflow-hidden"
                style={{ background: 'var(--bg-hover)' }}
              >
                <div
                  className="h-full rounded-full transition-all duration-500"
                  style={{
                    width: `${Math.min(usagePct, 100)}%`,
                    background:
                      usagePct > 80
                        ? 'var(--color-error)'
                        : usagePct > 50
                          ? 'var(--color-warning)'
                          : 'var(--color-success)',
                  }}
                />
              </div>
            </div>
            {stats.needs_compression && (
              <div
                className="mt-1 rounded-md px-2 py-1 text-[10px] font-medium"
                style={{ background: 'var(--color-warning-bg)', color: 'var(--color-warning)' }}
              >
                建议压缩
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
          background: compressing ? 'var(--border-primary)' : 'var(--action-run)',
          color: compressing ? 'var(--text-tertiary)' : 'var(--text-on-run)',
        }}
      >
        {compressing ? (
          <>
            <div className="spinner" /> 压缩中...
          </>
        ) : (
          <>
            <Zap size={12} /> 压缩上下文
          </>
        )}
      </button>

      {msg && (
        <div
          className="rounded-lg px-3 py-2 text-xs"
          style={{
            background: 'var(--accent-bg)',
            color: 'var(--accent)',
          }}
        >
          {msg}
        </div>
      )}

      {/* Last compression result */}
      {lastCompress && (
        <div className="rounded-lg border p-3" style={{ borderColor: 'var(--border-primary)' }}>
          <div className="flex items-center gap-2 mb-2">
            <Minimize2 size={14} style={{ color: 'var(--accent)' }} />
            <span className="text-xs font-medium" style={{ color: 'var(--text-primary)' }}>
              上次压缩
            </span>
          </div>
          <div className="grid grid-cols-2 gap-2 text-xs">
            <div className="rounded-lg p-2 text-center" style={{ background: 'var(--bg-hover)' }}>
              <div style={{ color: 'var(--text-tertiary)' }}>消息数</div>
              <div className="font-medium" style={{ color: 'var(--text-primary)' }}>
                {lastCompress.messages_before} → {lastCompress.messages_after}
              </div>
            </div>
            <div className="rounded-lg p-2 text-center" style={{ background: 'var(--bg-hover)' }}>
              <div style={{ color: 'var(--text-tertiary)' }}>节省令牌</div>
              <div className="font-medium" style={{ color: 'var(--color-success)' }}>
                {lastCompress.tokens_saved}
              </div>
            </div>
          </div>
        </div>
      )}

      {!stats && (
        <div className="py-8 text-center text-xs" style={{ color: 'var(--text-tertiary)' }}>
          <Minimize2 size={24} className="mx-auto mb-2" />
          发送消息以查看上下文统计
        </div>
      )}
    </div>
  );
}
