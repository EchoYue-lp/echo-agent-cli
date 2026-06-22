import { useState, type ReactNode } from 'react';

interface RuntimeStoryCardProps {
  /** Card title, e.g. "路由决策" "并行执行" */
  title: string;
  /** Left timeline dot status */
  status?: 'pending' | 'active' | 'done' | 'error';
  /** Whether the card is collapsed by default (for process cards) */
  defaultCollapsed?: boolean;
  /** Extra meta text shown in the header, right side */
  meta?: ReactNode;
  children: ReactNode;
}

export function RuntimeStoryCard({
  title,
  status = 'done',
  defaultCollapsed = false,
  meta,
  children,
}: RuntimeStoryCardProps) {
  const [collapsed, setCollapsed] = useState(defaultCollapsed);

  return (
    <section className="runtime-story-card my-3 rounded-lg border border-[var(--border-primary)] bg-[var(--bg-primary)]">
      <header
        className="flex items-center gap-2 px-4 py-2 cursor-pointer select-none"
        onClick={() => setCollapsed((c) => !c)}
      >
        <span className={`story-dot story-dot--${status}`} />
        <h3 className="text-sm font-medium flex-1" style={{ color: 'var(--text-primary)' }}>
          {title}
        </h3>
        {meta && (
          <span className="text-xs shrink-0" style={{ color: 'var(--text-tertiary)' }}>
            {meta}
          </span>
        )}
        <span className="text-xs shrink-0" style={{ color: 'var(--text-tertiary)' }}>
          {collapsed ? '展开' : '收起'}
        </span>
      </header>
      {!collapsed && <div className="px-4 pb-3">{children}</div>}
    </section>
  );
}
