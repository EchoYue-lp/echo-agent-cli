import { useEffect, useState } from 'react';
import { mcpApi } from '../../api/endpoints';
import type { McpServerInfo, McpConfig } from '../../types/api';
import { Globe, Trash2, ChevronDown, ChevronRight, Save, RefreshCw, Power } from 'lucide-react';

export function McpPanel() {
  const [servers, setServers] = useState<McpServerInfo[]>([]);
  const [config, setConfig] = useState<McpConfig | null>(null);
  const [expanded, setExpanded] = useState<string | null>(null);
  const [jsonEditor, setJsonEditor] = useState<string>('');
  const [parseError, setParseError] = useState<string | null>(null);
  const [isSaving, setIsSaving] = useState(false);
  const [saveMessage, setSaveMessage] = useState<{
    type: 'success' | 'error';
    text: string;
  } | null>(null);

  useEffect(() => {
    loadData();
  }, []);

  const loadData = async () => {
    try {
      const [serversData, configData] = await Promise.all([mcpApi.list(), mcpApi.getConfig()]);
      setServers(serversData);
      setConfig(configData);
      setJsonEditor(JSON.stringify(configData, null, 2));
    } catch (e) {
      console.error(e);
    }
  };

  const saveConfig = async () => {
    try {
      setIsSaving(true);
      setSaveMessage(null);

      const parsedConfig = JSON.parse(jsonEditor);

      if (!parsedConfig.mcpServers || typeof parsedConfig.mcpServers !== 'object') {
        throw new Error('Config must have "mcpServers" object');
      }

      // Timeout guard: even if the backend stalls, the spinner must release so
      // the user isn't left looking at "保存中..." forever. 20s is well above
      // the expected config-persist time (the heavy reconnect now runs in the
      // background on the Rust side).
      const SAVE_TIMEOUT_MS = 20_000;
      const result = await Promise.race([
        mcpApi.updateConfig(parsedConfig),
        new Promise<never>((_, reject) =>
          setTimeout(() => reject(new Error('保存超时，请重试')), SAVE_TIMEOUT_MS)
        ),
      ]);

      if (result.success) {
        setSaveMessage({ type: 'success', text: result.message || '配置已保存并应用' });
        await loadData();
      } else {
        setSaveMessage({
          type: 'error',
          text: result.message || '保存失败',
        });
      }
    } catch (e: any) {
      console.error(e);
      const msg = e?.message || '无效的JSON配置';
      setParseError(msg);
      setSaveMessage({ type: 'error', text: msg });
    } finally {
      setIsSaving(false);
    }
  };

  const disconnect = async (name: string) => {
    try {
      await mcpApi.disconnect(name);
      await loadData();
    } catch (e) {
      console.error(e);
    }
  };

  const toggle = async (name: string, currentEnabled: boolean) => {
    try {
      await mcpApi.toggle(name, !currentEnabled);
      await loadData();
    } catch (e) {
      console.error(e);
    }
  };

  const s = {
    text: 'var(--text-primary)',
    textSec: 'var(--text-secondary)',
    textTer: 'var(--text-tertiary)',
    border: 'var(--border-primary)',
    bg: 'var(--bg-primary)',
    bgHover: 'var(--bg-hover)',
    bgInput: 'var(--bg-input)',
    accent: 'var(--accent)',
    code: 'var(--bg-code)',
  };

  return (
    <div className="p-3 space-y-3">
      <div className="flex items-center justify-between">
        <h3 className="text-sm font-semibold" style={{ color: s.text }}>
          MCP 服务器 ({servers.length})
          {config && ` • 已配置 ${Object.keys(config.mcpServers || {}).length} 个`}
        </h3>
        <button
          onClick={loadData}
          className="rounded-md p-1 transition-colors"
          style={{ color: s.textTer }}
        >
          <RefreshCw size={16} />
        </button>
      </div>

      {saveMessage && (
        <div
          className="rounded-lg px-3 py-2 text-xs"
          style={{
            background:
              saveMessage.type === 'success' ? 'var(--color-success-bg)' : 'var(--color-error-bg)',
            color: saveMessage.type === 'success' ? 'var(--color-success)' : 'var(--color-error)',
          }}
        >
          {saveMessage.text}
        </div>
      )}

      <div
        className="space-y-2 rounded-lg border p-3"
        style={{ borderColor: s.border, background: s.bgHover }}
      >
        <div className="text-xs font-medium mb-1" style={{ color: s.text }}>
          MCP 配置（JSON 格式）
        </div>
        <div className="text-xs mb-2" style={{ color: s.textSec }}>
          编辑完整的 MCP 配置，保存后将立即应用。
        </div>

        <textarea
          value={jsonEditor}
          onChange={(e) => {
            setJsonEditor(e.target.value);
            setParseError(null);
            setSaveMessage(null);
          }}
          className="w-full rounded-lg border px-2 py-1.5 text-sm font-mono min-h-[300px]"
          style={{ background: s.bgInput, borderColor: s.border, color: s.text }}
          placeholder={`{\n  "mcpServers": {}\n}`}
        />

        {parseError && (
          <div
            className="rounded-lg px-2 py-1.5 text-xs"
            style={{ background: 'var(--color-error-bg)', color: 'var(--color-error)' }}
          >
            JSON 错误：{parseError}
          </div>
        )}

        <div className="flex gap-2">
          <button
            onClick={() => {
              const example = {
                mcpServers: {
                  playwright: {
                    command: 'npx',
                    args: ['@playwright/mcp@latest'],
                    env: { NODE_OPTIONS: '--no-warnings' },
                  },
                  'my-http-server': {
                    url: 'http://localhost:8100/mcp',
                    headers: {},
                  },
                  'my-sse-server': {
                    url: 'https://api.example.com/mcp/sse',
                    headers: { Authorization: 'Bearer your-token' },
                    transport: 'sse',
                  },
                },
              };
              setJsonEditor(JSON.stringify(example, null, 2));
              setParseError(null);
              setSaveMessage(null);
            }}
            className="flex-1 rounded-lg border py-1.5 text-xs transition-colors"
            style={{ borderColor: s.border, background: s.bg, color: s.text }}
          >
            加载示例
          </button>
          <button
            onClick={() => {
              try {
                const parsed = JSON.parse(jsonEditor);
                setJsonEditor(JSON.stringify(parsed, null, 2));
                setParseError(null);
                setSaveMessage(null);
              } catch {
                setParseError('Invalid JSON - cannot format');
              }
            }}
            className="flex-1 rounded-lg border py-1.5 text-xs transition-colors"
            style={{ borderColor: s.border, background: s.bg, color: s.text }}
          >
            格式化 JSON
          </button>
        </div>

        <div className="flex gap-2">
          <button
            onClick={() => {
              if (config) setJsonEditor(JSON.stringify(config, null, 2));
              setParseError(null);
              setSaveMessage(null);
            }}
            disabled={!config}
            className="flex-1 rounded-lg border py-1.5 text-xs transition-colors disabled:opacity-50"
            style={{ borderColor: s.border, background: s.bg, color: s.text }}
          >
            重置
          </button>
          <button
            onClick={saveConfig}
            disabled={isSaving}
            className="flex-1 rounded-lg py-1.5 text-xs font-medium text-white transition-colors disabled:opacity-50 disabled:cursor-not-allowed flex items-center justify-center gap-1"
            style={{ background: s.accent }}
          >
            {isSaving ? (
              <>
                <RefreshCw size={14} className="animate-spin" /> 保存中...
              </>
            ) : (
              <>
                <Save size={14} /> 保存配置
              </>
            )}
          </button>
        </div>
      </div>

      <div className="space-y-2">
        <div className="text-xs font-medium" style={{ color: s.text }}>
          已连接的服务器
        </div>
        {servers.length === 0 ? (
          <div
            className="rounded-lg px-3 py-4 text-center text-xs"
            style={{ color: s.textTer, background: s.bgHover }}
          >
            暂无 MCP 服务器连接
          </div>
        ) : (
          servers.map((srv) => {
            const isEnabled = srv.enabled !== false;
            return (
              <div
                key={srv.name}
                className="rounded-lg border"
                style={{ borderColor: s.border, background: s.bg, opacity: isEnabled ? 1 : 0.6 }}
              >
                <div className="flex items-center gap-2 px-3 py-2">
                  <button
                    onClick={() => setExpanded(expanded === srv.name ? null : srv.name)}
                    style={{ color: s.textTer }}
                  >
                    {expanded === srv.name ? <ChevronDown size={14} /> : <ChevronRight size={14} />}
                  </button>
                  <Globe
                    size={12}
                    style={{
                      color:
                        isEnabled && srv.status === 'connected'
                          ? 'var(--color-success)'
                          : 'var(--text-tertiary)',
                    }}
                  />
                  <span className="flex-1 truncate text-xs font-medium" style={{ color: s.text }}>
                    {srv.name}
                  </span>
                  <span className="text-[10px]" style={{ color: s.textTer }}>
                    {srv.transport} • {srv.tool_count ?? 0} tools
                  </span>
                  {/* Toggle enable/disable */}
                  <button
                    onClick={() => toggle(srv.name, isEnabled)}
                    title={isEnabled ? '禁用' : '启用'}
                    className="rounded-md p-1 transition-colors"
                    style={{ color: isEnabled ? 'var(--color-success)' : 'var(--text-tertiary)' }}
                  >
                    <Power size={12} />
                  </button>
                  <button onClick={() => disconnect(srv.name)} style={{ color: s.textTer }}>
                    <Trash2 size={12} />
                  </button>
                </div>
                {expanded === srv.name && (
                  <div className="border-t px-3 py-2 space-y-2" style={{ borderColor: s.border }}>
                    <div className="grid grid-cols-2 gap-1 text-xs">
                      <div>
                        <span style={{ color: s.textSec }}>状态：</span>
                        <span
                          className="ml-2 font-medium"
                          style={{
                            color:
                              srv.status === 'connected'
                                ? 'var(--color-success)'
                                : srv.status === 'error'
                                  ? 'var(--color-error)'
                                  : 'var(--color-warning)',
                          }}
                        >
                          {srv.status}
                        </span>
                      </div>
                      <div>
                        <span style={{ color: s.textSec }}>传输：</span>
                        <span className="ml-2 font-medium" style={{ color: s.text }}>
                          {srv.transport}
                        </span>
                      </div>
                      {srv.connected_at && (
                        <div className="col-span-2">
                          <span style={{ color: s.textSec }}>连接时间：</span>
                          <span className="ml-2" style={{ color: s.text }}>
                            {new Date(srv.connected_at).toLocaleString()}
                          </span>
                        </div>
                      )}
                      {srv.error && (
                        <div className="col-span-2">
                          <span style={{ color: s.textSec }}>错误：</span>
                          <span className="ml-2" style={{ color: 'var(--color-error)' }}>
                            {srv.error}
                          </span>
                        </div>
                      )}
                    </div>

                    {(srv.tools?.length ?? 0) > 0 && (
                      <div className="space-y-1">
                        <div className="text-xs font-medium" style={{ color: s.textSec }}>
                          Tools ({srv.tools!.length}):
                        </div>
                        {srv.tools!.map((t) => (
                          <div key={t.name} className="text-xs pl-2">
                            <span className="font-mono" style={{ color: s.text }}>
                              {t.name}
                            </span>
                            <span className="ml-2" style={{ color: s.textTer }}>
                              {t.description}
                            </span>
                          </div>
                        ))}
                      </div>
                    )}
                  </div>
                )}
              </div>
            );
          })
        )}
      </div>
    </div>
  );
}
