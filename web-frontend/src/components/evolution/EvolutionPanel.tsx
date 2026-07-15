import { useEffect, useState } from 'react';
import {
  Sparkles,
  Search,
  RefreshCw,
  Pin,
  Play,
  CheckCircle,
  Archive,
  Clock,
  Database,
  Heart,
  Gift,
  Wand2,
  Zap,
} from 'lucide-react';
import { evolutionApi } from '../../api/endpoints';
import { useToastStore } from '../../stores/toastStore';
import type {
  CuratorStatus,
  CuratorTransition,
  DashboardMetrics,
  RuleProposal,
  SkillCandidateInfo,
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

  // ── Skill candidates state(技能自动创建:检测 → 草稿 → 激活)
  const [skillCandidates, setSkillCandidates] = useState<SkillCandidateInfo[]>([]);
  const [loadingCandidates, setLoadingCandidates] = useState(false);
  const [actingOnSkill, setActingOnSkill] = useState<string | null>(null);

  // ── Review state
  const [reviewing, setReviewing] = useState(false);
  const [reviewResult, setReviewResult] = useState<{
    success: boolean;
    run_id: string;
    actions: string[];
    nothing_to_save: boolean;
    candidate?: {
      kind: 'user_preference' | 'project_fact' | 'debugging_lesson' | 'skill';
      content: string;
      evidence: string;
      confidence: number;
      persisted: boolean;
    } | null;
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

  // ── Skill candidates(技能自动创建闭环)
  const loadSkillCandidates = async () => {
    setLoadingCandidates(true);
    try {
      const data = await evolutionApi.scanSkillCandidates();
      setSkillCandidates(data.candidates);
    } catch (e) {
      console.error('Failed to load skill candidates:', e);
    }
    setLoadingCandidates(false);
  };

  // review gate:用户点「生成草稿」才写 _drafts/<name>/SKILL.md
  const handleGenerateDraft = async (name: string) => {
    setActingOnSkill(name);
    try {
      await evolutionApi.generateSkillDraft(name);
      addToast('success', `已为「${name}」生成草稿 SKILL.md`);
      await loadSkillCandidates();
    } catch (e) {
      addToast('error', `生成草稿失败: ${e instanceof Error ? e.message : '未知错误'}`);
    }
    setActingOnSkill(null);
  };

  // review gate:用户点「激活」才把草稿复制到正式 skills/ + curator Draft→Active
  const handleActivateSkill = async (name: string) => {
    setActingOnSkill(name);
    try {
      await evolutionApi.activateSkillDraft(name);
      addToast('success', `技能「${name}」已激活,下次加载时生效`);
      await loadSkillCandidates();
    } catch (e) {
      addToast('error', `激活失败: ${e instanceof Error ? e.message : '未知错误'}`);
    }
    setActingOnSkill(null);
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
    loadSkillCandidates();
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
              <StatCard
                label="总记忆"
                value={dashboard.total_memories}
                icon={<Database size={10} />}
              />
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
              <div className="rounded-lg border" style={{ borderColor: 'var(--border-primary)' }}>
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
                        className="rounded-md px-1.5 py-0.5 text-[9px] shrink-0"
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
                      className="rounded-md px-1.5 py-0.5 text-[9px] shrink-0"
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
                    className="flex items-center gap-1 rounded-md px-2 py-1 text-[10px] font-medium transition-colors shrink-0"
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

      {/* ── Section 0c: Skill Candidates (技能自动创建闭环) ── */}
      <section>
        <div className="flex items-center justify-between mb-3">
          <div className="flex items-center gap-2">
            <Wand2 size={14} style={{ color: 'var(--accent)' }} />
            <h3 className="text-sm font-semibold" style={{ color: 'var(--text-primary)' }}>
              技能候选
            </h3>
            {skillCandidates.length > 0 && (
              <span
                className="rounded-full px-1.5 py-0.5 text-[9px] font-medium"
                style={{ background: 'var(--accent-bg)', color: 'var(--accent)' }}
              >
                {skillCandidates.length}
              </span>
            )}
          </div>
          <button
            onClick={loadSkillCandidates}
            disabled={loadingCandidates}
            className="flex items-center gap-1 text-[10px] font-medium transition-colors"
            style={{ color: 'var(--accent)' }}
          >
            <RefreshCw size={10} className={loadingCandidates ? 'animate-spin' : ''} />
            刷新
          </button>
        </div>

        <p className="text-xs mb-3" style={{ color: 'var(--text-secondary)' }}>
          系统从重复工作流/调试经验中检测出可复用模式。生成草稿 → 审阅 → 激活后, 技能进入正式 skills
          目录,下次加载时自动生效。
        </p>

        {skillCandidates.length > 0 ? (
          <div className="space-y-2">
            {skillCandidates.map((c) => (
              <div
                key={c.name}
                className="rounded-lg border p-3"
                style={{ borderColor: 'var(--border-primary)' }}
              >
                <div className="flex items-center justify-between mb-1.5">
                  <div className="flex items-center gap-2 min-w-0">
                    <span
                      className="text-xs font-medium truncate"
                      style={{ color: 'var(--text-primary)' }}
                    >
                      {c.name}
                    </span>
                    {c.activated && (
                      <span
                        className="flex items-center gap-0.5 rounded-md px-1 py-0.5 text-[9px] shrink-0"
                        style={{
                          background: 'var(--color-success-bg, rgba(34,197,94,0.1))',
                          color: 'var(--color-success)',
                        }}
                      >
                        <CheckCircle size={8} /> 已激活
                      </span>
                    )}
                  </div>
                  <div
                    className="flex items-center gap-2 shrink-0 text-[10px]"
                    style={{ color: 'var(--text-tertiary)' }}
                  >
                    <span title="观测样本数">{c.sample_count} 次</span>
                    <span
                      className="rounded-md px-1 py-0.5"
                      style={{ background: 'var(--bg-hover)' }}
                    >
                      {c.source_type}
                    </span>
                  </div>
                </div>
                <p className="text-xs mb-2" style={{ color: 'var(--text-secondary)' }}>
                  {c.description}
                </p>
                <div className="flex items-center justify-end gap-2">
                  {/* 草稿状态决定按钮:未生成→生成草稿;已生成未激活→激活;已激活→禁用 */}
                  {!c.has_draft && !c.activated && (
                    <button
                      onClick={() => handleGenerateDraft(c.name)}
                      disabled={actingOnSkill !== null}
                      className="flex items-center gap-1 rounded-md px-2 py-1 text-[10px] font-medium transition-colors"
                      style={{
                        background:
                          actingOnSkill === c.name ? 'var(--border-primary)' : 'var(--bg-hover)',
                        color:
                          actingOnSkill === c.name ? 'var(--text-tertiary)' : 'var(--text-primary)',
                      }}
                    >
                      {actingOnSkill === c.name ? (
                        <RefreshCw size={9} className="animate-spin" />
                      ) : (
                        <Wand2 size={9} />
                      )}
                      生成草稿
                    </button>
                  )}
                  {c.has_draft && !c.activated && (
                    <button
                      onClick={() => handleActivateSkill(c.name)}
                      disabled={actingOnSkill !== null}
                      className="flex items-center gap-1 rounded-md px-2 py-1 text-[10px] font-medium transition-colors"
                      style={{
                        background:
                          actingOnSkill === c.name ? 'var(--border-primary)' : 'var(--action-run)',
                        color:
                          actingOnSkill === c.name ? 'var(--text-tertiary)' : 'var(--text-on-run)',
                      }}
                    >
                      {actingOnSkill === c.name ? (
                        <RefreshCw size={9} className="animate-spin" />
                      ) : (
                        <Zap size={9} />
                      )}
                      激活
                    </button>
                  )}
                  {c.activated && (
                    <span className="text-[10px]" style={{ color: 'var(--text-tertiary)' }}>
                      下次加载生效
                    </span>
                  )}
                </div>
              </div>
            ))}
          </div>
        ) : (
          <div className="py-4 text-center text-xs" style={{ color: 'var(--text-tertiary)' }}>
            {loadingCandidates ? (
              <RefreshCw size={16} className="mx-auto mb-1 animate-spin" />
            ) : (
              <Wand2 size={20} className="mx-auto mb-1" />
            )}
            {loadingCandidates
              ? '加载中...'
              : '暂无技能候选。重复工作流/调试经验积累 ≥ 3 次后,经 review 会出现在此。'}
          </div>
        )}
      </section>

      {/* ── Run Review ── */}
      <section>
        <div className="flex items-center gap-2 mb-3">
          <Search size={14} style={{ color: 'var(--accent)' }} />
          <h3 className="text-sm font-semibold" style={{ color: 'var(--text-primary)' }}>
            运行回顾
          </h3>
        </div>

        <p className="text-xs mb-3" style={{ color: 'var(--text-secondary)' }}>
          对最近一次运行生成带证据的长期记忆候选，默认不会自动写入记忆。
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
            {reviewResult.candidate && (
              <div
                className="mt-2 rounded-md px-2.5 py-2 text-[11px]"
                style={{ background: 'var(--bg-hover)', color: 'var(--text-secondary)' }}
              >
                <div>
                  类型: <span className="font-mono">{reviewResult.candidate.kind}</span>
                </div>
                <div className="mt-1">证据: {reviewResult.candidate.evidence}</div>
                <div className="mt-1">
                  置信度: {reviewResult.candidate.confidence.toFixed(2)} ·{' '}
                  {reviewResult.candidate.persisted ? '已保存为草稿记忆' : '未自动保存'}
                </div>
              </div>
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
