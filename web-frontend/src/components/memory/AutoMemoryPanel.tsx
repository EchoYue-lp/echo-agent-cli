import { useEffect, useState, useCallback } from 'react';
import {
  autoMemoryApi,
  type AutoMemoryStatus,
  type AutoMemoryObservation,
} from '../../api/endpoints';
import {
  Brain,
  RefreshCw,
  Zap,
  ToggleLeft,
  ToggleRight,
  AlertCircle,
  Tag,
} from 'lucide-react';

const CATEGORY_COLORS: Record<string, { bg: string; text: string }> = {
  Project: { bg: 'rgba(59,130,246,0.12)', text: 'var(--color-info, #3b82f6)' },
  User: { bg: 'rgba(168,85,247,0.12)', text: 'var(--color-purple, #a855f7)' },
  Bug: { bg: 'rgba(239,68,68,0.12)', text: 'var(--color-error, #ef4444)' },
  Decision: { bg: 'rgba(234,179,8,0.12)', text: 'var(--color-warning, #eab308)' },
  FilePath: { bg: 'rgba(34,197,94,0.12)', text: 'var(--color-success, #22c55e)' },
};

const CATEGORY_LABELS: Record<string, string> = {
  Project: '项目',
  User: '用户',
  Bug: '缺陷',
  Decision: '决策',
  FilePath: '路径',
};

