interface DiffLine {
  type: 'add' | 'remove' | 'context';
  content: string;
  lineNumber?: number;
}

interface DiffViewerProps {
  diff: string;
  oldFileName?: string;
  newFileName?: string;
}

/**
 * DiffViewer - 代码 diff 查看组件
 * Renders unified diff format with syntax highlighting
 */
export function DiffViewer({ diff, oldFileName = 'old', newFileName = 'new' }: DiffViewerProps) {
  const lines = diff.split('\n');
  const parsedLines: DiffLine[] = [];

  lines.forEach((line) => {
    if (line.startsWith('+')) {
      parsedLines.push({ type: 'add', content: line.slice(1) });
    } else if (line.startsWith('-')) {
      parsedLines.push({ type: 'remove', content: line.slice(1) });
    } else {
      parsedLines.push({ type: 'context', content: line });
    }
  });

  return (
    <div className="rounded-xl border border-[var(--border-primary)] bg-[var(--bg-primary)] overflow-hidden shadow-sm">
      <div className="flex items-center gap-3 px-4 py-3 bg-[var(--bg-secondary)] border-b border-[var(--border-primary)]">
        <div className="flex items-center gap-2 text-sm">
          <span className="font-medium text-[var(--text-secondary)]">{oldFileName}</span>
          <span className="text-[var(--text-tertiary)]">→</span>
          <span className="font-medium text-[var(--text-primary)]">{newFileName}</span>
        </div>
        <div className="flex items-center gap-2 ml-auto">
          <span className="inline-flex items-center gap-1 px-2 py-0.5 rounded-md bg-green-50 text-green-700 text-xs font-medium dark:bg-green-900/20 dark:text-green-400">
            +{parsedLines.filter((l) => l.type === 'add').length}
          </span>
          <span className="inline-flex items-center gap-1 px-2 py-0.5 rounded-md bg-red-50 text-red-700 text-xs font-medium dark:bg-red-900/20 dark:text-red-400">
            -{parsedLines.filter((l) => l.type === 'remove').length}
          </span>
        </div>
      </div>
      <div className="overflow-x-auto">
        <table className="w-full text-sm font-mono">
          <tbody>
            {parsedLines.map((line, index) => (
              <tr
                key={index}
                className={
                  line.type === 'add'
                    ? 'bg-green-50/50 dark:bg-green-900/10'
                    : line.type === 'remove'
                      ? 'bg-red-50/50 dark:bg-red-900/10'
                      : 'hover:bg-[var(--bg-hover)]'
                }
              >
                <td className="w-8 px-2 py-0.5 text-right select-none">
                  <span
                    className={
                      line.type === 'add'
                        ? 'text-green-600 dark:text-green-400 font-bold'
                        : line.type === 'remove'
                          ? 'text-red-600 dark:text-red-400 font-bold'
                          : 'text-[var(--text-tertiary)]'
                    }
                  >
                    {line.type === 'add' ? '+' : line.type === 'remove' ? '-' : ' '}
                  </span>
                </td>
                <td className="px-3 py-0.5 text-[var(--text-primary)] whitespace-pre">
                  {line.content}
                </td>
              </tr>
            ))}
          </tbody>
        </table>
      </div>
    </div>
  );
}

export default DiffViewer;
