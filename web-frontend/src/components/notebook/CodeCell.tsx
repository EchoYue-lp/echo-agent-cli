import type { Cell } from './NotebookPanel';

interface Props {
  cell: Cell;
  onChange: (content: string) => void;
  onRun: () => void;
}

export default function CodeCell({ cell, onChange, onRun }: Props) {
  return (
    <div>
      <div className="flex items-start">
        <span
          className="px-2 py-1 text-xs font-mono select-none"
          style={{ color: 'var(--text-secondary)' }}
        >
          In:
        </span>
        <textarea
          value={cell.content}
          onChange={(e) => onChange(e.target.value)}
          onKeyDown={(e) => {
            if (e.key === 'Enter' && (e.ctrlKey || e.metaKey)) {
              e.preventDefault();
              onRun();
            }
          }}
          className="flex-1 p-2 font-mono text-sm resize-none outline-none"
          style={{ background: 'var(--bg-code, #1e1e1e)', color: '#d4d4d4', minHeight: '60px' }}
          rows={Math.max(3, cell.content.split('\n').length)}
        />
      </div>
      {cell.output && (
        <div
          className="border-t px-4 py-2 font-mono text-sm whitespace-pre-wrap"
          style={{
            borderColor: 'var(--border-primary)',
            background: 'var(--bg-secondary)',
            color: 'var(--text-primary)',
          }}
        >
          {cell.output}
        </div>
      )}
      {cell.isRunning && (
        <div
          className="border-t px-4 py-2 text-xs animate-pulse"
          style={{ color: 'var(--text-secondary)' }}
        >
          Running...
        </div>
      )}
    </div>
  );
}
