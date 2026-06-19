import { useState } from 'react';
import type { Cell } from './NotebookPanel';
import MarkdownContent from '../common/MarkdownContent';

interface Props {
  cell: Cell;
  onChange: (content: string) => void;
}

export default function MarkdownCell({ cell, onChange }: Props) {
  const [editing, setEditing] = useState(!cell.content);

  if (editing) {
    return (
      <textarea
        value={cell.content}
        onChange={(e) => onChange(e.target.value)}
        onBlur={() => cell.content && setEditing(false)}
        className="w-full p-3 text-sm resize-none outline-none"
        style={{ background: 'transparent', color: 'var(--text-primary)', minHeight: '60px' }}
        autoFocus
        rows={Math.max(3, cell.content.split('\n').length)}
      />
    );
  }

  return (
    <div
      className="p-3 text-sm cursor-text prose prose-sm max-w-none"
      style={{ color: 'var(--text-primary)' }}
      onClick={() => setEditing(true)}
    >
      <MarkdownContent content={cell.content} />
    </div>
  );
}
