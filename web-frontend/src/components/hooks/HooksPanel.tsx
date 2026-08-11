import { useEffect, useState } from 'react';
import { hooksApi, type HookSourceInfo, type HooksReloadSummary } from '../../api/endpoints';
import {
  Webhook,
  RefreshCw,
  Check,
  AlertCircle,
  X,
  Loader2,
  FileCode2,
  TestTube2,
} from 'lucide-react';

/** Categorize a hook source string into a stable kind for icon/color. */
function sourceKind(source: string): 'user' | 'skill' | 'plugin' | 'other' {
  if (source === 'user_config') return 'user';
  if (source.startsWith('skill:')) return 'skill';
  if (source.startsWith('plugin:')) return 'plugin';
  return 'other';
}

const KIND_LABEL: Record<ReturnType<typeof sourceKind>, string> = {
  user: 'User Config',
  skill: 'Skill',
  plugin: 'Plugin',
  other: 'Other',
};

export function HooksPanel() {
  const [sources, setSources] = useState<HookSourceInfo[]>([]);
  const [loading, setLoading] = useState(true);
  const [testing, setTesting] = useState(false);
  const [events, setEvents] = useState<string[]>([]);
  const [selectedEvent, setSelectedEvent] = useState('PreToolUse');
  const [matcher, setMatcher] = useState('*');
  const [error, setError] = useState<string | null>(null);
  const [message, setMessage] = useState<{ type: 'success' | 'error'; text: string } | null>(null);

  useEffect(() => {
    loadHooks();
    hooksApi
      .events()
      .then((available) => {
        setEvents(available);
        setSelectedEvent((current) =>
          available.length > 0 && !available.includes(current) ? available[0] : current
        );
      })
      .catch((e: any) => setError(e.message || 'Failed to load hook events'));
  }, []);

  const loadHooks = async () => {
    try {
      setLoading(true);
      const data = await hooksApi.list();
      setSources(data);
      setError(null);
    } catch (e: any) {
      setError(e.message || 'Failed to load hooks');
    } finally {
      setLoading(false);
    }
  };

  const handleReload = async () => {
    try {
      setLoading(true);
      setMessage(null);
      const summary: HooksReloadSummary = await hooksApi.reload();
      if (summary.success) {
        const detail =
          summary.loaded_from.length > 0 ? ` from ${summary.loaded_from.join(', ')}` : '';
        setMessage({
          type: 'success',
          text: summary.message || `Reloaded ${summary.rule_count} hook rules${detail}`,
        });
        await loadHooks();
      } else {
        setMessage({ type: 'error', text: summary.message || 'Reload failed' });
      }
    } catch (e: any) {
      setMessage({ type: 'error', text: e.message || 'Reload failed' });
    } finally {
      setLoading(false);
    }
  };

  const handleTest = async () => {
    try {
      setTesting(true);
      setMessage(null);
      const result = await hooksApi.test(selectedEvent, matcher);
      const details = result.matches
        .map((item) => `${item.source}: ${item.action} (${item.matcher || '*'})`)
        .join('; ');
      setMessage({
        type: result.matches.length > 0 ? 'success' : 'error',
        text:
          result.matches.length > 0
            ? `${result.event} dry-run matched ${result.matches.length}: ${details}`
            : `${result.event} dry-run matched no actions`,
      });
    } catch (e: any) {
      setMessage({ type: 'error', text: e.message || 'Hook test failed' });
    } finally {
      setTesting(false);
    }
  };

  const totalRules = sources.reduce((sum, s) => sum + s.rule_count, 0);

  return (
    <div className="space-y-4">
      {/* Header */}
      <div className="flex items-center justify-between">
        <div className="flex items-center gap-2">
          <input
            value={matcher}
            onChange={(event) => setMatcher(event.target.value)}
            className="h-9 w-32 rounded-md border bg-[var(--bg-input)] px-2 text-xs"
            style={{ color: 'var(--text-secondary)', borderColor: 'var(--border-primary)' }}
            aria-label="Hook matcher"
            placeholder="Matcher"
          />
          <Webhook className="w-5 h-5" style={{ color: 'var(--accent)' }} />
          <h2 className="text-lg font-semibold" style={{ color: 'var(--text-primary)' }}>
            Hooks
          </h2>
          <span className="text-xs" style={{ color: 'var(--text-tertiary)' }}>
            {sources.length} sources, {totalRules} rules
          </span>
        </div>
        <div className="flex items-center gap-2">
          <select
            value={selectedEvent}
            onChange={(event) => setSelectedEvent(event.target.value)}
            disabled={events.length === 0 || testing}
            className="h-9 max-w-48 rounded-md border bg-[var(--bg-secondary)] px-2 text-xs"
            style={{ color: 'var(--text-secondary)', borderColor: 'var(--border-primary)' }}
            aria-label="Hook event"
          >
            {events.map((event) => (
              <option key={event} value={event}>
                {event}
              </option>
            ))}
          </select>
          <button
            onClick={handleTest}
            disabled={events.length === 0 || testing}
            className="flex h-9 w-9 items-center justify-center rounded-md transition-colors disabled:opacity-50 hover:bg-[var(--bg-hover)]"
            style={{ color: 'var(--text-secondary)', border: '1px solid var(--border-primary)' }}
            title="Dry-run hook matching"
            aria-label="Dry-run hook matching"
          >
            {testing ? (
              <Loader2 className="h-4 w-4 animate-spin" />
            ) : (
              <TestTube2 className="h-4 w-4" />
            )}
          </button>
          <button
            onClick={handleReload}
            disabled={loading}
            className="flex h-9 w-9 items-center justify-center rounded-md transition-colors disabled:opacity-50 hover:bg-[var(--bg-hover)]"
            style={{
              color: 'var(--text-secondary)',
              border: '1px solid var(--border-primary)',
            }}
            title="Reload hooks from config"
            aria-label="Reload hooks from config"
          >
            {loading ? (
              <Loader2 className="w-4 h-4 animate-spin" />
            ) : (
              <RefreshCw className="w-4 h-4" />
            )}
          </button>
        </div>
      </div>

      {/* Message */}
      {message && (
        <div
          className="flex items-center gap-2 px-3 py-2 rounded-lg text-sm border"
          style={{
            background:
              message.type === 'success' ? 'rgba(34, 197, 94, 0.1)' : 'rgba(239, 68, 68, 0.1)',
            color:
              message.type === 'success'
                ? 'var(--color-success, #22c55e)'
                : 'var(--color-error, #ef4444)',
            borderColor:
              message.type === 'success' ? 'rgba(34, 197, 94, 0.3)' : 'rgba(239, 68, 68, 0.3)',
          }}
        >
          {message.type === 'success' ? (
            <Check className="w-4 h-4 flex-shrink-0" />
          ) : (
            <AlertCircle className="w-4 h-4 flex-shrink-0" />
          )}
          <span className="break-all">{message.text}</span>
          <button onClick={() => setMessage(null)} className="ml-auto flex-shrink-0">
            <X className="w-3 h-3" />
          </button>
        </div>
      )}

      {/* Source List */}
      {loading ? (
        <div className="flex items-center justify-center py-8">
          <Loader2 className="w-6 h-6 animate-spin" style={{ color: 'var(--accent)' }} />
        </div>
      ) : error ? (
        <div
          className="flex items-center gap-2 px-3 py-4 text-sm"
          style={{ color: 'var(--color-error, #ef4444)' }}
        >
          <AlertCircle className="w-4 h-4" />
          {error}
        </div>
      ) : sources.length === 0 ? (
        <div className="text-center py-8 text-sm" style={{ color: 'var(--text-tertiary)' }}>
          No hooks registered.
        </div>
      ) : (
        <div className="space-y-2">
          {sources.map((src) => {
            const kind = sourceKind(src.source);
            return (
              <div
                key={src.source}
                className="flex items-center gap-3 px-4 py-3 rounded-lg border"
                style={{
                  background: 'var(--bg-secondary)',
                  borderColor: 'var(--border-primary)',
                }}
              >
                <FileCode2 className="w-4 h-4 flex-shrink-0" style={{ color: 'var(--accent)' }} />
                <div className="flex-1 min-w-0">
                  <div className="flex items-center gap-2">
                    <span
                      className="text-sm font-medium font-mono"
                      style={{ color: 'var(--text-primary)' }}
                    >
                      {src.source}
                    </span>
                    <span
                      className="text-xs px-1.5 py-0.5 rounded-md"
                      style={{
                        background: 'var(--bg-tertiary)',
                        color: 'var(--text-tertiary)',
                      }}
                    >
                      {KIND_LABEL[kind]}
                    </span>
                  </div>
                </div>
                <span
                  className="text-xs px-2 py-0.5 rounded-md flex-shrink-0"
                  style={{
                    background: 'rgba(139, 92, 246, 0.1)',
                    color: 'var(--accent)',
                  }}
                >
                  {src.rule_count} {src.rule_count === 1 ? 'rule' : 'rules'}
                </span>
              </div>
            );
          })}
        </div>
      )}
    </div>
  );
}
