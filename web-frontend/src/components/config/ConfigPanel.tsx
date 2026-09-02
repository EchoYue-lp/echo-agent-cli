import { useEffect, useState } from 'react';
import { configApi } from '../../api/endpoints';
import type { AgentConfigResponse, FullConfigResponse } from '../../generated';
import type { FullConfigUpdateRequest } from '../../types/api';

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
    <div className="space-y-4 p-3">
      {/* Header */}
      <div className="flex items-center justify-between">
        <div className="flex items-center gap-2">
          <h3 className="text-sm font-semibold text-[var(--text-primary)]">配置</h3>
          {dirty && (
            <span className="rounded-full bg-amber-500 px-2 py-0.5 text-[10px] font-medium text-white">
              未保存
            </span>
          )}
        </div>
        <div className="flex items-center gap-2">
          {message && <span className="text-xs text-emerald-500">{message}</span>}
          <button
            onClick={save}
            disabled={saving || !dirty}
            className="rounded-lg bg-[var(--accent)] px-3 py-1 text-xs font-medium text-[var(--text-on-accent)] transition-colors hover:opacity-90 disabled:cursor-not-allowed disabled:opacity-30"
          >
            {saving ? '保存中...' : '保存'}
          </button>
        </div>
      </div>

      {/* Agent — reads from running agent (configApi.get), not YAML */}
      <Section title="智能体">
        <Field
          label="系统提示词"
          value={edit.agent?.system_prompt ?? agentConfig.system_prompt}
          onChange={(v) => markDirty({ agent: { ...edit.agent, system_prompt: v } })}
          multiline
          rows={16}
          className="min-h-[320px] resize-y font-mono leading-relaxed"
        />
      </Section>

      <Section title="高级/调试">
        <button
          type="button"
          onClick={() => setAdvancedOpen((open) => !open)}
          className="w-full rounded-lg border border-[var(--border-primary)] bg-[var(--bg-primary)] px-3 py-2 text-left text-xs font-medium text-[var(--text-secondary)] transition-colors hover:bg-[var(--bg-hover)]"
        >
          {advancedOpen ? '收起高级运行参数' : '展开高级运行参数'}
        </button>
        {advancedOpen && (
          <div className="space-y-2 rounded-lg border border-[var(--border-secondary)] bg-[var(--bg-secondary)] p-3">
            <Field
              label="单轮推理安全上限"
              value={String(edit.agent?.max_iterations ?? agentConfig.max_iterations)}
              onChange={(v) => markDirty({ agent: { ...edit.agent, max_iterations: Number(v) } })}
              type="number"
            />
            <p className="text-[11px] leading-relaxed text-[var(--text-tertiary)]">
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

      {/* Channels - QQ */}
      <Section title="QQ 机器人">
        <Toggle
          label="启用"
          value={edit.channels?.qq?.enabled ?? fullConfig.channels.qq.enabled}
          onChange={(v) =>
            markDirty({
              channels: { ...edit.channels, qq: { ...edit.channels?.qq, enabled: v } },
            })
          }
        />
        <Field
          label="应用 ID"
          value={edit.channels?.qq?.app_id ?? fullConfig.channels.qq.app_id}
          onChange={(v) =>
            markDirty({
              channels: { ...edit.channels, qq: { ...edit.channels?.qq, app_id: v } },
            })
          }
        />
        <Field
          label="客户端密钥"
          value={edit.channels?.qq?.client_secret ?? ''}
          onChange={(v) =>
            markDirty({
              channels: { ...edit.channels, qq: { ...edit.channels?.qq, client_secret: v } },
            })
          }
          type="password"
        />
      </Section>

      {/* Channels - Feishu */}
      <Section title="飞书">
        <Toggle
          label="启用"
          value={edit.channels?.feishu?.enabled ?? fullConfig.channels.feishu.enabled}
          onChange={(v) =>
            markDirty({
              channels: { ...edit.channels, feishu: { ...edit.channels?.feishu, enabled: v } },
            })
          }
        />
        <Field
          label="应用 ID"
          value={edit.channels?.feishu?.app_id ?? fullConfig.channels.feishu.app_id}
          onChange={(v) =>
            markDirty({
              channels: { ...edit.channels, feishu: { ...edit.channels?.feishu, app_id: v } },
            })
          }
        />
        <Field
          label="应用密钥"
          value={edit.channels?.feishu?.app_secret ?? ''}
          onChange={(v) =>
            markDirty({
              channels: { ...edit.channels, feishu: { ...edit.channels?.feishu, app_secret: v } },
            })
          }
          type="password"
        />
        <Field
          label="模式"
          value={edit.channels?.feishu?.mode ?? fullConfig.channels.feishu.mode}
          onChange={(v) =>
            markDirty({
              channels: { ...edit.channels, feishu: { ...edit.channels?.feishu, mode: v } },
            })
          }
        />
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
    <div className="rounded-lg border border-[var(--border-primary)] p-3">
      <h4 className="mb-2 text-xs font-semibold uppercase tracking-wider text-[var(--text-tertiary)]">
        {title}
      </h4>
      <div className="space-y-2">{children}</div>
    </div>
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
  const cls =
    'w-full rounded-lg border border-[var(--border-primary)] bg-[var(--bg-input)] px-2 py-1.5 text-sm text-[var(--text-primary)] outline-none focus:border-[var(--border-focus)]';
  const controlClassName = `${cls} ${className ?? ''}`;
  return (
    <div>
      <label className="mb-1 block text-xs text-[var(--text-secondary)]">{label}</label>
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
    </div>
  );
}

function Toggle({
  label,
  value,
  onChange,
}: {
  label: string;
  value: boolean;
  onChange: (v: boolean) => void;
}) {
  return (
    <div className="flex items-center justify-between">
      <span className="text-sm text-[var(--text-primary)]">{label}</span>
      <button
        onClick={() => onChange(!value)}
        className={`relative h-5 w-9 rounded-full transition ${value ? 'bg-[var(--accent)]' : 'bg-[var(--text-tertiary)]'}`}
      >
        <span
          className={`absolute top-0.5 h-4 w-4 rounded-full bg-white transition ${value ? 'left-[18px]' : 'left-0.5'}`}
        />
      </button>
    </div>
  );
}
