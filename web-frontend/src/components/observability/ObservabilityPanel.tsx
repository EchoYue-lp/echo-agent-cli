import { useEffect, useMemo, useState, type ReactNode } from 'react';
import {
  Activity,
  AlertTriangle,
  Cpu,
  Gauge,
  RefreshCw,
  Server,
  Stethoscope,
  TrendingUp,
} from 'lucide-react';
import { traceEventsApi, type TraceEvent, type TraceSummary } from '../../api/endpoints';
import { TraceTimeline } from './TraceTimeline';
import { TokenUsageChart } from './TokenUsageChart';
import { CacheDiagnosticsPanel } from './CacheDiagnosticsPanel';
import { UsageTrendsPanel } from './UsageTrendsPanel';

type WindowId = '1h' | '24h' | '7d' | 'all';

interface SessionSnapshot {
  sessionId: string;
  summary: TraceSummary;
  events: TraceEvent[];
}

interface UsageAggregate {
  calls: number;
  input: number;
  output: number;
  cached: number;
  cacheWrite: number;
  missingUsage: number;
}

interface ModelAggregate extends UsageAggregate {
  model: string;
}

const WINDOWS: Array<{ id: WindowId; label: string; ms?: number }> = [
  { id: '1h', label: '1h', ms: 60 * 60 * 1000 },
  { id: '24h', label: '24h', ms: 24 * 60 * 60 * 1000 },
  { id: '7d', label: '7d', ms: 7 * 24 * 60 * 60 * 1000 },
  { id: 'all', label: 'All' },
];

function emptyUsage(): UsageAggregate {
  return {
    calls: 0,
    input: 0,
    output: 0,
    cached: 0,
    cacheWrite: 0,
    missingUsage: 0,
  };
}

function addUsage(target: UsageAggregate, event: TraceEvent) {
  if (event.kind.type !== 'llm_call') return;
  target.calls += 1;
  target.input += event.kind.input_tokens;
  target.output += event.kind.output_tokens;
  target.cached += event.kind.cached_input_tokens;
  target.cacheWrite += event.kind.cache_creation_input_tokens;
  if (!event.kind.usage_reported) target.missingUsage += 1;
}

function readRate(usage: UsageAggregate): number | null {
  return usage.input > 0 ? usage.cached / usage.input : null;
}

function formatRate(value: number | null): string {
  return value == null ? 'unknown' : `${(value * 100).toFixed(1)}%`;
}

function windowCutoff(windowId: WindowId): number | null {
  const window = WINDOWS.find((item) => item.id === windowId);
  return window?.ms ? Date.now() - window.ms : null;
}

function eventTimeMs(event: TraceEvent): number {
  const parsed = Date.parse(event.timestamp);
  return Number.isFinite(parsed) ? parsed : 0;
}

function diagnosticMessages(total: UsageAggregate, models: ModelAggregate[]): string[] {
  const messages: string[] = [];
  const rate = readRate(total);
  if (total.calls === 0) {
    return ['当前时间窗口没有 LLM usage 数据。先运行一次任务或选择更大的时间窗口。'];
  }
  if (total.missingUsage > 0) {
    messages.push(`${total.missingUsage} 次请求缺少 provider usage，缓存命中率可能被低估。`);
  }
  if (models.length > 1) {
    messages.push(`时间窗口内出现 ${models.length} 个模型；不同模型通常不会共享 prompt cache。`);
  }
  if (rate != null && rate < 0.2 && total.input >= 1000) {
    messages.push(
      'cache read rate 偏低。优先检查 system prefix、tools 定义、cwd/记忆/hook 注入和 subagent prompt 是否稳定。'
    );
  }
  if (total.cacheWrite > total.cached && total.cacheWrite > 0) {
    messages.push(
      'cache write 高于 cache read，说明更多是在创建缓存；重复同类任务后 read 仍不上升才需要继续排查前缀稳定性。'
    );
  }
  if (messages.length === 0) {
    messages.push('当前缓存数据没有明显异常。继续观察相同模型、相同提示词下的 read rate 趋势。');
  }
  return messages;
}

