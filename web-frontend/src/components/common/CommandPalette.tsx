import { useState, useEffect, useRef, useCallback } from 'react';
import { Search, Command } from 'lucide-react';
import { Modal } from './Modal';

export interface CommandItem {
  id: string;
  label: string;
  description?: string;
  icon?: string;
  action: () => void;
  category: string;
}

interface Props {
  isOpen: boolean;
  onClose: () => void;
  commands: CommandItem[];
}

export default function CommandPalette({ isOpen, onClose, commands }: Props) {
  const [query, setQuery] = useState('');
  const [selectedIndex, setSelectedIndex] = useState(0);
  const inputRef = useRef<HTMLInputElement>(null);

  const filtered = commands.filter(
    (cmd) =>
      cmd.label.toLowerCase().includes(query.toLowerCase()) ||
      cmd.description?.toLowerCase().includes(query.toLowerCase())
  );

  useEffect(() => {
    if (isOpen) {
      setQuery('');
      setSelectedIndex(0);
      setTimeout(() => inputRef.current?.focus(), 50);
    }
  }, [isOpen]);

  // Reset selected index when filtered results change
  useEffect(() => {
    setSelectedIndex(0);
  }, [query]);

  const handleKeyDown = useCallback(
    (e: React.KeyboardEvent) => {
      if (e.key === 'ArrowDown') {
        e.preventDefault();
        setSelectedIndex((i) => Math.min(i + 1, filtered.length - 1));
      } else if (e.key === 'ArrowUp') {
        e.preventDefault();
        setSelectedIndex((i) => Math.max(i - 1, 0));
      } else if (e.key === 'Enter') {
        e.preventDefault();
        if (filtered[selectedIndex]) {
          filtered[selectedIndex].action();
          onClose();
        }
      } else if (e.key === 'Escape') {
        onClose();
      }
    },
    [filtered, selectedIndex, onClose]
  );

  if (!isOpen) return null;

  // Group by category
  const grouped = filtered.reduce<Record<string, CommandItem[]>>((acc, cmd) => {
    if (!acc[cmd.category]) acc[cmd.category] = [];
    acc[cmd.category].push(cmd);
    return acc;
  }, {});

  let flatIndex = 0;

  return (
    <Modal
      onClose={onClose}
      ariaLabel="命令面板"
      initialFocusRef={inputRef}
      overlayClassName="items-start pt-20"
      className="relative w-full max-w-lg overflow-hidden rounded-xl bg-[var(--bg-primary)] shadow-[var(--shadow-xl)]"
    >
      {/* Search input */}
      <div
        className="flex items-center gap-2 px-4 py-3 border-b"
        style={{ borderColor: 'var(--border-primary)' }}
      >
        <Search size={16} style={{ color: 'var(--text-secondary)' }} />
        <input
          ref={inputRef}
          type="text"
          value={query}
          onChange={(e) => setQuery(e.target.value)}
          onKeyDown={handleKeyDown}
          placeholder="Type a command..."
          aria-label="搜索命令"
          className="flex-1 bg-transparent outline-none text-sm"
          style={{ color: 'var(--text-primary)' }}
        />
        <kbd
          className="text-xs px-1.5 py-0.5 rounded-md"
          style={{
            background: 'var(--bg-secondary)',
            color: 'var(--text-secondary)',
          }}
        >
          ESC
        </kbd>
      </div>

      {/* Results */}
      <div className="max-h-80 overflow-y-auto py-2">
        {filtered.length === 0 ? (
          <div className="px-4 py-8 text-center text-sm" style={{ color: 'var(--text-secondary)' }}>
            No results found
          </div>
        ) : (
          Object.entries(grouped).map(([category, items]) => (
            <div key={category}>
              <div
                className="px-4 py-1 text-xs font-medium uppercase tracking-wider"
                style={{ color: 'var(--text-tertiary)' }}
              >
                {category}
              </div>
              {items.map((cmd) => {
                const idx = flatIndex++;
                return (
                  <button
                    key={cmd.id}
                    className={`w-full flex items-center gap-3 px-4 py-2 text-left text-sm transition-colors ${
                      idx === selectedIndex ? 'bg-[var(--accent)]/10' : ''
                    }`}
                    style={{ color: 'var(--text-primary)' }}
                    onClick={() => {
                      cmd.action();
                      onClose();
                    }}
                    onMouseEnter={() => setSelectedIndex(idx)}
                  >
                    <Command size={14} style={{ color: 'var(--text-secondary)' }} />
                    <div className="flex-1 min-w-0">
                      <div className="truncate">{cmd.label}</div>
                      {cmd.description && (
                        <div
                          className="text-xs truncate"
                          style={{ color: 'var(--text-secondary)' }}
                        >
                          {cmd.description}
                        </div>
                      )}
                    </div>
                    <span
                      className="ml-auto text-xs shrink-0"
                      style={{ color: 'var(--text-tertiary)' }}
                    >
                      {cmd.category}
                    </span>
                  </button>
                );
              })}
            </div>
          ))
        )}
      </div>
    </Modal>
  );
}
