import { useState } from 'react';
import type { Paper } from '../../api/endpoints';

interface PaperListProps {
  papers: Paper[];
  selectedId: string | null;
  onSelect: (paper: Paper) => void;
}

export function PaperList({ papers, selectedId, onSelect }: PaperListProps) {
  const [searchQuery, setSearchQuery] = useState('');

  const filtered = papers.filter((p) =>
    p.title.toLowerCase().includes(searchQuery.toLowerCase()) ||
    p.authors.some((a) => a.toLowerCase().includes(searchQuery.toLowerCase()))
  );

  return (
    <div className="flex flex-col h-full">
      <div className="p-3 border-b" style={{ borderColor: 'var(--border-primary)' }}>
        <input
          type="text"
          value={searchQuery}
          onChange={(e) => setSearchQuery(e.target.value)}
          placeholder="搜索论文..."
          className="w-full rounded-lg border px-3 py-2 text-sm outline-none"
          style={{ background: 'var(--bg-input)', borderColor: 'var(--border-primary)', color: 'var(--text-primary)' }}
        />
      </div>
      <div className="flex-1 overflow-y-auto">
        {filtered.length === 0 ? (
          <div className="p-6 text-center text-sm" style={{ color: 'var(--text-tertiary)' }}>
            暂无论文
          </div>
        ) : (
          filtered.map((paper) => (
            <button
              key={paper.id}
              onClick={() => onSelect(paper)}
              className="w-full text-left px-4 py-3 border-b transition-colors"
              style={{
                borderColor: 'var(--border-primary)',
                background: selectedId === paper.id ? 'var(--bg-sidebar-active)' : 'transparent',
              }}
              onMouseEnter={(e) => {
                if (selectedId !== paper.id) e.currentTarget.style.background = 'var(--bg-hover)';
              }}
              onMouseLeave={(e) => {
                if (selectedId !== paper.id) e.currentTarget.style.background = 'transparent';
              }}
            >
              <div className="text-sm font-medium truncate" style={{ color: 'var(--text-primary)' }}>
                {paper.title}
              </div>
              <div className="text-xs mt-1 truncate" style={{ color: 'var(--text-secondary)' }}>
                {paper.authors.join(', ')} {paper.year ? `(${paper.year})` : ''}
              </div>
              {paper.tags.length > 0 && (
                <div className="flex gap-1 mt-1.5 flex-wrap">
                  {paper.tags.map((tag) => (
                    <span key={tag} className="text-[10px] px-1.5 py-0.5 rounded" style={{ background: 'var(--bg-secondary)', color: 'var(--text-tertiary)' }}>
                      {tag}
                    </span>
                  ))}
                </div>
              )}
            </button>
          ))
        )}
      </div>
    </div>
  );
}
