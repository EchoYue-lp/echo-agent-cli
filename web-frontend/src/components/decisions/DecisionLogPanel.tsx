import { useState, useEffect, useCallback } from 'react';
import { Scale, Plus, Trash2, X, ChevronDown, ChevronUp } from 'lucide-react';
import { decisionsApi, type Decision, type CreateDecisionRequest } from '../../api/endpoints';

export function DecisionLogPanel() {
  const [decisions, setDecisions] = useState<Decision[]>([]);
  const [showForm, setShowForm] = useState(false);
  const [expandedId, setExpandedId] = useState<string | null>(null);
  const [loading, setLoading] = useState(true);

  // Form state
  const [decision, setDecision] = useState('');
  const [rationale, setRationale] = useState('');
  const [alternatives, setAlternatives] = useState('');
  const [context, setContext] = useState('');
  const [saving, setSaving] = useState(false);

  const load = useCallback(async () => {
    setLoading(true);
    try {
      const data = await decisionsApi.list();
      setDecisions(data);
    } catch {
      // ignore
    } finally {
      setLoading(false);
    }
  }, []);

  useEffect(() => {
    load();
  }, [load]);

  const handleAdd = async () => {
    if (!decision.trim() || !rationale.trim()) return;
    setSaving(true);
    try {
      const req: CreateDecisionRequest = {
        decision: decision.trim(),
        rationale: rationale.trim(),
        alternatives: alternatives
          ? alternatives
              .split('\n')
              .map((s) => s.trim())
              .filter(Boolean)
          : undefined,
        context: context.trim() || undefined,
      };
      await decisionsApi.create(req);
      setDecision('');
      setRationale('');
      setAlternatives('');
      setContext('');
      setShowForm(false);
      await load();
    } catch {
      // ignore
    } finally {
      setSaving(false);
    }
  };

  const handleClear = async () => {
    if (!confirm('Clear all decisions? This cannot be undone.')) return;
    try {
      await decisionsApi.clear();
      setDecisions([]);
    } catch {
      // ignore
    }
  };

  const formatTime = (iso: string) => {
    if (!iso) return '';
    try {
      return new Date(iso).toLocaleString();
    } catch {
      return iso;
    }
  };

  return (
    <div className="flex flex-col h-full">
      {/* Toolbar */}
      <div
        className="flex items-center justify-between px-4 py-2 border-b shrink-0"
        style={{ borderColor: 'var(--border-primary)' }}
      >
        <div className="flex items-center gap-2">
          <Scale size={16} style={{ color: 'var(--text-secondary)' }} />
          <span className="text-sm font-medium" style={{ color: 'var(--text-primary)' }}>
            Decision Log
          </span>
          <span className="text-xs" style={{ color: 'var(--text-tertiary)' }}>
            ({decisions.length})
          </span>
        </div>
        <div className="flex items-center gap-1">
          <button
            onClick={() => setShowForm(!showForm)}
            className="p-1.5 rounded transition-colors hover:bg-[var(--bg-hover)]"
            style={{ color: showForm ? 'var(--accent)' : 'var(--text-secondary)' }}
            title="Add decision"
          >
            {showForm ? <X size={14} /> : <Plus size={14} />}
          </button>
          {decisions.length > 0 && (
            <button
              onClick={handleClear}
              className="p-1.5 rounded transition-colors hover:bg-[var(--bg-hover)]"
              style={{ color: 'var(--text-secondary)' }}
              title="Clear all"
            >
              <Trash2 size={14} />
            </button>
          )}
        </div>
      </div>

      {/* Add form */}
      {showForm && (
        <div
          className="px-4 py-3 border-b space-y-2 shrink-0"
          style={{ borderColor: 'var(--border-primary)', background: 'var(--bg-secondary)' }}
        >
          <input
            type="text"
            value={decision}
            onChange={(e) => setDecision(e.target.value)}
            placeholder="Decision (what was decided)"
            className="w-full px-3 py-1.5 rounded text-sm outline-none border bg-transparent"
            style={{
              color: 'var(--text-primary)',
              borderColor: 'var(--border-primary)',
            }}
          />
          <textarea
            value={rationale}
            onChange={(e) => setRationale(e.target.value)}
            placeholder="Rationale (why)"
            rows={2}
            className="w-full px-3 py-1.5 rounded text-sm outline-none border bg-transparent resize-none"
            style={{
              color: 'var(--text-primary)',
              borderColor: 'var(--border-primary)',
            }}
          />
          <textarea
            value={alternatives}
            onChange={(e) => setAlternatives(e.target.value)}
            placeholder="Alternatives considered (one per line)"
            rows={2}
            className="w-full px-3 py-1.5 rounded text-sm outline-none border bg-transparent resize-none"
            style={{
              color: 'var(--text-primary)',
              borderColor: 'var(--border-primary)',
            }}
          />
          <input
            type="text"
            value={context}
            onChange={(e) => setContext(e.target.value)}
            placeholder="Context (optional)"
            className="w-full px-3 py-1.5 rounded text-sm outline-none border bg-transparent"
            style={{
              color: 'var(--text-primary)',
              borderColor: 'var(--border-primary)',
            }}
          />
          <div className="flex justify-end gap-2 pt-1">
            <button
              onClick={() => setShowForm(false)}
              className="px-3 py-1 rounded text-sm transition-colors hover:bg-[var(--bg-hover)]"
              style={{ color: 'var(--text-secondary)' }}
            >
              Cancel
            </button>
            <button
              onClick={handleAdd}
              disabled={saving || !decision.trim() || !rationale.trim()}
              className="px-3 py-1 rounded text-sm text-white transition-colors disabled:opacity-50"
              style={{ background: 'var(--accent, #3b82f6)' }}
            >
              {saving ? 'Saving...' : 'Add Decision'}
            </button>
          </div>
        </div>
      )}

      {/* List */}
      <div className="flex-1 min-h-0 overflow-auto">
        {loading ? (
          <div className="p-4 text-center text-sm" style={{ color: 'var(--text-tertiary)' }}>
            Loading...
          </div>
        ) : decisions.length === 0 ? (
          <div className="p-8 text-center text-sm" style={{ color: 'var(--text-tertiary)' }}>
            No decisions recorded yet.
            <br />
            <span className="text-xs">
              Decisions help track key choices made during the project.
            </span>
          </div>
        ) : (
          <div className="divide-y" style={{ borderColor: 'var(--border-primary)' }}>
            {decisions.map((d) => {
              const expanded = expandedId === d.id;
              return (
                <div key={d.id} className="px-4 py-3">
                  <button
                    className="w-full flex items-start gap-2 text-left"
                    onClick={() => setExpandedId(expanded ? null : d.id)}
                  >
                    {expanded ? (
                      <ChevronUp
                        size={14}
                        className="mt-0.5 shrink-0"
                        style={{ color: 'var(--text-tertiary)' }}
                      />
                    ) : (
                      <ChevronDown
                        size={14}
                        className="mt-0.5 shrink-0"
                        style={{ color: 'var(--text-tertiary)' }}
                      />
                    )}
                    <div className="flex-1 min-w-0">
                      <div className="text-sm font-medium" style={{ color: 'var(--text-primary)' }}>
                        {d.decision}
                      </div>
                      <div className="text-xs mt-0.5" style={{ color: 'var(--text-tertiary)' }}>
                        {formatTime(d.timestamp)}
                      </div>
                    </div>
                  </button>
                  {expanded && (
                    <div className="mt-2 ml-6 space-y-2 text-sm">
                      <div>
                        <span className="font-medium" style={{ color: 'var(--text-secondary)' }}>
                          Rationale:
                        </span>
                        <p className="mt-0.5" style={{ color: 'var(--text-primary)' }}>
                          {d.rationale}
                        </p>
                      </div>
                      {d.alternatives && d.alternatives.length > 0 && (
                        <div>
                          <span className="font-medium" style={{ color: 'var(--text-secondary)' }}>
                            Alternatives:
                          </span>
                          <ul
                            className="mt-0.5 list-disc list-inside"
                            style={{ color: 'var(--text-primary)' }}
                          >
                            {d.alternatives.map((a, i) => (
                              <li key={i}>{a}</li>
                            ))}
                          </ul>
                        </div>
                      )}
                      {d.context && (
                        <div>
                          <span className="font-medium" style={{ color: 'var(--text-secondary)' }}>
                            Context:
                          </span>
                          <p className="mt-0.5" style={{ color: 'var(--text-primary)' }}>
                            {d.context}
                          </p>
                        </div>
                      )}
                    </div>
                  )}
                </div>
              );
            })}
          </div>
        )}
      </div>
    </div>
  );
}