export function AutoMemoryPanel() {
  const [status, setStatus] = useState<AutoMemoryStatus | null>(null);
  const [observations, setObservations] = useState<AutoMemoryObservation[]>([]);
  const [loading, setLoading] = useState(true);
  const [extracting, setExtracting] = useState(false);
  const [toggling, setToggling] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const s = {
    text: 'var(--text-primary)',
    textSec: 'var(--text-secondary)',
    textTer: 'var(--text-tertiary)',
    border: 'var(--border-primary)',
    bg: 'var(--bg-primary)',
    bgHover: 'var(--bg-hover)',
    bgCard: 'var(--bg-secondary)',
    accent: 'var(--accent)',
  };

  const fetchStatus = useCallback(async () => {
    try {
      const data = await autoMemoryApi.status();
      setStatus(data);
    } catch (e) {
      console.error('Failed to fetch auto-memory status:', e);
    }
  }, []);

  const fetchObservations = useCallback(async () => {
    try {
      const data = await autoMemoryApi.observations();
      setObservations(data);
    } catch (e) {
      console.error('Failed to fetch observations:', e);
    }
  }, []);

  useEffect(() => {
    Promise.all([fetchStatus(), fetchObservations()]).finally(() => setLoading(false));
  }, [fetchStatus, fetchObservations]);

  const handleToggle = async () => {
    if (!status) return;
    setToggling(true);
    try {
      await autoMemoryApi.toggle(!status.enabled);
      setStatus({ ...status, enabled: !status.enabled });
    } catch (e) {
      setError(String(e));
    } finally {
      setToggling(false);
    }
  };

  const handleExtract = async () => {
    setExtracting(true);
    setError(null);
    try {
      const res = await autoMemoryApi.extract();
      if (res.observations?.length) {
        setObservations((prev) => [...res.observations, ...prev]);
      }
      fetchStatus();
      fetchObservations();
    } catch (e) {
      setError(String(e));
    } finally {
      setExtracting(false);
    }
  };

  // Group observations by category
  const grouped = observations.reduce<Record<string, AutoMemoryObservation[]>>((acc, obs) => {
    const cat = obs.category || 'Other';
    if (!acc[cat]) acc[cat] = [];
    acc[cat].push(obs);
    return acc;
  }, {});

  if (loading) {
    return (
      <div className="p-3">
        <p className="text-xs" style={{ color: s.textTer }}>加载中...</p>
      </div>
    );
  }

  return (
    <div className="space-y-4">
      {/* Header with toggle */}
      <div className="flex items-center justify-between">
        <div className="flex items-center gap-2">
          <Brain size={16} style={{ color: s.accent }} />
          <h3 className="text-sm font-semibold" style={{ color: s.text }}>
            自动记忆
          </h3>
          {status && (
            <span
              className="rounded-full px-2 py-0.5 text-[10px] font-medium"
              style={{
                background: status.enabled ? 'rgba(34,197,94,0.12)' : 'rgba(156,163,175,0.15)',
                color: status.enabled ? 'var(--color-success, #22c55e)' : s.textTer,
              }}
            >
              {status.enabled ? '已启用' : '已禁用'}
            </span>
          )}
        </div>
        <div className="flex items-center gap-2">
          <button
            onClick={handleToggle}
            disabled={toggling}
            className="transition-colors disabled:opacity-50"
            style={{
              color: status?.enabled ? 'var(--color-success, #22c55e)' : s.textTer,
            }}
            title={status?.enabled ? '禁用' : '启用'}
          >
            {status?.enabled ? <ToggleRight size={20} /> : <ToggleLeft size={20} />}
          </button>
        </div>
      </div>

      {/* Description */}
      <p className="text-[11px] leading-relaxed" style={{ color: s.textSec }}>
        自动记忆功能会在对话结束后自动提取关键观察（项目结构、用户偏好、决策、缺陷等），并保存到项目记忆中，供后续对话使用。
      </p>

      {/* Actions */}
      <div className="flex items-center gap-2">
        <button
          onClick={handleExtract}
          disabled={extracting}
          className="flex items-center gap-1.5 rounded-lg px-3 py-2 text-xs font-medium text-white transition-colors disabled:opacity-50"
          style={{ background: s.accent }}
        >
          {extracting ? (
            <RefreshCw size={12} className="animate-spin" />
          ) : (
            <Zap size={12} />
          )}
          {extracting ? '提取中...' : '立即提取'}
        </button>
        <button
          onClick={() => { fetchStatus(); fetchObservations(); }}
          className="rounded-lg p-2 transition-colors hover:opacity-80"
          style={{ color: s.textTer }}
          title="刷新"
        >
          <RefreshCw size={14} />
        </button>
      </div>

      {/* Error */}
      {error && (
        <div
          className="flex items-start gap-2 rounded-lg border p-3 text-[11px]"
          style={{ borderColor: 'rgba(239,68,68,0.3)', background: 'rgba(239,68,68,0.05)', color: 'var(--color-error, #ef4444)' }}
        >
          <AlertCircle size={12} className="mt-0.5 flex-shrink-0" />
          <span>{error}</span>
        </div>
      )}

      {/* Stats */}
      {status && (
        <div
          className="flex items-center gap-4 rounded-lg border px-4 py-3 text-xs"
          style={{ borderColor: s.border, background: s.bgCard }}
        >
          <div>
            <span style={{ color: s.textTer }}>观察总数</span>
            <p className="text-base font-semibold" style={{ color: s.text }}>
              {status.observations_count}
            </p>
          </div>
        </div>
      )}

      {/* Observations grouped by category */}
      {observations.length === 0 ? (
        <p className="py-6 text-center text-xs" style={{ color: s.textTer }}>
          暂无观察记录。点击"立即提取"从当前会话中提取。
        </p>
      ) : (
        <div className="space-y-4">
          {Object.entries(grouped).map(([category, obs]) => {
            const colors = CATEGORY_COLORS[category] || { bg: s.bgHover, text: s.textSec };
            const label = CATEGORY_LABELS[category] || category;
            return (
              <div key={category} className="space-y-2">
                <div className="flex items-center gap-2">
                  <Tag size={11} style={{ color: colors.text }} />
                  <span
                    className="rounded-full px-2 py-0.5 text-[10px] font-semibold"
                    style={{ background: colors.bg, color: colors.text }}
                  >
                    {label}
                  </span>
                  <span className="text-[10px]" style={{ color: s.textTer }}>
                    {obs.length} 条
                  </span>
                </div>
                <div className="space-y-1.5 pl-4">
                  {obs.map((o, idx) => (
                    <div
                      key={idx}
                      className="rounded-lg border px-3 py-2"
                      style={{ borderColor: s.border, background: s.bg }}
                    >
                      <p className="text-xs leading-relaxed" style={{ color: s.text }}>
                        {o.text}
                      </p>
                      <div className="mt-1 flex items-center gap-2 text-[10px]" style={{ color: s.textTer }}>
                        <span>置信度: {Math.round(o.confidence * 100)}%</span>
                        <div
                          className="h-1 flex-1 max-w-[80px] rounded-full overflow-hidden"
                          style={{ background: s.bgHover }}
                        >
                          <div
                            className="h-full rounded-full"
                            style={{
                              width: `${Math.round(o.confidence * 100)}%`,
                              background: colors.text,
                            }}
                          />
                        </div>
                      </div>
                    </div>
                  ))}
                </div>
              </div>
            );
          })}
        </div>
      )}
    </div>
  );
}
