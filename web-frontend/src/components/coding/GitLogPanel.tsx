interface GitCommit {
  hash: string;
  message: string;
  author: string;
  date: string;
  filesChanged?: number;
  insertions?: number;
  deletions?: number;
}

interface GitLogPanelProps {
  commits: GitCommit[];
  branch?: string;
  onCommitClick?: (commit: GitCommit) => void;
}

/**
 * GitLogPanel - Git 提交历史可视化组件
 * Displays git commit history with graph visualization
 */
export function GitLogPanel({ commits, branch = 'main', onCommitClick }: GitLogPanelProps) {
  return (
    <div className="rounded-xl border border-[var(--border-primary)] bg-[var(--bg-primary)] overflow-hidden shadow-[var(--shadow-sm)]">
      {/* Branch Header */}
      <div className="flex items-center gap-3 px-5 py-3 bg-[var(--bg-secondary)] border-b border-[var(--border-primary)]">
        <div className="flex items-center gap-2">
          <svg
            className="w-4 h-4 text-[var(--text-tertiary)]"
            fill="none"
            viewBox="0 0 24 24"
            stroke="currentColor"
          >
            <path
              strokeLinecap="round"
              strokeLinejoin="round"
              strokeWidth={2}
              d="M7 7h.01M7 3h5c.512 0 1.024.05 1.497.15m-5.13 13.36c-.15.32-.268.662-.337 1.018m6.137-.15c.512.15 1.024.15 1.497.15m-8.13-13.36c-.15.32-.268.662-.337 1.018m6.137-.15c.512.15 1.024.15 1.497.15"
            />
          </svg>
          <span className="text-sm font-medium text-[var(--text-primary)]">{branch}</span>
        </div>
        <span className="text-xs text-[var(--text-tertiary)]">{commits.length} commits</span>
      </div>

      {/* Commit Timeline */}
      <div className="divide-y divide-[var(--border-primary)]">
        {commits.length === 0 ? (
          <div className="py-8 text-center text-sm text-[var(--text-tertiary)]">No commits yet</div>
        ) : (
          commits.map((commit, index) => (
            <button
              type="button"
              key={index}
              className="group flex w-full cursor-pointer gap-4 px-5 py-4 text-left transition-colors hover:bg-[var(--bg-hover)]"
              onClick={() => onCommitClick?.(commit)}
            >
              {/* Timeline */}
              <div className="flex flex-col items-center pt-1">
                <div className="w-3 h-3 rounded-full border-2 border-[var(--accent)] bg-[var(--bg-primary)] group-hover:bg-[var(--accent)] transition-colors" />
                {index < commits.length - 1 && (
                  <div className="w-px flex-1 bg-[var(--border-primary)] mt-1" />
                )}
              </div>

              {/* Commit Content */}
              <div className="flex-1 min-w-0 pb-2">
                <p className="text-sm font-medium text-[var(--text-primary)] leading-snug mb-1 line-clamp-2">
                  {commit.message}
                </p>
                <div className="flex items-center gap-3 text-xs text-[var(--text-tertiary)]">
                  <span className="font-mono text-[var(--accent)] bg-[var(--accent-bg)] px-1.5 py-0.5 rounded-md">
                    {commit.hash.slice(0, 7)}
                  </span>
                  <span>{commit.author}</span>
                  <span>{commit.date}</span>
                </div>
                {(commit.filesChanged ||
                  commit.insertions !== undefined ||
                  commit.deletions !== undefined) && (
                  <div className="flex items-center gap-3 mt-2 text-xs">
                    {commit.filesChanged !== undefined && commit.filesChanged > 0 && (
                      <span className="text-[var(--text-secondary)]">
                        {commit.filesChanged} file{commit.filesChanged !== 1 ? 's' : ''}
                      </span>
                    )}
                    {commit.insertions !== undefined && commit.insertions > 0 && (
                      <span className="text-green-600 dark:text-green-400 font-medium">
                        +{commit.insertions}
                      </span>
                    )}
                    {commit.deletions !== undefined && commit.deletions > 0 && (
                      <span className="text-red-600 dark:text-red-400 font-medium">
                        -{commit.deletions}
                      </span>
                    )}
                  </div>
                )}
              </div>
            </button>
          ))
        )}
      </div>
    </div>
  );
}

export default GitLogPanel;
