import { useState, useRef, useEffect, useCallback, useMemo } from 'react';
import { ArrowUp, Square, Paperclip, X, File, Terminal } from 'lucide-react';
import type { Attachment } from '../../types/api';
import {
  filterCommands,
  groupByCategory,
  CATEGORY_META,
  type SlashCommand,
} from '../../lib/slashCommands';

interface PendingFile {
  id: string;
  file: File;
  previewUrl: string;
  name: string;
  mime_type: string;
  size: number;
}

interface ChatInputProps {
  onSend: (text: string, attachments?: Attachment[]) => void;
  isStreaming?: boolean;
  onCancel?: () => void;
}

const MAX_FILE_SIZE = 10 * 1024 * 1024; // 10MB

function fileToBase64(file: File): Promise<string> {
  return new Promise((resolve, reject) => {
    const reader = new FileReader();
    reader.onload = () => {
      const result = reader.result as string;
      resolve(result.split(',')[1]); // strip data:...;base64, prefix
    };
    reader.onerror = reject;
    reader.readAsDataURL(file);
  });
}

function isImageFile(mime: string): boolean {
  return mime.startsWith('image/');
}

/**
 * Determine the active slash-command query from the input text.
 * Returns the query string (e.g. "/per") only when the user is still
 * typing the command token (no space yet, cursor at end, starts with /).
 */
function getSlashQuery(text: string, cursorPos: number | null): string | null {
  if (!text.startsWith('/')) return null;
  const firstSpace = text.indexOf(' ');
  // If there's a space, the user has moved past the command token
  if (firstSpace !== -1) return null;
  // Only show palette when cursor is at end (still typing the command)
  if (cursorPos !== null && cursorPos < text.length) return null;
  return text;
}

// ─── Slash Command Palette ──────────────────────────────────────────────────

interface CommandPaletteProps {
  commands: SlashCommand[];
  selectedIndex: number;
  onSelect: (cmd: SlashCommand) => void;
}

function CommandPalette({ commands, selectedIndex, onSelect }: CommandPaletteProps) {
  const listRef = useRef<HTMLDivElement>(null);

  // Keep selected item scrolled into view
  useEffect(() => {
    if (!listRef.current) return;
    const items = listRef.current.querySelectorAll('[data-cmd-item]');
    items[selectedIndex]?.scrollIntoView({ block: 'nearest' });
  }, [selectedIndex]);

  if (commands.length === 0) {
    return (
      <div className="absolute bottom-full left-0 right-0 z-50 mb-2 px-4">
        <div className="glass mx-auto max-w-3xl rounded-xl border border-[var(--border-primary)] bg-[var(--bg-primary)] p-3 shadow-[var(--shadow-md)]">
          <p className="text-center text-xs text-[var(--text-tertiary)]">No matching commands</p>
        </div>
      </div>
    );
  }

  const grouped = groupByCategory(commands);
  // Flatten to compute per-item indices that match the parent's selectedIndex
  let flatIndex = -1;

  return (
    <div className="absolute bottom-full left-0 right-0 z-50 mb-2 px-4">
      <div className="glass mx-auto max-w-3xl rounded-xl border border-[var(--border-primary)] bg-[var(--bg-primary)] shadow-[var(--shadow-md)]">
        {/* Header */}
        <div className="flex items-center gap-2 border-b border-[var(--border-primary)] px-4 py-2">
          <Terminal size={13} className="text-[var(--text-tertiary)]" />
          <span className="text-xs font-medium text-[var(--text-secondary)]">Slash Commands</span>
          <span className="ml-auto text-[10px] text-[var(--text-tertiary)]">
            ↑↓ navigate · ↵ select · esc close
          </span>
        </div>

        {/* Command list */}
        <div ref={listRef} className="max-h-[280px] overflow-y-auto px-2 py-2">
          {Array.from(grouped.entries()).map(([category, cmds]) => {
            const meta = CATEGORY_META[category];
            return (
              <div key={category} className="mb-1">
                <div className="flex items-center gap-1.5 px-2 py-1 text-[10px] font-semibold uppercase tracking-wider text-[var(--text-tertiary)]">
                  <span>{meta?.icon ?? '📌'}</span>
                  <span>{category}</span>
                </div>
                {cmds.map((cmd) => {
                  flatIndex++;
                  const idx = flatIndex;
                  const isSelected = idx === selectedIndex;
                  return (
                    <button
                      key={cmd.name}
                      data-cmd-item
                      onClick={() => onSelect(cmd)}
                      className={`flex w-full items-center gap-3 rounded-lg px-3 py-2 text-left transition-colors ${
                        isSelected
                          ? 'bg-[var(--accent)]/10 text-[var(--accent)]'
                          : 'text-[var(--text-primary)] hover:bg-[var(--bg-hover)]'
                      }`}
                    >
                      <code
                        className={`shrink-0 rounded px-1.5 py-0.5 text-xs font-semibold ${
                          isSelected
                            ? 'bg-[var(--accent)]/15 text-[var(--accent)]'
                            : 'bg-[var(--bg-secondary)] text-[var(--text-secondary)]'
                        }`}
                      >
                        {cmd.name}
                      </code>
                      <span className="truncate text-xs text-[var(--text-secondary)]">
                        {cmd.description}
                      </span>
                      {cmd.aliases.length > 0 && (
                        <span className="ml-auto shrink-0 text-[10px] text-[var(--text-tertiary)]">
                          {cmd.aliases.join(', ')}
                        </span>
                      )}
                    </button>
                  );
                })}
              </div>
            );
          })}
        </div>
      </div>
    </div>
  );
}

