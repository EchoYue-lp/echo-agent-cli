import { useEffect, useState } from 'react';
import { Play, Settings, Cpu } from 'lucide-react';
import { sandboxApi } from '../../api/endpoints';
import type { SandboxStatus, SandboxConfig, SandboxExecuteResult } from '../../types/api';

const LANGUAGES = [
  { value: 'shell', label: 'Shell (sh)' },
  { value: 'python', label: 'Python' },
  { value: 'ruby', label: 'Ruby' },
  { value: 'node', label: 'Node.js' },
  { value: 'perl', label: 'Perl' },
];

export function SandboxPanel() {
  const [status, setStatus] = useState<SandboxStatus | null>(null);
  const [config, setConfig] = useState<SandboxConfig | null>(null);
  const [language, setLanguage] = useState('python');
  const [code, setCode] = useState('');
  const [result, setResult] = useState<SandboxExecuteResult | null>(null);
  const [running, setRunning] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [showConfig, setShowConfig] = useState(false);

  useEffect(() => {
    sandboxApi.status().then(setStatus).catch(console.error);
    sandboxApi.config().then(setConfig).catch(console.error);
  }, []);

  const run = async () => {
    if (!code.trim()) return;
    setRunning(true);
    setError(null);
    setResult(null);
    try {
      const res = await sandboxApi.execute({ language, code });
      setResult(res);
    } catch (e: unknown) {
      setError(e instanceof Error ? e.message : 'Execution failed');
    }
    setRunning(false);
  };

  const updateConfig = async (key: keyof SandboxConfig, value: unknown) => {
    if (!config) return;
    const newConfig = { ...config, [key]: value };
    setConfig(newConfig);
    try {
      await sandboxApi.updateConfig(newConfig);
    } catch (e) {
      console.error(e);
    }
  };

  return (
    <div className="p-3 space-y-3">
      <div className="flex items-center justify-between">
        <h3 className="text-sm font-semibold" style={{ color: 'var(--text-primary)' }}>
          Sandbox
        </h3>
        <button
          onClick={() => setShowConfig(!showConfig)}
          className="rounded-md p-1 transition-colors"
          style={{ color: 'var(--text-tertiary)' }}
        >
          <Settings size={14} />
        </button>
      </div>

      {/* Status badges */}
      {status && (
        <div className="flex flex-wrap gap-2">
          <span className="inline-flex items-center gap-1 rounded-full px-2 py-0.5 text-[10px] font-medium"
            style={{ background: status.local_available ? '#10b98118' : '#ef444418', color: status.local_available ? '#10b981' : '#ef4444' }}>
            <Cpu size={10} /> Local
          </span>
          <span className="inline-flex items-center gap-1 rounded-full px-2 py-0.5 text-[10px] font-medium"
            style={{ background: status.docker_available ? '#10b98118' : '#94a3b818', color: status.docker_available ? '#10b981' : '#94a3b8' }}>
            Docker
          </span>
          <span className="inline-flex items-center gap-1 rounded-full px-2 py-0.5 text-[10px] font-medium"
            style={{ background: '#94a3b818', color: '#94a3b8' }}>
            K8s
          </span>
        </div>
      )}

      {/* Config */}
      {showConfig && config && (
        <div className="space-y-2 rounded-lg border p-3" style={{ borderColor: 'var(--border-primary)' }}>
          <label className="flex items-center justify-between text-xs" style={{ color: 'var(--text-secondary)' }}>
            Security Level
            <select value={config.security_level} onChange={(e) => updateConfig('security_level', e.target.value)}
              className="rounded border px-1.5 py-0.5 text-xs" style={{ background: 'var(--bg-input)', borderColor: 'var(--border-primary)', color: 'var(--text-primary)' }}>
              <option value="low">Low</option>
              <option value="medium">Medium</option>
              <option value="high">High</option>
            </select>
          </label>
          <label className="flex items-center justify-between text-xs" style={{ color: 'var(--text-secondary)' }}>
            Max Memory (MB)
            <input type="number" value={config.max_memory_mb ?? 512} onChange={(e) => updateConfig('max_memory_mb', parseInt(e.target.value) || 512)}
              className="w-20 rounded border px-1.5 py-0.5 text-xs" style={{ background: 'var(--bg-input)', borderColor: 'var(--border-primary)', color: 'var(--text-primary)' }} />
          </label>
          <label className="flex items-center justify-between text-xs" style={{ color: 'var(--text-secondary)' }}>
            Max CPU (seconds)
            <input type="number" value={config.max_cpu_seconds ?? 30} onChange={(e) => updateConfig('max_cpu_seconds', parseInt(e.target.value) || 30)}
              className="w-20 rounded border px-1.5 py-0.5 text-xs" style={{ background: 'var(--bg-input)', borderColor: 'var(--border-primary)', color: 'var(--text-primary)' }} />
          </label>
          <label className="flex items-center gap-2 text-xs" style={{ color: 'var(--text-secondary)' }}>
            <input type="checkbox" checked={config.network_enabled} onChange={(e) => updateConfig('network_enabled', e.target.checked)} />
            Network Enabled
          </label>
        </div>
      )}

      {/* Code editor */}
      <div className="space-y-2">
        <select value={language} onChange={(e) => setLanguage(e.target.value)}
          className="w-full rounded-lg border px-2 py-1.5 text-xs"
          style={{ background: 'var(--bg-input)', borderColor: 'var(--border-primary)', color: 'var(--text-primary)' }}>
          {LANGUAGES.map((l) => <option key={l.value} value={l.value}>{l.label}</option>)}
        </select>

        <textarea
          value={code}
          onChange={(e) => setCode(e.target.value)}
          placeholder="Enter code to execute..."
          rows={6}
          className="w-full rounded-lg border px-3 py-2 font-mono text-xs leading-relaxed"
          style={{ background: 'var(--bg-code)', borderColor: 'var(--border-primary)', color: '#e2e8f0' }}
        />

        <button
          onClick={run}
          disabled={running || !code.trim()}
          className="flex w-full items-center justify-center gap-2 rounded-lg py-2 text-xs font-medium transition-colors"
          style={{
            background: running ? 'var(--border-primary)' : 'var(--accent)',
            color: running ? 'var(--text-tertiary)' : 'white',
          }}
        >
          {running ? (
            <><div className="spinner" /> Running...</>
          ) : (
            <><Play size={12} /> Run Code</>
          )}
        </button>
      </div>

      {/* Results */}
      {error && (
        <div className="rounded-lg border-l-[3px] border-red-500 p-3 text-xs" style={{ background: '#ef444410' }}>
          <p style={{ color: '#ef4444' }}>{error}</p>
        </div>
      )}

      {result && (
        <div className="space-y-2 rounded-lg border p-3" style={{ borderColor: result.success ? '#10b981' : '#ef4444' }}>
          <div className="flex items-center justify-between">
            <span className="text-[10px] font-semibold uppercase tracking-wider" style={{ color: result.success ? '#10b981' : '#ef4444' }}>
              {result.success ? 'Success' : 'Failed'}
            </span>
            <span className="text-[10px]" style={{ color: 'var(--text-tertiary)' }}>
              {result.duration_ms}ms | Exit: {result.exit_code ?? 'N/A'}
            </span>
          </div>
          {result.stdout && (
            <div>
              <span className="text-[10px] font-medium uppercase tracking-wider" style={{ color: 'var(--text-tertiary)' }}>stdout</span>
              <pre className="mt-1 max-h-32 overflow-auto rounded-lg p-2 text-[11px]" style={{ background: 'var(--bg-code)', color: '#e2e8f0' }}>
                {result.stdout}
              </pre>
            </div>
          )}
          {result.stderr && (
            <div>
              <span className="text-[10px] font-medium uppercase tracking-wider" style={{ color: '#ef4444' }}>stderr</span>
              <pre className="mt-1 max-h-32 overflow-auto rounded-lg p-2 text-[11px]" style={{ background: 'var(--bg-code)', color: '#e2e8f0' }}>
                {result.stderr}
              </pre>
            </div>
          )}
        </div>
      )}
    </div>
  );
}
