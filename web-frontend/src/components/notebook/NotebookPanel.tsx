import { useState, useEffect, useCallback } from 'react';
import CodeCell from './CodeCell';
import MarkdownCell from './MarkdownCell';
import { Plus, Play, Trash2, Save } from 'lucide-react';

export interface Cell {
  id: string;
  type: 'code' | 'markdown';
  content: string;
  output?: string;
  isRunning?: boolean;
}

const NOTEBOOK_STORAGE_KEY = 'echo_notebook_cells';

function loadNotebook(): Cell[] {
  try {
    const saved = localStorage.getItem(NOTEBOOK_STORAGE_KEY);
    if (saved) {
      const parsed = JSON.parse(saved);
      if (Array.isArray(parsed) && parsed.length > 0) return parsed;
    }
  } catch {
    /* ignore corrupt data */
  }
  return [
    { id: '1', type: 'code', content: '# Welcome to Echo Notebook\nprint("Hello!")', output: '' },
    { id: '2', type: 'markdown', content: '## Notes\nAdd your analysis notes here.' },
  ];
}

function saveNotebook(cells: Cell[]) {
  try {
    localStorage.setItem(NOTEBOOK_STORAGE_KEY, JSON.stringify(cells));
  } catch {
    /* storage full or unavailable */
  }
}

export default function NotebookPanel() {
  const [cells, setCells] = useState<Cell[]>(loadNotebook);
  const [saved, setSaved] = useState(true);

  useEffect(() => {
    saveNotebook(cells);
    setSaved(true);
  }, [cells]);

  const updateCells = useCallback((updater: (prev: Cell[]) => Cell[]) => {
    setCells(updater);
    setSaved(false);
  }, []);

  const addCell = (type: 'code' | 'markdown') => {
    const newCell: Cell = {
      id: `cell-${Date.now()}-${Math.random().toString(36).slice(2, 6)}`,
      type,
      content: '',
      output: '',
    };
    updateCells((prev) => [...prev, newCell]);
  };

  const updateCell = (id: string, content: string) => {
    updateCells((prev) => prev.map((c) => (c.id === id ? { ...c, content } : c)));
  };

  const deleteCell = (id: string) => {
    updateCells((prev) => prev.filter((c) => c.id !== id));
  };

  const runCell = async (id: string) => {
    const cell = cells.find((c) => c.id === id);
    if (!cell || cell.type !== 'code') return;

    updateCells((prev) => prev.map((c) => (c.id === id ? { ...c, isRunning: true } : c)));

    // Code execution is now handled by the Agent through conversation
    // Users should ask the Agent to execute shell commands instead
    const output =
      '💡 代码执行请通过 Agent 对话使用 shell 工具。\n\n例如：\n- "执行 ls -la 命令"\n- "运行 python script.py"';

    updateCells((prev) => prev.map((c) => (c.id === id ? { ...c, output, isRunning: false } : c)));
  };

  return (
    <div className="flex flex-col h-full" style={{ background: 'var(--bg-primary)' }}>
      {/* Toolbar */}
      <div
        className="flex items-center gap-2 p-2 border-b"
        style={{ borderColor: 'var(--border-primary)' }}
      >
        <button
          onClick={() => addCell('code')}
          className="flex items-center gap-1 px-2 py-1 rounded text-sm hover:opacity-80"
          style={{ background: 'var(--bg-secondary)', color: 'var(--text-primary)' }}
        >
          <Plus size={14} /> Code
        </button>
        <button
          onClick={() => addCell('markdown')}
          className="flex items-center gap-1 px-2 py-1 rounded text-sm hover:opacity-80"
          style={{ background: 'var(--bg-secondary)', color: 'var(--text-primary)' }}
        >
          <Plus size={14} /> Markdown
        </button>
        <span
          className="ml-auto flex items-center gap-2 text-xs"
          style={{ color: 'var(--text-secondary)' }}
        >
          <Save size={12} style={{ color: saved ? 'var(--text-tertiary)' : 'var(--accent)' }} />
          {cells.length} cells
        </span>
      </div>

      {/* Cell list */}
      <div className="flex-1 overflow-y-auto p-4 space-y-3">
        {cells.map((cell) => (
          <div
            key={cell.id}
            className="group relative rounded-lg border"
            style={{ borderColor: 'var(--border-primary)' }}
          >
            {/* Cell toolbar */}
            <div className="absolute -top-2 right-2 flex gap-1 opacity-0 group-hover:opacity-100 transition-opacity">
              {cell.type === 'code' && (
                <button
                  onClick={() => runCell(cell.id)}
                  className="p-1 rounded bg-green-600 text-white hover:bg-green-700"
                  title="Run"
                >
                  <Play size={12} />
                </button>
              )}
              <button
                onClick={() => deleteCell(cell.id)}
                className="p-1 rounded bg-red-600 text-white hover:bg-red-700"
                title="Delete"
              >
                <Trash2 size={12} />
              </button>
            </div>

            {cell.type === 'code' ? (
              <CodeCell
                cell={cell}
                onChange={(content) => updateCell(cell.id, content)}
                onRun={() => runCell(cell.id)}
              />
            ) : (
              <MarkdownCell cell={cell} onChange={(content) => updateCell(cell.id, content)} />
            )}
          </div>
        ))}
      </div>
    </div>
  );
}