// ─── Main ChatInput ──────────────────────────────────────────────────────────

export function ChatInput({ onSend, isStreaming, onCancel }: ChatInputProps) {
  const [text, setText] = useState('');
  const [pendingFiles, setPendingFiles] = useState<PendingFile[]>([]);
  const [paletteSelectedIndex, setPaletteSelectedIndex] = useState(0);
  const textareaRef = useRef<HTMLTextAreaElement>(null);
  const fileInputRef = useRef<HTMLInputElement>(null);

  // Auto-resize textarea
  useEffect(() => {
    const el = textareaRef.current;
    if (!el) return;
    el.style.height = 'auto';
    el.style.height = Math.min(el.scrollHeight, 200) + 'px';
  }, [text]);

  // ── Slash command palette state ──

  const cursorPos = textareaRef.current?.selectionStart ?? null;
  const slashQuery = getSlashQuery(text, cursorPos);
  const filteredCommands = useMemo(
    () => (slashQuery !== null ? filterCommands(slashQuery) : []),
    [slashQuery]
  );
  const showPalette = slashQuery !== null;

  // Reset selection whenever the filtered list changes
  useEffect(() => {
    setPaletteSelectedIndex(0);
  }, [slashQuery]);

  // ── File handling ──

  const addFiles = useCallback((files: FileList | File[]) => {
    const newFiles: PendingFile[] = [];
    for (const file of Array.from(files)) {
      if (file.size > MAX_FILE_SIZE) {
        alert(`File "${file.name}" exceeds the 10MB limit and was skipped`);
        continue;
      }
      newFiles.push({
        id: `pending-${Date.now()}-${Math.random().toString(36).slice(2, 8)}`,
        file,
        previewUrl: isImageFile(file.type) ? URL.createObjectURL(file) : '',
        name: file.name,
        mime_type: file.type || 'application/octet-stream',
        size: file.size,
      });
    }
    setPendingFiles((prev) => [...prev, ...newFiles]);
  }, []);

  const removeFile = useCallback((id: string) => {
    setPendingFiles((prev) => {
      const file = prev.find((f) => f.id === id);
      if (file?.previewUrl) URL.revokeObjectURL(file.previewUrl);
      return prev.filter((f) => f.id !== id);
    });
  }, []);

  const handleFileSelect = useCallback(
    (e: React.ChangeEvent<HTMLInputElement>) => {
      if (e.target.files?.length) {
        addFiles(e.target.files);
        e.target.value = '';
      }
    },
    [addFiles]
  );

  const handlePaste = useCallback(
    (e: React.ClipboardEvent<HTMLTextAreaElement>) => {
      const items = e.clipboardData?.items;
      if (!items) return;

      const imageFiles: File[] = [];
      for (let i = 0; i < items.length; i++) {
        const item = items[i];
        if (item.type.startsWith('image/')) {
          const file = item.getAsFile();
          if (file) imageFiles.push(file);
        }
      }
      if (imageFiles.length > 0) {
        e.preventDefault();
        addFiles(imageFiles);
      }
    },
    [addFiles]
  );

  // ── Send ──

  const handleSend = async () => {
    const trimmed = text.trim();
    if (!trimmed && pendingFiles.length === 0) return;

    // If palette is open and there are filtered commands, select the highlighted one
    if (showPalette && filteredCommands.length > 0) {
      const cmd = filteredCommands[paletteSelectedIndex];
      if (cmd) {
        selectCommand(cmd);
        return;
      }
    }

    const attachmentData: Attachment[] = await Promise.all(
      pendingFiles.map(async (pf) => ({
        name: pf.name,
        mime_type: pf.mime_type,
        data: await fileToBase64(pf.file),
        size: pf.size,
      }))
    );

    // Release preview URLs
    pendingFiles.forEach((pf) => {
      if (pf.previewUrl) URL.revokeObjectURL(pf.previewUrl);
    });

    onSend(trimmed, attachmentData.length > 0 ? attachmentData : undefined);
    setText('');
    setPendingFiles([]);

    requestAnimationFrame(() => {
      if (textareaRef.current) textareaRef.current.style.height = 'auto';
    });
  };

  // ── Command selection ──

  const selectCommand = useCallback((cmd: SlashCommand) => {
    // Place the command name into the text input with a trailing space
    // so the user can append arguments, or just press Enter to send.
    setText(cmd.name + ' ');
    setPaletteSelectedIndex(0);
    // Refocus the textarea
    requestAnimationFrame(() => {
      textareaRef.current?.focus();
    });
  }, []);

  // ── Keyboard handling ──

  const handleKeyDown = (e: React.KeyboardEvent<HTMLTextAreaElement>) => {
    // Palette navigation takes priority when palette is visible
    if (showPalette) {
      if (e.key === 'Escape') {
        e.preventDefault();
        // Clear the slash query by removing the text
        setText('');
        return;
      }

      if (e.key === 'ArrowDown') {
        e.preventDefault();
        setPaletteSelectedIndex((prev) => (prev < filteredCommands.length - 1 ? prev + 1 : 0));
        return;
      }

      if (e.key === 'ArrowUp') {
        e.preventDefault();
        setPaletteSelectedIndex((prev) => (prev > 0 ? prev - 1 : filteredCommands.length - 1));
        return;
      }

      if (e.key === 'Tab') {
        e.preventDefault();
        if (filteredCommands.length > 0) {
          selectCommand(filteredCommands[paletteSelectedIndex]);
        }
        return;
      }

      // Enter: if palette has matches, select the highlighted command
      if (e.key === 'Enter' && !e.shiftKey) {
        if (filteredCommands.length > 0) {
          e.preventDefault();
          selectCommand(filteredCommands[paletteSelectedIndex]);
          return;
        }
        // If no matches, fall through to normal send
      }
    }

    // Normal send on Enter (no Shift)
    if (e.key === 'Enter' && !e.shiftKey) {
      e.preventDefault();
      handleSend();
    }
  };

  // ── Helpers ──

  const formatFileSize = (bytes: number): string => {
    if (bytes < 1024) return `${bytes} B`;
    if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(1)} KB`;
    return `${(bytes / (1024 * 1024)).toFixed(1)} MB`;
  };

  const [isDragging, setIsDragging] = useState(false);

  // ── Drag & drop handling ──
  const handleDragOver = useCallback((e: React.DragEvent) => {
    e.preventDefault();
    e.stopPropagation();
    setIsDragging(true);
  }, []);

  const handleDragLeave = useCallback((e: React.DragEvent) => {
    e.preventDefault();
    e.stopPropagation();
    setIsDragging(false);
  }, []);

  const handleDrop = useCallback(
    (e: React.DragEvent) => {
      e.preventDefault();
      e.stopPropagation();
      setIsDragging(false);
      if (e.dataTransfer.files?.length) {
        addFiles(e.dataTransfer.files);
      }
    },
    [addFiles]
  );

  const hasContent = text.trim().length > 0 || pendingFiles.length > 0;

  return (
    <div
      className="px-4 pb-4 pt-2"
      onDragOver={handleDragOver}
      onDragLeave={handleDragLeave}
      onDrop={handleDrop}
    >
      {/* Relative container so the absolute-positioned palette anchors here */}
      <div className="relative mx-auto max-w-3xl">
        {/* ── Slash Command Palette ── */}
        {showPalette && (
          <CommandPalette
            commands={filteredCommands}
            selectedIndex={paletteSelectedIndex}
            onSelect={selectCommand}
          />
        )}

        <div className="glass flex flex-col rounded-2xl shadow-[var(--shadow-sm)] transition-shadow focus-within:shadow-[var(--shadow-md)] relative">
          {/* Drag overlay */}
          {isDragging && (
            <div className="absolute inset-0 z-10 flex items-center justify-center rounded-2xl border-2 border-dashed border-[var(--accent)] bg-[var(--accent)]/5">
              <span className="text-sm font-medium text-[var(--accent)]">
                Drop files here to upload
              </span>
            </div>
          )}
          {/* Attachment previews */}
          {pendingFiles.length > 0 && (
            <div className="flex flex-wrap gap-2 px-4 pt-3">
              {pendingFiles.map((pf) => (
                <div
                  key={pf.id}
                  className="group relative flex items-center gap-2 rounded-lg border border-[var(--border-primary)] bg-[var(--bg-secondary)] p-2 pr-8"
                >
                  {pf.previewUrl ? (
                    <div className="h-10 w-10 shrink-0 overflow-hidden rounded-md">
                      <img
                        src={pf.previewUrl}
                        alt={pf.name}
                        className="h-full w-full object-cover"
                      />
                    </div>
                  ) : (
                    <div className="flex h-10 w-10 shrink-0 items-center justify-center rounded-md bg-[var(--bg-hover)]">
                      <File size={16} className="text-[var(--text-tertiary)]" />
                    </div>
                  )}
                  <div className="min-w-0 max-w-[120px]">
                    <div className="truncate text-xs font-medium text-[var(--text-primary)]">
                      {pf.name}
                    </div>
                    <div className="text-[10px] text-[var(--text-tertiary)]">
                      {formatFileSize(pf.size)}
                    </div>
                  </div>
                  <button
                    onClick={() => removeFile(pf.id)}
                    className="absolute right-1 top-1 flex h-5 w-5 items-center justify-center rounded-full bg-[var(--bg-primary)] text-[var(--text-tertiary)] opacity-0 shadow-[var(--shadow-sm)] transition-opacity group-hover:opacity-100 hover:text-[var(--text-primary)]"
                  >
                    <X size={10} />
                  </button>
                </div>
              ))}
            </div>
          )}

          {/* Input row */}
          <div className="flex items-end px-4 py-3">
            {/* Attachment button */}
            <button
              onClick={() => fileInputRef.current?.click()}
              className="mr-1 flex h-8 w-8 shrink-0 items-center justify-center rounded-lg text-[var(--text-tertiary)] transition-colors hover:bg-[var(--bg-hover)] hover:text-[var(--text-primary)]"
              title="Upload attachment"
            >
              <Paperclip size={16} />
            </button>
            <input
              ref={fileInputRef}
              type="file"
              multiple
              accept="image/*,.pdf,.txt,.doc,.docx,.xls,.xlsx,.csv,.json,.xml,.yaml,.yml,.md,.log"
              className="hidden"
              onChange={handleFileSelect}
            />

            <textarea
              ref={textareaRef}
              value={text}
              onChange={(e) => setText(e.target.value)}
              onKeyDown={handleKeyDown}
              onPaste={handlePaste}
              rows={1}
              placeholder="Send a message, or type / for commands..."
              className="max-h-[200px] min-h-[24px] flex-1 resize-none bg-transparent text-sm leading-relaxed text-[var(--text-primary)] outline-none placeholder:text-[var(--text-tertiary)]"
            />
            {isStreaming ? (
              <button
                onClick={onCancel}
                className="ml-2 flex h-8 w-8 shrink-0 items-center justify-center rounded-lg bg-[var(--text-secondary)] text-white transition-all hover:bg-[var(--text-primary)]"
                title="Stop generating"
              >
                <Square size={14} fill="white" color="white" />
              </button>
            ) : (
              <button
                onClick={handleSend}
                disabled={!hasContent}
                className="ml-2 flex h-8 w-8 shrink-0 items-center justify-center rounded-lg bg-[var(--accent)] text-white transition-all hover:shadow-[var(--shadow-glow)] disabled:opacity-20 disabled:hover:shadow-none"
              >
                <ArrowUp size={16} strokeWidth={2.5} />
              </button>
            )}
          </div>
        </div>
      </div>
      <div className="mx-auto mt-2 flex max-w-3xl items-center justify-between text-[11px] text-[var(--text-tertiary)]">
        <span>EchoCoWork may make mistakes; verify important information</span>
        {text.length > 0 && <span>{text.length} chars</span>}
      </div>
    </div>
  );
}