export function ObservabilityPanel() {
  const [activeTab, setActiveTab] = useState<'overview' | 'diagnostics' | 'trends'>('overview');
  const [windowId, setWindowId] = useState<WindowId>('24h');
  const [sessions, setSessions] = useState<SessionSnapshot[]>([]);
  const [selectedSessionId, setSelectedSessionId] = useState<string | null>(null);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const load = async () => {
    setLoading(true);
    setError(null);
    try {
      const ids = await traceEventsApi.listSessions();
      const snapshots = await Promise.all(
        ids.map(async (sessionId) => {
          const [summary, events] = await Promise.all([
            traceEventsApi.getSummary(sessionId),
            traceEventsApi.getEvents(sessionId),
          ]);
          return { sessionId, summary, events };
        })
      );
      snapshots.sort((a, b) => {
        const lastA = Math.max(0, ...a.events.map(eventTimeMs));
        const lastB = Math.max(0, ...b.events.map(eventTimeMs));
        return lastB - lastA;
      });
      setSessions(snapshots);
      setSelectedSessionId((current) => current ?? snapshots[0]?.sessionId ?? null);
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err));
    } finally {
      setLoading(false);
    }
  };

  useEffect(() => {
    void load();
  }, []);

  const cutoff = windowCutoff(windowId);
  const filteredSessions = useMemo(
    () =>
      sessions
        .map((session) => ({
          ...session,
          events:
            cutoff == null
              ? session.events
              : session.events.filter((event) => eventTimeMs(event) >= cutoff),
        }))
        .filter((session) => session.events.length > 0),
    [sessions, cutoff]
  );

  const allEvents = useMemo(
    () => filteredSessions.flatMap((session) => session.events),
    [filteredSessions]
  );
  const totalUsage = useMemo(() => {
    const usage = emptyUsage();
    allEvents.forEach((event) => addUsage(usage, event));
    return usage;
  }, [allEvents]);

  const modelUsage = useMemo<ModelAggregate[]>(() => {
    const map = new Map<string, ModelAggregate>();
    allEvents.forEach((event) => {
      if (event.kind.type !== 'llm_call') return;
      const model = event.kind.model || 'unknown';
      const usage = map.get(model) ?? { model, ...emptyUsage() };
      addUsage(usage, event);
      map.set(model, usage);
    });
    return [...map.values()].sort((a, b) => b.input + b.output - (a.input + a.output));
  }, [allEvents]);

  const selectedSession =
    filteredSessions.find((session) => session.sessionId === selectedSessionId) ??
    filteredSessions[0] ??
    null;
  const diagnostics = diagnosticMessages(totalUsage, modelUsage);

  return (
    <div className="flex h-full min-h-0 flex-col">
      <div className="mb-4 flex items-center justify-between gap-3">
        <div>
          <h2 className="text-sm font-semibold" style={{ color: 'var(--text-primary)' }}>
            运行观测
          </h2>
          <p className="mt-1 text-xs" style={{ color: 'var(--text-tertiary)' }}>
            按时间窗口、模型和会话查看 token/cache 趋势。
          </p>
        </div>
        <button
          onClick={() => void load()}
          className="flex items-center gap-1 rounded-md px-2 py-1 text-xs"
          style={{ background: 'var(--bg-hover)', color: 'var(--text-secondary)' }}
        >
          <RefreshCw size={13} className={loading ? 'animate-spin' : ''} /> 刷新
        </button>
      </div>

      {/* Tab bar */}
      <div className="mb-4 flex gap-1 border-b" style={{ borderColor: 'var(--border-secondary)' }}>
        <button
          onClick={() => setActiveTab('overview')}
          className="rounded-t-md px-3 py-1.5 text-xs font-medium"
          style={{
            background: activeTab === 'overview' ? 'var(--bg-secondary)' : 'transparent',
            color: activeTab === 'overview' ? 'var(--text-primary)' : 'var(--text-tertiary)',
            borderBottom:
              activeTab === 'overview' ? `2px solid var(--accent)` : '2px solid transparent',
          }}
        >
          <Activity size={13} className="inline mr-1" /> 运行概览
        </button>
        <button
          onClick={() => setActiveTab('diagnostics')}
          className="rounded-t-md px-3 py-1.5 text-xs font-medium"
          style={{
            background: activeTab === 'diagnostics' ? 'var(--bg-secondary)' : 'transparent',
            color: activeTab === 'diagnostics' ? 'var(--text-primary)' : 'var(--text-tertiary)',
            borderBottom:
              activeTab === 'diagnostics' ? `2px solid var(--accent)` : '2px solid transparent',
          }}
        >
          <Stethoscope size={13} className="inline mr-1" /> 缓存诊断
        </button>
        <button
          onClick={() => setActiveTab('trends')}
          className="rounded-t-md px-3 py-1.5 text-xs font-medium"
          style={{
            background: activeTab === 'trends' ? 'var(--bg-secondary)' : 'transparent',
            color: activeTab === 'trends' ? 'var(--text-primary)' : 'var(--text-tertiary)',
            borderBottom:
              activeTab === 'trends' ? `2px solid var(--accent)` : '2px solid transparent',
          }}
        >
          <TrendingUp size={13} className="inline mr-1" /> Usage 趋势
        </button>
      </div>

      {activeTab === 'diagnostics' ? (
        <CacheDiagnosticsPanel />
      ) : activeTab === 'trends' ? (
        <UsageTrendsPanel />
      ) : (
        <>
          <div className="mb-4 flex flex-wrap gap-1">
            {WINDOWS.map((window) => (
              <button
                key={window.id}
                onClick={() => setWindowId(window.id)}
                className="rounded-md px-2 py-1 text-xs"
                style={{
                  background: windowId === window.id ? 'var(--accent)' : 'var(--bg-hover)',
                  color: windowId === window.id ? 'var(--text-on-accent)' : 'var(--text-secondary)',
                }}
              >
                {window.label}
              </button>
            ))}
          </div>

          {error && (
            <div
              className="mb-3 rounded-md px-3 py-2 text-xs"
              style={{ background: 'var(--bg-hover)', color: 'var(--color-error)' }}
            >
              {error}
            </div>
          )}

          <div className="grid grid-cols-4 gap-2">
            <Metric
              label="LLM calls"
              value={totalUsage.calls.toLocaleString()}
              icon={<Cpu size={14} />}
            />
            <Metric
              label="Input tokens"
              value={totalUsage.input.toLocaleString()}
              icon={<Activity size={14} />}
            />
            <Metric
              label="Cache read"
              value={formatRate(readRate(totalUsage))}
              icon={<Gauge size={14} />}
            />
            <Metric
              label="Missing usage"
              value={totalUsage.missingUsage.toLocaleString()}
              icon={<AlertTriangle size={14} />}
            />
          </div>

          <div className="mt-4 grid min-h-0 flex-1 grid-cols-[minmax(220px,280px)_1fr] gap-4">
            <section className="min-h-0 overflow-auto">
              <PanelTitle icon={<Server size={13} />} title="会话" />
              <div className="space-y-1.5">
                {filteredSessions.map((session) => {
                  const usage = emptyUsage();
                  session.events.forEach((event) => addUsage(usage, event));
                  return (
                    <button
                      key={session.sessionId}
                      onClick={() => setSelectedSessionId(session.sessionId)}
                      className="w-full rounded-md px-2 py-2 text-left"
                      style={{
                        background:
                          selectedSession?.sessionId === session.sessionId
                            ? 'var(--bg-hover)'
                            : 'var(--bg-secondary)',
                        color: 'var(--text-secondary)',
                      }}
                    >
                      <div
                        className="truncate text-xs font-medium"
                        style={{ color: 'var(--text-primary)' }}
                      >
                        {session.sessionId}
                      </div>
                      <div className="mt-1 text-[10px]" style={{ color: 'var(--text-tertiary)' }}>
                        {usage.calls} calls · {formatRate(readRate(usage))} cache read
                      </div>
                    </button>
                  );
                })}
                {filteredSessions.length === 0 && (
                  <div
                    className="rounded-md p-3 text-center text-xs"
                    style={{ color: 'var(--text-tertiary)', background: 'var(--bg-secondary)' }}
                  >
                    当前窗口暂无 trace 数据
                  </div>
                )}
              </div>
            </section>

            <section className="min-h-0 overflow-auto">
              <PanelTitle icon={<Gauge size={13} />} title="诊断建议" />
              <div className="mb-4 space-y-1.5">
                {diagnostics.map((message) => (
                  <div
                    key={message}
                    className="rounded-md px-3 py-2 text-xs"
                    style={{ background: 'var(--bg-secondary)', color: 'var(--text-secondary)' }}
                  >
                    {message}
                  </div>
                ))}
              </div>

              <PanelTitle icon={<Cpu size={13} />} title="模型对比" />
              <div
                className="mb-4 overflow-hidden rounded-lg border"
                style={{ borderColor: 'var(--border-primary)' }}
              >
                {modelUsage.map((model) => (
                  <div
                    key={model.model}
                    className="grid grid-cols-[1fr_80px_80px_80px] gap-2 border-b px-3 py-2 text-xs last:border-b-0"
                    style={{ borderColor: 'var(--border-primary)' }}
                  >
                    <span className="truncate font-mono" style={{ color: 'var(--text-primary)' }}>
                      {model.model}
                    </span>
                    <span style={{ color: 'var(--text-secondary)' }}>{model.calls} calls</span>
                    <span style={{ color: 'var(--text-secondary)' }}>
                      {formatRate(readRate(model))}
                    </span>
                    <span style={{ color: 'var(--text-tertiary)' }}>
                      {model.missingUsage} missing
                    </span>
                  </div>
                ))}
                {modelUsage.length === 0 && (
                  <div
                    className="px-3 py-4 text-center text-xs"
                    style={{ color: 'var(--text-tertiary)' }}
                  >
                    暂无模型 usage
                  </div>
                )}
              </div>

              {selectedSession && (
                <>
                  <PanelTitle icon={<Activity size={13} />} title="选中会话 usage" />
                  <div className="mb-4">
                    <TokenUsageChart events={selectedSession.events} />
                  </div>
                  <PanelTitle icon={<Activity size={13} />} title="事件时间线" />
                  <TraceTimeline events={selectedSession.events} />
                </>
              )}
            </section>
          </div>
        </>
      )}
    </div>
  );
}

function Metric({ label, value, icon }: { label: string; value: string; icon: ReactNode }) {
  return (
    <div className="rounded-lg p-3" style={{ background: 'var(--bg-secondary)' }}>
      <div
        className="mb-1 flex items-center gap-1.5 text-[10px]"
        style={{ color: 'var(--text-tertiary)' }}
      >
        {icon}
        {label}
      </div>
      <div
        className="truncate font-mono text-sm font-semibold"
        style={{ color: 'var(--text-primary)' }}
      >
        {value}
      </div>
    </div>
  );
}

function PanelTitle({ icon, title }: { icon: ReactNode; title: string }) {
  return (
    <div
      className="mb-2 flex items-center gap-1.5 text-xs font-medium"
      style={{ color: 'var(--text-primary)' }}
    >
      {icon}
      {title}
    </div>
  );
}
