import { useEffect, useState } from 'react';
import { Sparkles, Play, Clock, Loader2 } from 'lucide-react';
import { sessionApi } from '../../api/endpoints';
import { useConversationStore } from '../../stores/conversationStore';

interface Suggestion {
  icon: React.ComponentType<{ size?: number; color?: string; className?: string }>;
  text: string;
  color: string;
  colorHex: string;
  bg: string;
}

const suggestions: Suggestion[] = [
  // Quick navigation items removed per user request
];

interface LatestSession {
  id: string;
  title: string;
  updated_at: string;
  message_count: number;
}

export function WelcomeScreen({
  onSuggestionClick,
}: {
  onSuggestionClick: (text: string) => void;
}) {
  const loadConversation = useConversationStore((s) => s.loadConversation);

  const [latestSession, setLatestSession] = useState<LatestSession | null>(null);
  const [resumeLoading, setResumeLoading] = useState(false);
  const [checking, setChecking] = useState(true);

  useEffect(() => {
    let cancelled = false;

    const checkLatest = async () => {
      try {
        const res = await sessionApi.getLatest();
        if (!cancelled && res.found && res.id) {
          setLatestSession({
            id: res.id,
            title: res.title || 'Untitled',
            updated_at: res.updated_at || '',
            message_count: res.message_count || 0,
          });
        }
      } catch {
        // ignore
      } finally {
        if (!cancelled) setChecking(false);
      }
    };

    checkLatest();
    return () => {
      cancelled = true;
    };
  }, []);

  const handleResume = async () => {
    if (!latestSession) return;
    setResumeLoading(true);
    try {
      await loadConversation(latestSession.id);
    } catch (e) {
      console.error('Failed to resume session:', e);
    } finally {
      setResumeLoading(false);
    }
  };

  const formatRelativeTime = (dateStr: string) => {
    if (!dateStr) return '';
    const date = new Date(dateStr);
    const now = new Date();
    const diffMs = now.getTime() - date.getTime();
    const diffMins = Math.floor(diffMs / 60000);
    if (diffMins < 1) return 'just now';
    if (diffMins < 60) return `${diffMins}m ago`;
    const diffHours = Math.floor(diffMins / 60);
    if (diffHours < 24) return `${diffHours}h ago`;
    const diffDays = Math.floor(diffHours / 24);
    if (diffDays < 7) return `${diffDays}d ago`;
    return date.toLocaleDateString();
  };

  return (
    <div className="flex h-full flex-col items-center justify-center px-4">
      <div className="mb-10 flex flex-col items-center">
        <div className="animate-slide-up mb-5 flex h-12 w-12 items-center justify-center rounded-xl border border-[var(--border-primary)] bg-[var(--bg-primary)] text-[var(--accent)] shadow-[var(--shadow-sm)]">
          <Sparkles size={22} />
        </div>
        <h1
          className="animate-slide-up text-xl font-semibold tracking-tight text-[var(--text-primary)]"
          style={{ animationDelay: '0.05s' }}
        >
          今天有什么可以帮你的？
        </h1>
        <p
          className="animate-slide-up mt-2 text-sm text-[var(--text-secondary)]"
          style={{ animationDelay: '0.1s' }}
        >
          我是 EKO，你的智能协作助手
        </p>
      </div>

      {/* Continue last session */}
      {!checking && latestSession && (
        <div className="animate-slide-up mb-6 w-full max-w-2xl" style={{ animationDelay: '0.12s' }}>
          <button
            onClick={handleResume}
            disabled={resumeLoading}
            className="group flex w-full items-center gap-4 rounded-xl border border-[var(--border-primary)] bg-[var(--bg-primary)] p-4 text-left transition-all duration-200 hover:-translate-y-0.5 hover:shadow-[var(--shadow-lg)] disabled:opacity-60 disabled:cursor-not-allowed"
            style={{ borderColor: 'var(--accent)' }}
            onMouseEnter={(e) => {
              if (!resumeLoading) {
                e.currentTarget.style.borderColor = 'var(--accent)';
                e.currentTarget.style.boxShadow =
                  '0 8px 28px -4px color-mix(in srgb, var(--accent) 35%, transparent)';
              }
            }}
            onMouseLeave={(e) => {
              e.currentTarget.style.boxShadow = '';
            }}
          >
            <div
              className="flex h-10 w-10 shrink-0 items-center justify-center rounded-lg transition-all duration-200 group-hover:scale-110"
              style={{ background: 'var(--accent-bg)' }}
            >
              {resumeLoading ? (
                <Loader2 size={18} className="animate-spin" style={{ color: 'var(--accent)' }} />
              ) : (
                <Play size={18} style={{ color: 'var(--accent)' }} />
              )}
            </div>
            <div className="min-w-0 flex-1">
              <div className="flex items-center gap-2">
                <span className="text-[13px] font-medium text-[var(--text-primary)]">
                  {resumeLoading ? '正在恢复会话...' : '继续上次对话'}
                </span>
              </div>
              <div className="mt-0.5 flex items-center gap-2 text-xs text-[var(--text-tertiary)]">
                <Clock size={11} />
                <span className="truncate">{formatRelativeTime(latestSession.updated_at)}</span>
                <span>·</span>
                <span>{latestSession.message_count} messages</span>
                {latestSession.title && latestSession.title !== 'Untitled' && (
                  <>
                    <span>·</span>
                    <span className="truncate">{latestSession.title}</span>
                  </>
                )}
              </div>
            </div>
          </button>
        </div>
      )}

      <div className="grid w-full max-w-2xl grid-cols-1 gap-3 sm:grid-cols-2">
        {suggestions.map((s, i) => (
          <button
            key={i}
            onClick={() => onSuggestionClick(s.text)}
            className="animate-slide-up group flex items-start gap-3 rounded-xl border border-[var(--border-primary)] bg-[var(--bg-primary)] p-4 text-left transition-all duration-200 hover:-translate-y-1 hover:shadow-[var(--shadow-lg)]"
            style={{ animationDelay: `${0.1 + i * 0.05}s` }}
            onMouseEnter={(e) => {
              e.currentTarget.style.borderColor = s.colorHex;
              e.currentTarget.style.boxShadow = `0 8px 28px -4px ${s.colorHex}35`;
            }}
            onMouseLeave={(e) => {
              e.currentTarget.style.borderColor = '';
              e.currentTarget.style.boxShadow = '';
            }}
          >
            <div
              className="flex h-10 w-10 shrink-0 items-center justify-center rounded-lg transition-all duration-200 group-hover:scale-110 group-hover:rotate-[-4deg]"
              style={{ background: s.bg }}
            >
              <s.icon size={18} color={s.color} />
            </div>
            <div className="min-w-0 flex items-center">
              <span className="text-[13px] leading-snug text-[var(--text-primary)]">{s.text}</span>
            </div>
          </button>
        ))}
      </div>
    </div>
  );
}
