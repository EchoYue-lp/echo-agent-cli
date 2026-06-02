import type { DiffHunk } from '../../stores/fileStore';

interface DiffViewerProps {
  hunks: DiffHunk[];
  path: string;
}

export function DiffViewer({ hunks, path }: DiffViewerProps) {
  if (hunks.length === 0) {
    return (
      <div className="flex h-full items-center justify-center text-sm text-[var(--text-tertiary)]">
        <div className="text-center">
          <div className="mb-2 text-lg">No changes</div>
          <div className="text-xs">{path} has no uncommitted changes compared to HEAD</div>
        </div>
      </div>
    );
  }

  const totalInsertions = hunks.reduce(
    (sum, h) => sum + h.lines.filter((l) => l.tag === 'insert').length,
    0
  );
  const totalDeletions = hunks.reduce(
    (sum, h) => sum + h.lines.filter((l) => l.tag === 'delete').length,
    0
  );

  return (
    <div className="flex h-full flex-col min-h-0">
      {/* Summary bar */}
      <div className="flex items-center gap-4 border-b border-[var(--border-primary)] px-4 py-2 text-xs">
        <span className="font-mono text-[var(--text-secondary)]">{path}</span>
        <span className="font-medium text-green-600">+{totalInsertions}</span>
        <span className="font-medium text-red-600">-{totalDeletions}</span>
      </div>

      {/* Diff content */}
      <div className="flex-1 overflow-auto font-mono text-xs leading-5">
        {hunks.map((hunk, hunkIdx) => (
          <div key={hunkIdx} className="border-b border-[var(--border-primary)] last:border-b-0">
            {/* Hunk header */}
            <div
              className="sticky top-0 z-10 px-4 py-1 text-[var(--text-tertiary)]"
              style={{ background: 'color-mix(in srgb, var(--accent) 8%, var(--bg-primary))' }}
            >
              @@ -{hunk.old_start},{hunk.old_count} +{hunk.new_start},{hunk.new_count} @@
            </div>

            {/* Lines */}
            {hunk.lines.map((line, lineIdx) => {
              const bgColor =
                line.tag === 'insert'
                  ? 'rgba(34, 197, 94, 0.12)'
                  : line.tag === 'delete'
                    ? 'rgba(239, 68, 68, 0.12)'
                    : 'transparent';

              const textColor =
                line.tag === 'insert'
                  ? 'var(--color-success, #22c55e)'
                  : line.tag === 'delete'
                    ? 'var(--color-error, #ef4444)'
                    : 'var(--text-secondary)';

              const prefix = line.tag === 'insert' ? '+' : line.tag === 'delete' ? '-' : ' ';

              return (
                <div key={lineIdx} className="flex" style={{ background: bgColor }}>
                  {/* Old line number */}
                  <span className="w-12 shrink-0 select-none px-2 text-right text-[var(--text-tertiary)]">
                    {line.old_line ?? ''}
                  </span>
                  {/* New line number */}
                  <span className="w-12 shrink-0 select-none px-2 text-right text-[var(--text-tertiary)]">
                    {line.new_line ?? ''}
                  </span>
                  {/* Prefix + content */}
                  <span className="shrink-0 select-none px-1" style={{ color: textColor }}>
                    {prefix}
                  </span>
                  <span className="flex-1 whitespace-pre px-2 text-[var(--text-primary)]">
                    {line.content.replace(/\n$/, '')}
                  </span>
                </div>
              );
            })}
          </div>
        ))}
      </div>
    </div>
  );
}
