import { useEffect, useState } from 'react';
import { configApi } from '../../api/endpoints';
import type { FullConfigResponse, FullConfigUpdateRequest } from '../../types/api';

export function ConfigPanel() {
  const [config, setConfig] = useState<FullConfigResponse | null>(null);
  const [edit, setEdit] = useState<FullConfigUpdateRequest>({});
  const [dirty, setDirty] = useState(false);
  const [saving, setSaving] = useState(false);
  const [message, setMessage] = useState('');

  useEffect(() => {
    configApi.getFull().then((c) => { setConfig(c); }).catch(console.error);
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
    const timeout = setTimeout(() => controller.abort(), 15_000); // 15s timeout
    try {
      const updated = await configApi.updateFull(edit, controller.signal);
      setConfig(updated);
      setEdit({});
      setDirty(false);
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

  if (!config) return <div className="p-3 text-sm text-[var(--text-tertiary)]">加载中...</div>;

  return (
    <div className="space-y-4 p-3">
      {/* Header */}
      <div className="flex items-center justify-between">
        <h3 className="text-sm font-semibold text-[var(--text-primary)]">配置</h3>
        <div className="flex items-center gap-2">
          {message && <span className="text-xs text-emerald-500">{message}</span>}
          {dirty && (
            <button
              onClick={save}
              disabled={saving}
              className="rounded-lg bg-[var(--accent)] px-3 py-1 text-xs font-medium text-white transition-colors hover:opacity-90 disabled:opacity-50"
            >
              {saving ? '保存中...' : '保存'}
            </button>
          )}
        </div>
      </div>

      {/* Model */}
      <Section title="模型">
        <SelectField label="模型名称"
          value={edit.model?.name ?? config.model.name}
          options={config.agent.available_models}
          onChange={(v) => markDirty({ model: { ...edit.model, name: v } })} />
        <Field label="最大令牌数" value={String(edit.model?.max_tokens ?? config.model.max_tokens ?? '')}
          onChange={(v) => markDirty({ model: { ...edit.model, max_tokens: v ? Number(v) : undefined } })} type="number" />
        <Field label="温度" value={String(edit.model?.temperature ?? config.model.temperature ?? '')}
          onChange={(v) => markDirty({ model: { ...edit.model, temperature: v ? Number(v) : undefined } })} type="number" />
      </Section>

      {/* Agent */}
      <Section title="智能体">
        <Field label="系统提示词" value={edit.agent?.system_prompt ?? config.agent.system_prompt}
          onChange={(v) => markDirty({ agent: { ...edit.agent, system_prompt: v } })} multiline />
        <Field label="最大迭代次数" value={String(edit.agent?.max_iterations ?? config.agent.max_iterations)}
          onChange={(v) => markDirty({ agent: { ...edit.agent, max_iterations: Number(v) } })} type="number" />
        <Toggle label="工具" value={edit.agent?.enable_tools ?? config.agent.enable_tools}
          onChange={(v) => markDirty({ agent: { ...edit.agent, enable_tools: v } })} />
        <Toggle label="记忆" value={edit.agent?.enable_memory ?? config.agent.enable_memory}
          onChange={(v) => markDirty({ agent: { ...edit.agent, enable_memory: v } })} />
        <Toggle label="人工介入" value={edit.agent?.enable_human_in_loop ?? config.agent.enable_human_loop}
          onChange={(v) => markDirty({ agent: { ...edit.agent, enable_human_in_loop: v } })} />
      </Section>

      {/* MCP */}
      <Section title="MCP">
        <Field label="配置路径" value={edit.mcp?.config_path ?? config.mcp.config_path ?? ''}
          onChange={(v) => markDirty({ mcp: { config_path: v } })} />
      </Section>

      {/* Channels - QQ */}
      <Section title="QQ 机器人">
        <Toggle label="启用" value={edit.channels?.qq?.enabled ?? config.channels.qq.enabled}
          onChange={(v) => markDirty({ channels: { ...edit.channels, qq: { ...edit.channels?.qq, enabled: v } } })} />
        <Field label="应用 ID" value={edit.channels?.qq?.app_id ?? config.channels.qq.app_id}
          onChange={(v) => markDirty({ channels: { ...edit.channels, qq: { ...edit.channels?.qq, app_id: v } } })} />
        <Field label="客户端密钥" value={edit.channels?.qq?.client_secret ?? ''}
          onChange={(v) => markDirty({ channels: { ...edit.channels, qq: { ...edit.channels?.qq, client_secret: v } } })}
          type="password" />
      </Section>

      {/* Channels - Feishu */}
      <Section title="飞书">
        <Toggle label="启用" value={edit.channels?.feishu?.enabled ?? config.channels.feishu.enabled}
          onChange={(v) => markDirty({ channels: { ...edit.channels, feishu: { ...edit.channels?.feishu, enabled: v } } })} />
        <Field label="应用 ID" value={edit.channels?.feishu?.app_id ?? config.channels.feishu.app_id}
          onChange={(v) => markDirty({ channels: { ...edit.channels, feishu: { ...edit.channels?.feishu, app_id: v } } })} />
        <Field label="应用密钥" value={edit.channels?.feishu?.app_secret ?? ''}
          onChange={(v) => markDirty({ channels: { ...edit.channels, feishu: { ...edit.channels?.feishu, app_secret: v } } })}
          type="password" />
        <Field label="模式" value={edit.channels?.feishu?.mode ?? config.channels.feishu.mode}
          onChange={(v) => markDirty({ channels: { ...edit.channels, feishu: { ...edit.channels?.feishu, mode: v } } })} />
      </Section>

      {/* Session */}
      <Section title="会话">
        <Field label="超时（分钟）" value={String(edit.channels?.session?.timeout_minutes ?? config.channels.session.timeout_minutes)}
          onChange={(v) => markDirty({ channels: { ...edit.channels, session: { ...edit.channels?.session, timeout_minutes: Number(v) } } })} type="number" />
      </Section>

      {/* Server */}
      <Section title="服务端">
        <Field label="主机" value={edit.server?.host ?? config.server.host}
          onChange={(v) => markDirty({ server: { ...edit.server, host: v } })} />
        <Field label="端口" value={String(edit.server?.port ?? config.server.port)}
          onChange={(v) => markDirty({ server: { ...edit.server, port: Number(v) } })} type="number" />
      </Section>

      {/* Logging */}
      <Section title="日志">
        <Field label="级别" value={edit.logging?.level ?? config.logging.level}
          onChange={(v) => markDirty({ logging: { level: v } })} />
      </Section>
    </div>
  );
}

function Section({ title, children }: { title: string; children: React.ReactNode }) {
  return (
    <div className="rounded-lg border border-[var(--border-primary)] p-3">
      <h4 className="mb-2 text-xs font-semibold uppercase tracking-wider text-[var(--text-tertiary)]">{title}</h4>
      <div className="space-y-2">{children}</div>
    </div>
  );
}

function Field({ label, value, onChange, multiline, type }: {
  label: string;
  value: string;
  onChange: (v: string) => void;
  multiline?: boolean;
  type?: string;
}) {
  const cls = 'w-full rounded-lg border border-[var(--border-primary)] bg-[var(--bg-input)] px-2 py-1.5 text-sm text-[var(--text-primary)] outline-none focus:border-[var(--border-focus)]';
  return (
    <div>
      <label className="mb-1 block text-xs text-[var(--text-secondary)]">{label}</label>
      {multiline ? (
        <textarea value={value} onChange={(e) => onChange(e.target.value)} className={cls} rows={3} />
      ) : (
        <input type={type} value={value} onChange={(e) => onChange(e.target.value)} className={cls} />
      )}
    </div>
  );
}

function SelectField({ label, value, options, onChange }: {
  label: string;
  value: string;
  options: string[];
  onChange: (v: string) => void;
}) {
  return (
    <div>
      <label className="mb-1 block text-xs text-[var(--text-secondary)]">{label}</label>
      <select
        value={value}
        onChange={(e) => onChange(e.target.value)}
        className="w-full rounded-lg border border-[var(--border-primary)] bg-[var(--bg-input)] px-2 py-1.5 text-sm text-[var(--text-primary)] outline-none focus:border-[var(--border-focus)]"
      >
        {options.map((opt) => (
          <option key={opt} value={opt}>{opt}</option>
        ))}
      </select>
    </div>
  );
}

function Toggle({ label, value, onChange }: { label: string; value: boolean; onChange: (v: boolean) => void }) {
  return (
    <div className="flex items-center justify-between">
      <span className="text-sm text-[var(--text-primary)]">{label}</span>
      <button
        onClick={() => onChange(!value)}
        className={`relative h-5 w-9 rounded-full transition ${value ? 'bg-[var(--accent)]' : 'bg-[var(--text-tertiary)]'}`}
      >
        <span className={`absolute top-0.5 h-4 w-4 rounded-full bg-white transition ${value ? 'left-[18px]' : 'left-0.5'}`} />
      </button>
    </div>
  );
}
