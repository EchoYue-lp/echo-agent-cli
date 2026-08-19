import { useCallback, useEffect, useState, type ReactNode } from 'react';
import { Minimize2, BarChart3, Zap, Brain, ShieldCheck, Workflow } from 'lucide-react';
import { compressApi } from '../../api/endpoints';
import type { CompressionStats, CompressResponse } from '../../types/api';
import { cacheHitRate, useChatStore } from '../../stores/chatStore';
import { useConversationStore } from '../../stores/conversationStore';
import { useSubagentRunStore } from '../../stores/subagentRunStore';
import { useTaskRuntimeStore } from '../../stores/taskRuntimeStore';
import { summarizeSubagentUsage } from './subagentUsage';

function formatTokens(n: number): string {
  if (n < 1000) return String(n);
  const formatted = (n / 1000).toFixed(n < 10_000 ? 1 : 0);
  return formatted.endsWith('.0') ? `${Math.round(n / 1000)}k` : `${formatted}k`;
}

function formatPct(pct: number): string {
  if (pct > 0 && pct < 1) return '<1%';
  if (pct < 10) return `${Math.round(pct * 10) / 10}%`;
  return `${Math.round(pct)}%`;
}

function usageTone(pct: number): string {
  if (pct >= 90) return 'var(--color-error)';
  if (pct >= 70) return 'var(--color-warning)';
  return 'var(--accent)';
}

export function CompressPanel() {
  const [stats, setStats] = useState<CompressionStats | null>(null);
  const [lastCompress, setLastCompress] = useState<CompressResponse | null>(null);
  const [compressing, setCompressing] = useState(false);
  const [msg, setMsg] = useState<string | null>(null);
  const contextWindow = useChatStore((s) => s.contextWindow);
  const usageAccumulator = useChatStore((s) => s.usageAccumulator);
  const subagentRuns = useSubagentRunStore((s) => s.runs);
  const activeConversationId = useConversationStore((s) => s.activeId);
  const runtimeConversationId = useTaskRuntimeStore((s) => s.activeRun?.conversation_id ?? null);
  const targetConversationId = activeConversationId ?? runtimeConversationId ?? undefined;

  const loadStats = useCallback(async () => {
    try {
      const data = await compressApi.getStats(targetConversationId);
      setStats(data);
    } catch (e) {
      console.error(e);
    }
  }, [targetConversationId]);

  useEffect(() => {
    void loadStats();
  }, [loadStats]);

  const compress = async () => {
    setCompressing(true);
    setMsg(null);
    try {
      const res = await compressApi.trigger({ conversation_id: targetConversationId });
      if (res.success) {
        setLastCompress(res);
        // 与后端 emit context_compressed 对齐：Snapshot 置空（Accumulator 保留）。
        // 有实际压缩时才清；"No messages to compress" 时 before/after 均为 0。
        if (res.messages_before > 0 || res.messages_after > 0) {
          useChatStore.getState().clearContextWindow();
        }
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
  const mainPct =
    stats && contextWindow && stats.token_limit > 0
      ? (contextWindow.inputTokens / stats.token_limit) * 100
      : null;
  const subagentUsage = summarizeSubagentUsage(
    Object.values(subagentRuns),
    activeConversationId ?? runtimeConversationId
  );
  const cacheRate = cacheHitRate(usageAccumulator);

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
            <div className="flex min-w-0 flex-1 items-center justify-between gap-2">
              <span className="text-xs font-medium" style={{ color: 'var(--text-primary)' }}>
                上下文运行态
              </span>
              <span className="text-[10px]" style={{ color: 'var(--text-tertiary)' }}>
                main / subagent
              </span>
            </div>
          </div>

          <div className="grid grid-cols-2 gap-2">
            <MetricTile
              icon={<Brain size={13} />}
              label="主 agent 输入"
              value={
                contextWindow
                  ? `${formatTokens(contextWindow.inputTokens)} / ${formatTokens(stats.token_limit)}`
                  : '等待 usage'
              }
              sub={
                mainPct != null
                  ? `${formatPct(mainPct)} · ${contextWindow?.usageReported ? 'provider' : '估算'}`
                  : '最近一次 LLM 输入'
              }
              accent={mainPct != null ? usageTone(mainPct) : 'var(--text-tertiary)'}
            />
            <MetricTile
              icon={<Minimize2 size={13} />}
              label="压缩候选"
              value={`${formatTokens(stats.current_tokens)} / ${formatTokens(stats.token_limit)}`}
              sub={`${stats.message_count} 条 · ${formatPct(usagePct)}`}
              accent={usageTone(usagePct)}
            />
            <MetricTile
              icon={<ShieldCheck size={13} />}
              label="恢复胶囊"
              value={stats.runtime_recovery_active ? '已继承' : '未激活'}
              sub={`${stats.protected_message_count ?? 0} 条 protected`}
              accent={
                stats.runtime_recovery_active ? 'var(--color-success)' : 'var(--text-tertiary)'
              }
            />
            <MetricTile
              icon={<Workflow size={13} />}
              label="Subagent trace"
              value={`${subagentUsage.running}/${subagentUsage.total}`}
              sub={`输入 ${formatTokens(subagentUsage.input)} · 输出 ${formatTokens(subagentUsage.output)}`}
              accent={subagentUsage.running > 0 ? 'var(--color-info)' : 'var(--text-tertiary)'}
            />
          </div>

          <div className="mt-3 space-y-1.5 text-xs" style={{ color: 'var(--text-secondary)' }}>
            <div className="flex justify-between">
              <span>缓存命中率</span>
              <span style={{ color: 'var(--text-primary)' }}>
                {cacheRate == null ? '--' : `${(cacheRate * 100).toFixed(1)}%`}
              </span>
            </div>
            <div className="mt-2">
              <div className="flex justify-between mb-1">
                <span>压缩候选使用率</span>
                <span style={{ color: usageTone(usagePct) }}>{formatPct(usagePct)}</span>
              </div>
              <div
                className="h-2.5 rounded-full overflow-hidden border"
                style={{
                  background: 'var(--bg-hover)',
                  borderColor: 'var(--border-primary)',
                }}
              >
                <div
                  className="h-full rounded-full transition-all duration-500"
                  style={{
                    width: `${Math.min(usagePct, 100)}%`,
                    background: usageTone(usagePct),
                    boxShadow: `0 0 10px ${usageTone(usagePct)}`,
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

function MetricTile({
  icon,
  label,
  value,
  sub,
  accent,
}: {
  icon: ReactNode;
  label: string;
  value: string;
  sub: string;
  accent: string;
}) {
  return (
    <div
      className="min-w-0 rounded-md border p-2"
      style={{
        borderColor: `color-mix(in srgb, ${accent} 30%, var(--border-primary))`,
        background: `color-mix(in srgb, ${accent} 8%, var(--bg-secondary))`,
      }}
    >
      <div className="mb-1 flex items-center gap-1.5 text-[10px]" style={{ color: accent }}>
        {icon}
        <span className="truncate font-medium">{label}</span>
      </div>
      <div className="truncate text-[12px] font-semibold" style={{ color: 'var(--text-primary)' }}>
        {value}
      </div>
      <div className="mt-0.5 truncate text-[10px]" style={{ color: 'var(--text-tertiary)' }}>
        {sub}
      </div>
    </div>
  );
}
