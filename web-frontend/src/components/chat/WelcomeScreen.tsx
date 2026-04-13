import { Wrench, FileSearch, Terminal, HelpCircle, Sparkles } from 'lucide-react';

const suggestions = [
  { icon: HelpCircle, text: 'What can you do? Tell me about your capabilities', color: '#6366f1' },
  { icon: Wrench, text: 'What tools do you have available?', color: '#f59e0b' },
  { icon: FileSearch, text: 'Help me analyze a file or document', color: '#10b981' },
  { icon: Terminal, text: 'Run a system command for me', color: '#3b82f6' },
];

export function WelcomeScreen({ onSuggestionClick }: { onSuggestionClick: (text: string) => void }) {
  return (
    <div className="flex h-full flex-col items-center justify-center px-4">
      {/* Hero */}
      <div className="mb-10 flex flex-col items-center">
        <div
          className="mb-5 flex h-[72px] w-[72px] items-center justify-center rounded-2xl"
          style={{
            background: 'linear-gradient(135deg, var(--accent) 0%, #a78bfa 100%)',
            boxShadow: '0 8px 24px -4px rgba(99, 102, 241, 0.3)',
          }}
        >
          <Sparkles size={32} color="white" />
        </div>
        <h1
          className="text-2xl font-semibold tracking-tight"
          style={{ color: 'var(--text-primary)' }}
        >
          How can I help you today?
        </h1>
        <p className="mt-2 text-sm" style={{ color: 'var(--text-secondary)' }}>
          I'm Echo Agent, your intelligent AI assistant
        </p>
      </div>

      {/* Suggestion cards */}
      <div className="grid w-full max-w-2xl grid-cols-1 gap-3 sm:grid-cols-2">
        {suggestions.map((s, i) => (
          <button
            key={i}
            onClick={() => onSuggestionClick(s.text)}
            className="flex items-start gap-3 rounded-xl p-4 text-left transition-all"
            style={{
              background: 'var(--bg-primary)',
              border: '1px solid var(--border-primary)',
            }}
            onMouseEnter={(e) => {
              e.currentTarget.style.borderColor = s.color;
              e.currentTarget.style.transform = 'translateY(-1px)';
              e.currentTarget.style.boxShadow = `0 4px 12px -2px ${s.color}25`;
            }}
            onMouseLeave={(e) => {
              e.currentTarget.style.borderColor = 'var(--border-primary)';
              e.currentTarget.style.transform = 'none';
              e.currentTarget.style.boxShadow = 'none';
            }}
          >
            <div
              className="flex h-9 w-9 shrink-0 items-center justify-center rounded-lg"
              style={{ background: s.color + '15' }}
            >
              <s.icon size={17} style={{ color: s.color }} />
            </div>
            <div className="min-w-0">
              <span className="text-[13px] leading-snug" style={{ color: 'var(--text-primary)' }}>
                {s.text}
              </span>
            </div>
          </button>
        ))}
      </div>
    </div>
  );
}
