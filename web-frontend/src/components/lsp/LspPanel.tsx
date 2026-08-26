import { useCallback, useEffect, useMemo, useState } from 'react';
import { Code2, Play, RefreshCw, Square } from 'lucide-react';
import { extensionRequestScope, lspApi } from '../../api/endpoints';
import { useWorkspaceStore } from '../../stores/workspaceStore';

export function LspPanel() {
  const workspace = useWorkspaceStore((state) => state.current);
  const requestScope = useMemo(() => extensionRequestScope(workspace), [workspace]);
  const [status, setStatus] = useState('');
  const [language, setLanguage] = useState('');
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const refresh = useCallback(async () => {
    setError(null);
    try {
      const result = await lspApi.control(requestScope, 'status');
      setStatus(result.message);
    } catch (cause) {
      setError(cause instanceof Error ? cause.message : String(cause));
    }
  }, [requestScope]);

  useEffect(() => {
    void refresh();
  }, [refresh]);

  const control = async (action: 'start' | 'stop' | 'restart') => {
    const target = language.trim();
    if (!target) return;
    setBusy(true);
    setError(null);
    try {
      await lspApi.control(requestScope, action, target);
      await refresh();
    } catch (cause) {
      setError(cause instanceof Error ? cause.message : String(cause));
    } finally {
      setBusy(false);
    }
  };

  return (
    <div className="space-y-4 p-4">
      <div className="flex items-center justify-between">
        <div className="flex items-center gap-2 text-sm font-semibold text-[var(--text-primary)]">
          <Code2 size={16} />
          Language Servers
        </div>
        <button
          type="button"
          onClick={() => void refresh()}
          className="p-1 text-[var(--text-tertiary)] hover:text-[var(--text-primary)]"
          title="Refresh language server status"
        >
          <RefreshCw size={15} />
        </button>
      </div>
      <pre className="min-h-24 whitespace-pre-wrap rounded bg-[var(--bg-code)] p-3 text-xs text-[var(--text-secondary)]">
        {status || 'No language server status available.'}
      </pre>
      {error && <p className="text-xs text-[var(--color-error)]">{error}</p>}
      <div className="flex gap-2">
        <input
          value={language}
          onChange={(event) => setLanguage(event.target.value)}
          placeholder="rust, typescript, python..."
          className="min-w-0 flex-1 rounded border border-[var(--border-primary)] bg-[var(--bg-primary)] px-3 py-2 text-xs text-[var(--text-primary)]"
        />
        <button type="button" disabled={busy} onClick={() => void control('start')} title="Start">
          <Play size={16} />
        </button>
        <button
          type="button"
          disabled={busy}
          onClick={() => void control('restart')}
          title="Restart"
        >
          <RefreshCw size={16} />
        </button>
        <button type="button" disabled={busy} onClick={() => void control('stop')} title="Stop">
          <Square size={16} />
        </button>
      </div>
    </div>
  );
}
