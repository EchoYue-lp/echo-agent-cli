import { useEffect, useState } from 'react';
import {
  Sparkles,
  BarChart3,
  Search,
  RefreshCw,
  Pin,
  Play,
  CheckCircle,
  XCircle,
  Archive,
  Clock,
  Activity,
  Database,
  Heart,
  Gift,
} from 'lucide-react';
import { evolutionApi } from '../../api/endpoints';
import { useToastStore } from '../../stores/toastStore';
import type {
  TrajectoryEntry,
  TrajectoryStats,
  CuratorStatus,
  CuratorTransition,
  DashboardMetrics,
  RuleProposal,
} from '../../types/api';

export function EvolutionPanel() {
  const addToast = useToastStore((s) => s.addToast);

  // ── Dashboard state(进化概览:分层记忆统计 + 技能健康 + 变更活动)
  const [dashboard, setDashboard] = useState<DashboardMetrics | null>(null);
  const [loadingDashboard, setLoadingDashboard] = useState(false);

  // ── Rule proposals state(规则候选:用户审阅 → 采纳才写 AGENTS.md)
  const [proposals, setProposals] = useState<RuleProposal[]>([]);
  const [loadingProposals, setLoadingProposals] = useState(false);
  const [promotingKey, setPromotingKey] = useState<string | null>(null);

  // ── Trajectory state
  const [stats, setStats] = useState<TrajectoryStats | null>(null);
  const [trajectories, setTrajectories] = useState<TrajectoryEntry[]>([]);
  const [loadingStats, setLoadingStats] = useState(false);
  const [loadingTraj, setLoadingTraj] = useState(false);

  // ── Review state
  const [reviewing, setReviewing] = useState(false);
  const [reviewResult, setReviewResult] = useState<{
    success: boolean;
    run_id: string;
    actions: string[];
    nothing_to_save: boolean;
    error?: string | null;
  } | null>(null);
  const [reviewError, setReviewError] = useState<string | null>(null);

  // ── Curator state
  const [curatorStatus, setCuratorStatus] = useState<CuratorStatus | null>(null);
  const [transitions, setTransitions] = useState<CuratorTransition[]>([]);
  const [curatorLoading, setCuratorLoading] = useState(false);
  const [curatorMsg, setCuratorMsg] = useState<string | null>(null);

  // ── Load data
  const loadDashboard = async () => {
    setLoadingDashboard(true);
    try {
      const data = await evolutionApi.dashboard();
      setDashboard(data.metrics);
    } catch (e) {
      console.error('Failed to load evolution dashboard:', e);
    }
    setLoadingDashboard(false);
  };

  const loadProposals = async () => {
    setLoadingProposals(true);
    try {
      const data = await evolutionApi.scanProposals();
      setProposals(data.proposals);
    } catch (e) {
      console.error('Failed to load rule proposals:', e);
    }
    setLoadingProposals(false);
  };

  // review gate:用户点「采纳」才写 AGENTS.md。采纳后 toast 提示 + 刷新候选 +
  // 刷新 dashboard(晋升会改 memory 状态 + 写变更日志)。
  const handlePromote = async (memoryKey: string) => {
    setPromotingKey(memoryKey);
    try {
      await evolutionApi.promoteRule(memoryKey);
      addToast('success', `已采纳规则并写入 AGENTS.md`);
      await Promise.all([loadProposals(), loadDashboard()]);
    } catch (e) {
      addToast('error', `采纳失败: ${e instanceof Error ? e.message : '未知错误'}`);
    }
    setPromotingKey(null);
  };

  const loadStats = async () => {
    setLoadingStats(true);
    try {
      const data = await evolutionApi.trajectoryStats();
      setStats(data.stats);
    } catch (e) {
      console.error('Failed to load trajectory stats:', e);
    }
    setLoadingStats(false);
  };

  const loadTrajectories = async () => {
    setLoadingTraj(true);
    try {
      const data = await evolutionApi.trajectories();
      setTrajectories(data.trajectories.slice(0, 20));
    } catch (e) {
      console.error('Failed to load trajectories:', e);
    }
    setLoadingTraj(false);
  };

  const loadCuratorStatus = async () => {
    try {
      const res = await evolutionApi.curator('status');
      if (res.status) setCuratorStatus(res.status);
    } catch (e) {
      console.error('Failed to load curator status:', e);
    }
  };

  useEffect(() => {
    loadDashboard();
    loadProposals();
    loadStats();
    loadTrajectories();
    loadCuratorStatus();
  }, []);

  // ── Actions
  const runReview = async () => {
    setReviewing(true);
    setReviewResult(null);
    setReviewError(null);
    try {
      const res = await evolutionApi.review();
      if (res.error && !res.success) {
        setReviewError(res.error);
      } else {
        setReviewResult(res);
      }
    } catch (e: unknown) {
      setReviewError(e instanceof Error ? e.message : 'Unknown error');
    }
    setReviewing(false);
  };

  const runCurator = async () => {
    setCuratorLoading(true);
    setCuratorMsg(null);
    setTransitions([]);
    try {
      const res = await evolutionApi.curator('run');
      if (res.error) {
        setCuratorMsg(`Error: ${res.error}`);
      } else {
        setTransitions(res.transitions ?? []);
        setCuratorMsg(res.count ? `Applied ${res.count} transition(s)` : 'No transitions needed');
        await loadCuratorStatus();
      }
    } catch (e: unknown) {
      setCuratorMsg(`Error: ${e instanceof Error ? e.message : 'Unknown error'}`);
    }
    setCuratorLoading(false);
  };

  return (
    <div className="space-y-6">
      {/* ── Section 0: Evolution Overview (Dashboard) ── */}
      <section>
        <div className="flex items-center justify-between mb-3">
          <div className="flex items-center gap-2">
            <Database size={14} style={{ color: 'var(--accent)' }} />
            <h3 className="text-sm font-semibold" style={{ color: 'var(--text-primary)' }}>
              进化概览
            </h3>
          </div>
          <button
            onClick={loadDashboard}
            disabled={loadingDashboard}
            className="flex items-center gap-1 text-[10px] font-medium transition-colors"
            style={{ color: 'var(--accent)' }}
          >
            <RefreshCw size={10} className={loadingDashboard ? 'animate-spin' : ''} />
            刷新
          </button>
        </div>

        {dashboard ? (
          <>
            {/* 总记忆数 + 技能健康概览 */}
            <div className="grid grid-cols-3 gap-2 mb-3">
              <StatCard label="总记忆" value={dashboard.total_memories} icon={<Database size={10} />} />
              <StatCard
                label="健康技能"
                value={dashboard.skill_health.healthy_skills}
                color="var(--color-success)"
                icon={<Heart size={10} />}
              />
              <StatCard
                label="需关注"
                value={dashboard.skill_health.needs_attention}
                color="var(--color-warning, orange)"
              />
            </div>

            {/* 按记忆类型分布 */}
            {Object.keys(dashboard.memory_by_type).length > 0 && (
              <div
                className="rounded-lg border mb-2"
                style={{ borderColor: 'var(--border-primary)' }}
              >
                <div
                  className="border-b px-3 py-2"
                  style={{ borderColor: 'var(--border-primary)' }}
                >
                  <span
                    className="text-[11px] font-medium"
                    style={{ color: 'var(--text-secondary)' }}
                  >
                    按类型分布
                  </span>
                </div>
                <div
                  className="max-h-40 overflow-y-auto divide-y"
                  style={{ borderColor: 'var(--border-primary)' }}
                >
                  {Object.entries(dashboard.memory_by_type)
                    .sort(([, a], [, b]) => b.count - a.count)
                    .map(([type, s]) => (
                      <div
                        key={type}
                        className="flex items-center justify-between px-3 py-1.5 text-xs"
                        style={{ color: 'var(--text-secondary)' }}
                      >
                        <span className="truncate" style={{ maxWidth: '140px' }}>
                          {type}
                        </span>
                        <div className="flex items-center gap-3 shrink-0">
                          <span>{s.count} 条</span>
                          <span title="活跃 / 归档" style={{ color: 'var(--text-tertiary)' }}>
                            {s.active_count}/{s.archived_count}
                          </span>
                          <span
                            title="平均置信度"
                            className="font-mono"
                            style={{ color: 'var(--accent)' }}
                          >
                            {s.avg_confidence.toFixed(2)}
                          </span>
                        </div>
                      </div>
                    ))}
                </div>
              </div>
            )}

            {/* 最近变更活动 */}
            {dashboard.recent_activities.length > 0 && (
              <div
                className="rounded-lg border"
                style={{ borderColor: 'var(--border-primary)' }}
              >
                <div
                  className="border-b px-3 py-2"
                  style={{ borderColor: 'var(--border-primary)' }}
                >
                  <span
                    className="text-[11px] font-medium"
                    style={{ color: 'var(--text-secondary)' }}
                  >
                    最近变更
                  </span>
                </div>
                <div
                  className="max-h-32 overflow-y-auto divide-y"
                  style={{ borderColor: 'var(--border-primary)' }}
                >
                  {dashboard.recent_activities.slice(0, 6).map((a, i) => (
                    <div
                      key={i}
                      className="flex items-center gap-2 px-3 py-1.5 text-xs"
                      style={{ color: 'var(--text-secondary)' }}
                    >
                      <span
                        className="rounded px-1.5 py-0.5 text-[9px] shrink-0"
                        style={{
                          background: 'var(--bg-hover)',
                          color: 'var(--text-tertiary)',
                        }}
                      >
                        {a.activity_type}
                      </span>
                      <span className="truncate">{a.description || '—'}</span>
                    </div>
                  ))}
                </div>
              </div>
            )}

            {dashboard.generated_at && (
              <div
                className="flex items-center gap-1.5 mt-1 text-[10px]"
                style={{ color: 'var(--text-tertiary)' }}
              >
                <Clock size={10} />
                生成于 {new Date(dashboard.generated_at).toLocaleString()}
              </div>
            )}
          </>
        ) : (
          <div className="py-4 text-center text-xs" style={{ color: 'var(--text-tertiary)' }}>
            {loadingDashboard ? (
              <RefreshCw size={16} className="mx-auto mb-1 animate-spin" />
            ) : (
              <Database size={20} className="mx-auto mb-1" />
            )}
            {loadingDashboard ? '加载中...' : '尚无进化数据。运行对话后将自动积累。'}
          </div>
        )}
      </section>

      {/* ── Section 0b: Rule Promotion Candidates (review gate) ── */}
      <section>
        <div className="flex items-center justify-between mb-3">
          <div className="flex items-center gap-2">
            <Gift size={14} style={{ color: 'var(--accent)' }} />
            <h3 className="text-sm font-semibold" style={{ color: 'var(--text-primary)' }}>
              规则候选
            </h3>
            {proposals.length > 0 && (
              <span
                className="rounded-full px-1.5 py-0.5 text-[9px] font-medium"
                style={{ background: 'var(--accent-bg)', color: 'var(--accent)' }}
              >
                {proposals.length}
              </span>
            )}
          </div>
          <button
            onClick={loadProposals}
            disabled={loadingProposals}
            className="flex items-center gap-1 text-[10px] font-medium transition-colors"
            style={{ color: 'var(--accent)' }}
          >
            <RefreshCw size={10} className={loadingProposals ? 'animate-spin' : ''} />
            刷新
          </button>
        </div>

        <p className="text-xs mb-3" style={{ color: 'var(--text-secondary)' }}>
          高置信记忆可晋升为 AGENTS.md 永久规则。采纳才会写入,agent 不会自动改规则。
        </p>

        {proposals.length > 0 ? (
          <div className="space-y-2">
            {proposals.map((p) => (
              <div
                key={p.memory_key}
                className="rounded-lg border p-3"
                style={{ borderColor: 'var(--border-primary)' }}
              >
                <div className="flex items-center justify-between mb-1.5">
                  <div className="flex items-center gap-2 min-w-0">
                    <span
                      className="rounded px-1.5 py-0.5 text-[9px] shrink-0"
                      style={{ background: 'var(--bg-hover)', color: 'var(--text-tertiary)' }}
                    >
                      {p.memory_type}
                    </span>
                    <span
                      className="font-mono text-[10px] truncate"
                      style={{ color: 'var(--text-tertiary)' }}
                    >
                      {p.memory_key}
                    </span>
                  </div>
                  <span
                    className="font-mono text-[10px] shrink-0"
                    style={{ color: 'var(--accent)' }}
                    title="置信度"
                  >
                    {p.confidence.toFixed(2)}
                  </span>
                </div>
                <p className="text-xs mb-2" style={{ color: 'var(--text-primary)' }}>
                  {p.rule_text}
                </p>
                <div className="flex items-center justify-between">
                  <span className="text-[10px]" style={{ color: 'var(--text-tertiary)' }}>
                    {p.reason}
                  </span>
                  <button
                    onClick={() => handlePromote(p.memory_key)}
                    disabled={promotingKey !== null}
                    className="flex items-center gap-1 rounded px-2 py-1 text-[10px] font-medium transition-colors shrink-0"
                    style={{
                      background:
                        promotingKey === p.memory_key
                          ? 'var(--border-primary)'
                          : 'var(--action-run)',
                      color:
                        promotingKey === p.memory_key
                          ? 'var(--text-tertiary)'
                          : 'var(--text-on-run)',
                    }}
                  >
                    {promotingKey === p.memory_key ? (
                      <>
                        <RefreshCw size={9} className="animate-spin" /> 采纳中...
                      </>
                    ) : (
                      <>
                        <CheckCircle size={9} /> 采纳
                      </>
                    )}
                  </button>
                </div>
              </div>
            ))}
          </div>
        ) : (
          <div className="py-4 text-center text-xs" style={{ color: 'var(--text-tertiary)' }}>
            {loadingProposals ? (
              <RefreshCw size={16} className="mx-auto mb-1 animate-spin" />
            ) : (
              <Gift size={20} className="mx-auto mb-1" />
            )}
            {loadingProposals
              ? '加载中...'
              : '暂无规则候选。记忆置信度 ≥ 0.95 且存续 ≥ 7 天后会出现在此。'}
          </div>
        )}
      </section>

      {/* ── Section 1: Trajectory Stats ── */}
      <section>
        <div className="flex items-center justify-between mb-3">
          <div className="flex items-center gap-2">
            <BarChart3 size={14} style={{ color: 'var(--accent)' }} />
            <h3 className="text-sm font-semibold" style={{ color: 'var(--text-primary)' }}>
              轨迹统计
            </h3>
          </div>
          <button
            onClick={() => {
              loadStats();
              loadTrajectories();
            }}
            disabled={loadingStats}
            className="flex items-center gap-1 text-[10px] font-medium transition-colors"
            style={{ color: 'var(--accent)' }}
          >
            <RefreshCw size={10} className={loadingStats ? 'animate-spin' : ''} />
            刷新
          </button>
        </div>

        {stats && (
          <div className="grid grid-cols-3 gap-2 mb-3">
            <StatCard label="总运行数" value={stats.total} />
            <StatCard label="已完成" value={stats.completed} color="var(--color-success)" />
            <StatCard label="已失败" value={stats.failed} color="var(--color-error)" />
            <StatCard label="总令牌" value={formatNumber(stats.total_tokens)} />
            <StatCard label="工具调用" value={stats.total_tool_calls} />
            <StatCard label="平均耗时" value={`${(stats.avg_duration_ms / 1000).toFixed(1)}s`} />
          </div>
        )}

        {/* Recent trajectories */}
        {trajectories.length > 0 && (
          <div className="rounded-lg border" style={{ borderColor: 'var(--border-primary)' }}>
            <div
              className="flex items-center gap-2 border-b px-3 py-2"
              style={{ borderColor: 'var(--border-primary)' }}
            >
              <Activity size={12} style={{ color: 'var(--text-tertiary)' }} />
              <span className="text-[11px] font-medium" style={{ color: 'var(--text-secondary)' }}>
                最近轨迹 ({trajectories.length})
              </span>
            </div>
            <div
              className="max-h-48 overflow-y-auto divide-y"
              style={{ borderColor: 'var(--border-primary)' }}
            >
              {trajectories.map((t) => (
                <div
                  key={t.id}
                  className="flex items-center justify-between px-3 py-2 text-xs"
                  style={{ color: 'var(--text-secondary)' }}
                >
                  <div className="flex items-center gap-2 min-w-0">
                    {t.completed ? (
                      <CheckCircle size={11} style={{ color: 'var(--color-success)' }} />
                    ) : (
                      <XCircle size={11} style={{ color: 'var(--color-error)' }} />
                    )}
                    <span
                      className="font-mono truncate"
                      style={{ maxWidth: '80px', color: 'var(--text-tertiary)' }}
                    >
                      {t.id.slice(0, 8)}
                    </span>
                    <span
                      className="rounded px-1.5 py-0.5 text-[10px]"
                      style={{ background: 'var(--bg-hover)', color: 'var(--text-secondary)' }}
                    >
                      {t.model || 'unknown'}
                    </span>
                  </div>
                  <div className="flex items-center gap-3 shrink-0">
                    <span title="Tokens">{formatNumber(t.token_usage)} tok</span>
                    <span title="Tool calls">{t.tool_call_count} tools</span>
                    <span title="Duration">{(t.duration_ms / 1000).toFixed(1)}s</span>
                  </div>
                </div>
              ))}
            </div>
          </div>
        )}

        {loadingTraj && trajectories.length === 0 && (
          <div className="py-4 text-center text-xs" style={{ color: 'var(--text-tertiary)' }}>
            <RefreshCw size={16} className="mx-auto mb-1 animate-spin" />
            加载中...
          </div>
        )}

        {!loadingTraj && trajectories.length === 0 && stats?.total === 0 && (
          <div className="py-4 text-center text-xs" style={{ color: 'var(--text-tertiary)' }}>
            <BarChart3 size={20} className="mx-auto mb-1" />
            尚无轨迹数据。运行对话后将自动保存。
          </div>
        )}
      </section>

      {/* ── Section 2: Background Review ── */}
      <section>
        <div className="flex items-center gap-2 mb-3">
          <Search size={14} style={{ color: 'var(--accent)' }} />
          <h3 className="text-sm font-semibold" style={{ color: 'var(--text-primary)' }}>
            后台审查
          </h3>
        </div>

        <p className="text-xs mb-3" style={{ color: 'var(--text-secondary)' }}>
          对最近的运行进行回顾审查，自动提取有价值的记忆和技能。
        </p>

        <button
          onClick={runReview}
          disabled={reviewing}
          className="flex w-full items-center justify-center gap-2 rounded-lg py-2.5 text-xs font-medium transition-colors"
          style={{
            background: reviewing ? 'var(--border-primary)' : 'var(--action-run)',
            color: reviewing ? 'var(--text-tertiary)' : 'var(--text-on-run)',
          }}
        >
          {reviewing ? (
            <>
              <RefreshCw size={12} className="animate-spin" /> 审查中...
            </>
          ) : (
            <>
              <Search size={12} /> 立即审查
            </>
          )}
        </button>

        {reviewError && (
          <div
            className="mt-2 rounded-lg px-3 py-2 text-xs"
            style={{
              background: 'var(--color-error-bg, rgba(239,68,68,0.1))',
              color: 'var(--color-error)',
            }}
          >
            {reviewError}
          </div>
        )}

        {reviewResult && (
          <div
            className="mt-2 rounded-lg border p-3"
            style={{ borderColor: 'var(--border-primary)' }}
          >
            <div className="flex items-center gap-2 mb-2">
              {reviewResult.nothing_to_save ? (
                <Archive size={12} style={{ color: 'var(--text-tertiary)' }} />
              ) : (
                <CheckCircle size={12} style={{ color: 'var(--color-success)' }} />
              )}
              <span className="text-xs font-medium" style={{ color: 'var(--text-primary)' }}>
                运行: <span className="font-mono">{reviewResult.run_id.slice(0, 12)}</span>
              </span>
            </div>
            {reviewResult.nothing_to_save ? (
              <p className="text-xs" style={{ color: 'var(--text-tertiary)' }}>
                没有发现需要保存的内容。
              </p>
            ) : (
              <ul className="space-y-1">
                {reviewResult.actions.map((action, i) => (
                  <li
                    key={i}
                    className="text-xs flex items-start gap-1.5"
                    style={{ color: 'var(--text-secondary)' }}
                  >
                    <span style={{ color: 'var(--accent)' }}>-</span>
                    {action}
                  </li>
                ))}
              </ul>
            )}
            {reviewResult.error && (
              <p className="mt-1 text-[10px]" style={{ color: 'var(--color-warning, orange)' }}>
                Warning: {reviewResult.error}
              </p>
            )}
          </div>
        )}
      </section>

      {/* ── Section 3: Skill Curator ── */}
      <section>
        <div className="flex items-center justify-between mb-3">
          <div className="flex items-center gap-2">
            <Sparkles size={14} style={{ color: 'var(--accent)' }} />
            <h3 className="text-sm font-semibold" style={{ color: 'var(--text-primary)' }}>
              技能策展
            </h3>
          </div>
          <span className="text-[10px]" style={{ color: 'var(--text-tertiary)' }}>
            Active → Stale → Archived
          </span>
        </div>

        {curatorStatus && (
          <div className="grid grid-cols-5 gap-2 mb-3">
            <StatCard label="总计" value={curatorStatus.total} />
            <StatCard label="活跃" value={curatorStatus.active} color="var(--color-success)" />
            <StatCard
              label="过时"
              value={curatorStatus.stale}
              color="var(--color-warning, orange)"
            />
            <StatCard label="归档" value={curatorStatus.archived} color="var(--text-tertiary)" />
            <StatCard label="已固定" value={curatorStatus.pinned} icon={<Pin size={10} />} />
          </div>
        )}

        {curatorStatus?.last_run_at && (
          <div
            className="flex items-center gap-1.5 mb-3 text-[10px]"
            style={{ color: 'var(--text-tertiary)' }}
          >
            <Clock size={10} />
            上次运行: {new Date(curatorStatus.last_run_at).toLocaleString()}
          </div>
        )}

        <button
          onClick={runCurator}
          disabled={curatorLoading}
          className="flex w-full items-center justify-center gap-2 rounded-lg py-2.5 text-xs font-medium transition-colors"
          style={{
            background: curatorLoading ? 'var(--border-primary)' : 'var(--action-run)',
            color: curatorLoading ? 'var(--text-tertiary)' : 'var(--text-on-run)',
          }}
        >
          {curatorLoading ? (
            <>
              <RefreshCw size={12} className="animate-spin" /> 运行中...
            </>
          ) : (
            <>
              <Play size={12} /> 运行策展
            </>
          )}
        </button>

        {curatorMsg && (
          <div
            className="mt-2 rounded-lg px-3 py-2 text-xs"
            style={{
              background: curatorMsg.startsWith('Error')
                ? 'var(--color-error-bg, rgba(239,68,68,0.1))'
                : 'var(--accent-bg)',
              color: curatorMsg.startsWith('Error') ? 'var(--color-error)' : 'var(--accent)',
            }}
          >
            {curatorMsg}
          </div>
        )}

        {transitions.length > 0 && (
          <div className="mt-2 rounded-lg border" style={{ borderColor: 'var(--border-primary)' }}>
            <div className="border-b px-3 py-2" style={{ borderColor: 'var(--border-primary)' }}>
              <span className="text-[11px] font-medium" style={{ color: 'var(--text-secondary)' }}>
                状态变更 ({transitions.length})
              </span>
            </div>
            <div className="divide-y" style={{ borderColor: 'var(--border-primary)' }}>
              {transitions.map((t, i) => (
                <div
                  key={i}
                  className="flex items-center justify-between px-3 py-2 text-xs"
                  style={{ color: 'var(--text-secondary)' }}
                >
                  <span className="font-medium" style={{ color: 'var(--text-primary)' }}>
                    {t.skill}
                  </span>
                  <span className="font-mono text-[10px]">
                    {t.from} → {t.to}
                  </span>
                </div>
              ))}
            </div>
          </div>
        )}

        {!curatorStatus && (
          <div className="py-4 text-center text-xs" style={{ color: 'var(--text-tertiary)' }}>
            <Sparkles size={20} className="mx-auto mb-1" />
            尚无策展数据。
          </div>
        )}
      </section>
    </div>
  );
}

// ── Helpers ──

function StatCard({
  label,
  value,
  color,
  icon,
}: {
  label: string;
  value: number | string;
  color?: string;
  icon?: React.ReactNode;
}) {
  return (
    <div className="rounded-lg p-2 text-center" style={{ background: 'var(--bg-hover)' }}>
      <div
        className="flex items-center justify-center gap-1"
        style={{ color: 'var(--text-tertiary)' }}
      >
        {icon}
        <span className="text-[10px]">{label}</span>
      </div>
      <div className="text-sm font-semibold" style={{ color: color ?? 'var(--text-primary)' }}>
        {value}
      </div>
    </div>
  );
}

function formatNumber(n: number): string {
  if (n >= 1_000_000) return `${(n / 1_000_000).toFixed(1)}M`;
  if (n >= 1_000) return `${(n / 1_000).toFixed(1)}K`;
  return String(n);
}
