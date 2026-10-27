import { useEffect, useState } from 'react';
import { configApi } from '../../api/endpoints';
import { useUiStore } from '../../stores/uiStore';
import type { AgentConfigResponse, FullConfigResponse } from '../../generated';
import type { FullConfigUpdateRequest } from '../../types/api';

// Validate the wire value without hand-editing Rust-generated contracts. The
// generated response gains this field only when the authorized Rust export runs.
function readMaxSubagentDepth(config: unknown): 1 | 2 | 3 {
  if (typeof config === 'object' && config !== null && 'max_subagent_depth' in config) {
    const value = config.max_subagent_depth;
    if (value === 1 || value === 2 || value === 3) return value;
  }
  throw new Error('配置缺少有效的最大子智能体层数（1、2 或 3）');
}

export function ConfigPanel() {
  const [agentConfig, setAgentConfig] = useState<AgentConfigResponse | null>(null);
  const [fullConfig, setFullConfig] = useState<FullConfigResponse | null>(null);
  const [edit, setEdit] = useState<FullConfigUpdateRequest>({});
  const [dirty, setDirty] = useState(false);
  const [saving, setSaving] = useState(false);
  const [message, setMessage] = useState('');
  const [advancedOpen, setAdvancedOpen] = useState(false);
  const [loadError, setLoadError] = useState<string | null>(null);

  const load = async () => {
    setLoadError(null);
    try {
      const [agent, full] = await Promise.all([configApi.get(), configApi.getFull()]);
      readMaxSubagentDepth(full);
      setAgentConfig(agent);
      setFullConfig(full);
    } catch (e) {
      console.error('[ConfigPanel] failed to load config:', e);
      setLoadError(e instanceof Error ? e.message : String(e));
    }
  };

  useEffect(() => {
    load();
  }, []);

  const markDirty = (update: Partial<FullConfigUpdateRequest>) => {
    setEdit((prev) => ({ ...prev, ...update }));
    setDirty(true);
    setMessage('');
  };

  const save = async () => {
    setSaving(true);
    setMessage('');
    const controller = new AbortController();
    const timeout = setTimeout(() => controller.abort(), 15_000);

    try {
      // Persist all changes to YAML config file. The backend transaction also
      // syncs system_prompt and max_iterations to the running agents (primary
      // + pools), so no second update_config round-trip is needed here.
      const updatedFull = await configApi.updateFull(edit, controller.signal);
      const savedDepth = readMaxSubagentDepth(updatedFull);
      if (edit.max_subagent_depth !== undefined && savedDepth !== edit.max_subagent_depth) {
        throw new Error('层数配置未按请求保存');
      }
      setFullConfig(updatedFull);
      setEdit({});
      setDirty(false);

      // Refresh the running-agent projection (configApi.get) for display.
      try {
        setAgentConfig(await configApi.get());
      } catch (e) {
        console.error('[ConfigPanel] failed to refresh running agent config:', e);
      }

      setMessage('已保存');
      setTimeout(() => setMessage(''), 2000);
    } catch (e: any) {
      if (e?.name === 'AbortError') {
        setMessage('保存超时，请重试');
      } else {
        setMessage('保存失败');
      }
      console.error(e);
    } finally {
      clearTimeout(timeout);
      setSaving(false);
    }
  };

  if (loadError)
    return (
      <div className="p-3 text-sm" style={{ color: 'var(--color-error)' }}>
        配置加载失败：{loadError}
        <button onClick={load} className="ml-2 underline">
          重试
        </button>
      </div>
    );

  if (!agentConfig || !fullConfig)
    return <div className="p-3 text-sm text-[var(--text-tertiary)]">加载中...</div>;

  return (
    <div className="settings-basic space-y-7">
      {/* Header */}
      <div className="settings-panel-toolbar flex flex-wrap items-center justify-between gap-3">
        <div className="flex items-center gap-2">
          <h3 className="text-sm font-semibold text-[var(--text-primary)]">基础配置</h3>
          {dirty && (
            <span className="rounded-md bg-[var(--bg-secondary)] px-2 py-1 text-xs text-[var(--text-secondary)]">
              未保存
            </span>
          )}
        </div>
        <div className="flex items-center gap-2">
          {message && (
            <span
              role="status"
              className="text-sm"
              style={{
                color:
                  message === '已保存' ? 'var(--color-success-text)' : 'var(--color-error-text)',
              }}
            >
              {message}
            </span>
          )}
          <button
            onClick={save}
            disabled={saving || !dirty}
            className="btn btn-primary min-h-9 disabled:cursor-not-allowed disabled:opacity-40"
          >
            {saving ? '保存中...' : '保存'}
          </button>
        </div>
      </div>

      {/* Prompt reads the live Agent; depth reads committed product config. */}
      <Section title="智能体">
        <div className="settings-config-field space-y-2">
          <label
            htmlFor="max-subagent-depth"
            className="text-sm font-medium text-[var(--text-primary)]"
          >
            最大子智能体层数
          </label>
          <select
            id="max-subagent-depth"
            disabled={saving}
            value={edit.max_subagent_depth ?? readMaxSubagentDepth(fullConfig)}
            onChange={(event) => {
              const value = Number(event.target.value);
              if (value === 1 || value === 2 || value === 3) {
                markDirty({ max_subagent_depth: value });
              }
            }}
            className="w-full rounded-lg border border-[var(--border-primary)] bg-[var(--bg-primary)] px-3 py-2 text-sm text-[var(--text-primary)]"
          >
            <option value="1">1 层：仅主智能体可以分工（默认）</option>
            <option value="2">2 层：允许子智能体继续分工</option>
            <option value="3">3 层：最多三层</option>
          </select>
          <p className="text-xs leading-relaxed text-[var(--text-secondary)]">
            主智能体为第 0 层。对新任务生效，已运行任务及其重试、恢复保持原上限。
          </p>
        </div>
        <Field
          label="系统提示词"
          value={edit.agent?.system_prompt ?? agentConfig.system_prompt}
          onChange={(v) => markDirty({ agent: { ...edit.agent, system_prompt: v } })}
          multiline
          rows={8}
          className="min-h-[200px] resize-y font-mono leading-relaxed"
        />
      </Section>

      <Section title="高级/调试">
        <button
          type="button"
          onClick={() => setAdvancedOpen((open) => !open)}
          aria-expanded={advancedOpen}
          className="min-h-9 rounded-lg px-3 py-2 text-left text-sm text-[var(--text-secondary)] transition-colors hover:bg-[var(--bg-hover)]"
        >
          {advancedOpen ? '收起高级运行参数' : '展开高级运行参数'}
        </button>
        {advancedOpen && (
          <div className="mt-3 space-y-3 rounded-lg bg-[var(--bg-secondary)] p-4">
            <Field
              label="单轮推理安全上限"
              value={String(edit.agent?.max_iterations ?? agentConfig.max_iterations)}
              onChange={(v) => markDirty({ agent: { ...edit.agent, max_iterations: Number(v) } })}
              type="number"
            />
            <p className="text-xs leading-relaxed text-[var(--text-secondary)]">
              仅用于调试或防止异常循环。0 表示不限制，长程 CoWork 任务建议保持 0。
            </p>
          </div>
        )}
      </Section>

      {/* MCP */}
      <Section title="MCP">
        <Field
          label="配置路径"
          value={edit.mcp?.config_path ?? fullConfig.mcp.config_path ?? ''}
          onChange={(v) => markDirty({ mcp: { config_path: v } })}
        />
      </Section>

      <Section title="IM 渠道">
        <p className="text-xs leading-relaxed text-[var(--text-secondary)]">
          QQ、飞书和个人微信的配置与连接统一在 IM 渠道管理。
        </p>
        <button
          type="button"
          onClick={() => useUiStore.getState().setActiveSettingsTab('channels')}
          className="inline-flex min-h-9 items-center text-sm font-medium text-[var(--accent)] underline-offset-4 hover:underline"
        >
          前往IM渠道
        </button>
      </Section>

      {/* Session */}
      <Section title="会话">
        <Field
          label="超时（分钟）"
          value={String(
            edit.channels?.session?.timeout_minutes ?? fullConfig.channels.session.timeout_minutes
          )}
          onChange={(v) =>
            markDirty({
              channels: {
                ...edit.channels,
                session: { ...edit.channels?.session, timeout_minutes: Number(v) },
              },
            })
          }
          type="number"
        />
      </Section>

      {/* Logging */}
      <Section title="日志">
        <Field
          label="级别"
          value={edit.logging?.level ?? fullConfig.logging.level}
          onChange={(v) => markDirty({ logging: { level: v } })}
        />
      </Section>
    </div>
  );
}

function Section({ title, children }: { title: string; children: React.ReactNode }) {
  return (
    <section className="settings-config-section border-b border-[var(--border-secondary)] pb-7 last:border-0 last:pb-0">
      <h4 className="mb-4 text-sm font-semibold text-[var(--text-primary)]">{title}</h4>
      <div className="space-y-5">{children}</div>
    </section>
  );
}

function Field({
  label,
  value,
  onChange,
  multiline,
  type,
  rows,
  className,
}: {
  label: string;
  value: string;
  onChange: (v: string) => void;
  multiline?: boolean;
  type?: string;
  rows?: number;
  className?: string;
}) {
  const cls = 'input w-full text-sm';
  const controlClassName = `${cls} ${className ?? ''}`;
  return (
    <label className="settings-config-field block space-y-2">
      <span className="block text-sm font-medium text-[var(--text-primary)]">{label}</span>
      {multiline ? (
        <textarea
          value={value}
          onChange={(e) => onChange(e.target.value)}
          className={controlClassName}
          rows={rows ?? 3}
          spellCheck={false}
        />
      ) : (
        <input
          type={type}
          value={value}
          onChange={(e) => onChange(e.target.value)}
          className={controlClassName}
        />
      )}
    </label>
  );
}
