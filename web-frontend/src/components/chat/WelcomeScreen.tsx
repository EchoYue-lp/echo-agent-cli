import { Code, BarChart3, GraduationCap, FlaskConical, Sparkles } from 'lucide-react';

const suggestions = [
  { icon: Code, text: '从零开发一个全栈项目：需求分析、技术选型、代码实现到部署文档', color: 'var(--accent)', colorHex: '#6366f1', bg: 'var(--accent-bg)' },
  { icon: BarChart3, text: '深度分析用户行为数据，构建画像并生成可落地的业务策略报告', color: 'var(--color-warning)', colorHex: '#f59e0b', bg: 'var(--color-warning-bg)' },
  { icon: GraduationCap, text: '系统检索某研究领域的核心文献，生成结构化的文献综述初稿', color: 'var(--color-success)', colorHex: '#10b981', bg: 'var(--color-success-bg)' },
  { icon: FlaskConical, text: '启动多步骤数据处理任务：清洗、特征构建、模型训练与结果可视化', color: 'var(--color-info)', colorHex: '#3b82f6', bg: 'var(--color-info-bg)' },
];

export function WelcomeScreen({ onSuggestionClick }: { onSuggestionClick: (text: string) => void }) {
  return (
    <div className="flex h-full flex-col items-center justify-center px-4">
      <div className="mb-12 flex flex-col items-center">
        <div
          className="animate-slide-up mb-6 flex h-[80px] w-[80px] items-center justify-center rounded-2xl"
          style={{
            background: 'linear-gradient(135deg, var(--accent) 0%, var(--color-purple) 100%)',
            boxShadow: '0 12px 40px -8px rgba(99, 102, 241, 0.35)',
          }}
        >
          <Sparkles size={36} color="white" />
        </div>
        <h1 className="animate-slide-up text-2xl font-semibold tracking-tight text-[var(--text-primary)]" style={{ animationDelay: '0.05s' }}>
          今天有什么可以帮你的？
        </h1>
        <p className="animate-slide-up mt-2 text-sm text-[var(--text-secondary)]" style={{ animationDelay: '0.1s' }}>
          我是 EchoCoWork，你的智能协作助手
        </p>
      </div>

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
              <s.icon size={18} style={{ color: s.color }} />
            </div>
            <div className="min-w-0 flex items-center">
              <span className="text-[13px] leading-snug text-[var(--text-primary)]">
                {s.text}
              </span>
            </div>
          </button>
        ))}
      </div>
    </div>
  );
}
