import { useState, useRef, useEffect, useCallback, useMemo } from 'react';
import {
  ArrowUp,
  Square,
  Paperclip,
  X,
  File,
  Terminal,
  ShieldCheck,
  Cpu,
  Brain,
  Workflow,
  ChevronDown,
  Check,
} from 'lucide-react';
import { permissionsApi, providerApi, taskRuntimeApi } from '../../api/endpoints';
import { useUiStore } from '../../stores/uiStore';
import type { Attachment, ConfiguredModel } from '../../types/api';
import {
  filterCommands,
  groupByCategory,
  CATEGORY_META,
  type SlashCommand,
} from '../../lib/slashCommands';
import {
  PERMISSION_MODES,
  PERMISSIONS_CHANGED_EVENT,
  normalizePermissionMode,
  notifyPermissionsChanged,
} from '../../lib/permissionModes';

/**
 * 思考深度选项。与模型解耦:所有模型都展示这个下拉,后端按
 * ThinkingProtocol 决定是否真正下发(不支持的模型静默忽略 + warn),
 * 所以控制始终安全暴露。
 *
 * 持久化在 localStorage,默认 'auto'(模型默认行为,不发 thinking 字段)。
 */
const THINKING_LEVELS = [
  { id: 'auto', label: '自动' },
  { id: 'minimal', label: '最低' },
  { id: 'low', label: '低' },
  { id: 'medium', label: '中' },
  { id: 'high', label: '高' },
] as const;
const THINKING_STORAGE_KEY = 'echo_thinking_level';
const INTERACTION_MODES = [
  { id: 0, label: 'Auto' },
  { id: 1, label: 'Chat' },
  { id: 2, label: 'Task' },
] as const;
function loadThinkingLevel(): string {
  try {
    const v = localStorage.getItem(THINKING_STORAGE_KEY);
    if (v && THINKING_LEVELS.some((l) => l.id === v)) return v;
  } catch {
    /* localStorage may be unavailable */
  }
  return 'auto';
}

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

const MODELS_CHANGED_EVENT = 'echocowork:models-changed';

function notifyModelsChanged() {
  window.dispatchEvent(new Event(MODELS_CHANGED_EVENT));
}

export function ChatInput({ onSend, isStreaming, onCancel }: ChatInputProps) {
  const [text, setText] = useState('');
  const [pendingFiles, setPendingFiles] = useState<PendingFile[]>([]);
  const [paletteSelectedIndex, setPaletteSelectedIndex] = useState(0);
  const [configuredModels, setConfiguredModels] = useState<ConfiguredModel[]>([]);
  const [modelMenuOpen, setModelMenuOpen] = useState(false);
  const [switchingModelId, setSwitchingModelId] = useState<string | null>(null);
  const [permissionMode, setPermissionMode] = useState('default');
  const [permissionMenuOpen, setPermissionMenuOpen] = useState(false);
  const [switchingPermissionMode, setSwitchingPermissionMode] = useState<string | null>(null);
  const [thinkingLevel, setThinkingLevel] = useState<string>(loadThinkingLevel);
  const [thinkingMenuOpen, setThinkingMenuOpen] = useState(false);
  const [switchingThinking, setSwitchingThinking] = useState(false);
  const [interactionMode, setInteractionMode] = useState<number>(0);
  const [switchingInteractionMode, setSwitchingInteractionMode] = useState<number | null>(null);
  const textareaRef = useRef<HTMLTextAreaElement>(null);
  const fileInputRef = useRef<HTMLInputElement>(null);
  const setActiveSettingsTab = useUiStore((s) => s.setActiveSettingsTab);
  const activeModel = configuredModels.find((model) => model.is_default);
  const visibleModels = configuredModels.filter((model) => model.enabled);
  const displayModel = activeModel ?? visibleModels[0] ?? null;
  const activePermissionMode =
    PERMISSION_MODES.find((mode) => mode.id === permissionMode) ?? PERMISSION_MODES[0];

  const loadConfiguredModels = useCallback(async () => {
    try {
      const res = await providerApi.listConfigured();
      setConfiguredModels(res.models);
    } catch (e) {
      console.error('[ChatInput] Failed to load configured models:', e);
    }
  }, []);

  useEffect(() => {
    loadConfiguredModels();
  }, [loadConfiguredModels]);

  const loadPermissionMode = useCallback(async () => {
    try {
      const res = await permissionsApi.getMode();
      setPermissionMode(normalizePermissionMode(res.mode));
    } catch (e) {
      console.error('[ChatInput] Failed to load permission mode:', e);
    }
  }, []);

  useEffect(() => {
    loadPermissionMode();
  }, [loadPermissionMode]);

  const loadInteractionMode = useCallback(async () => {
    try {
      const mode = await taskRuntimeApi.getInteractionMode();
      setInteractionMode(INTERACTION_MODES.some((m) => m.id === mode) ? mode : 0);
    } catch (e) {
      console.error('[ChatInput] Failed to load interaction mode:', e);
    }
  }, []);

  useEffect(() => {
    loadInteractionMode();
  }, [loadInteractionMode]);

  useEffect(() => {
    const refreshPermissionMode = () => {
      void loadPermissionMode();
    };
    window.addEventListener(PERMISSIONS_CHANGED_EVENT, refreshPermissionMode);
    window.addEventListener('focus', refreshPermissionMode);
    return () => {
      window.removeEventListener(PERMISSIONS_CHANGED_EVENT, refreshPermissionMode);
      window.removeEventListener('focus', refreshPermissionMode);
    };
  }, [loadPermissionMode]);

  useEffect(() => {
    const refreshModels = () => {
      void loadConfiguredModels();
    };
    window.addEventListener(MODELS_CHANGED_EVENT, refreshModels);
    window.addEventListener('focus', refreshModels);
    return () => {
      window.removeEventListener(MODELS_CHANGED_EVENT, refreshModels);
      window.removeEventListener('focus', refreshModels);
    };
  }, [loadConfiguredModels]);

  const openModelSettings = useCallback(() => {
    setModelMenuOpen(false);
    setActiveSettingsTab('providers');
  }, [setActiveSettingsTab]);

  const switchModel = useCallback(
    async (model: ConfiguredModel) => {
      if (model.is_default || switchingModelId) return;
      setSwitchingModelId(model.id);
      try {
        await providerApi.setDefault(model.id);
        await loadConfiguredModels();
        notifyModelsChanged();
        setModelMenuOpen(false);
      } catch (e) {
        console.error('[ChatInput] Failed to switch model:', e);
      } finally {
        setSwitchingModelId(null);
      }
    },
    [loadConfiguredModels, switchingModelId]
  );

  const switchPermissionMode = useCallback(
    async (mode: string) => {
      if (mode === permissionMode || switchingPermissionMode) return;
      setSwitchingPermissionMode(mode);
      try {
        await permissionsApi.setMode(mode);
        setPermissionMode(mode);
        notifyPermissionsChanged();
        setPermissionMenuOpen(false);
      } catch (e) {
        console.error('[ChatInput] Failed to switch permission mode:', e);
      } finally {
        setSwitchingPermissionMode(null);
      }
    },
    [permissionMode, switchingPermissionMode]
  );

  const switchInteractionMode = useCallback(
    async (mode: number) => {
      if (mode === interactionMode || switchingInteractionMode !== null) return;
      setSwitchingInteractionMode(mode);
      try {
        const next = await taskRuntimeApi.setInteractionMode(mode);
        setInteractionMode(INTERACTION_MODES.some((m) => m.id === next) ? next : mode);
      } catch (e) {
        console.error('[ChatInput] Failed to switch interaction mode:', e);
      } finally {
        setSwitchingInteractionMode(null);
      }
    },
    [interactionMode, switchingInteractionMode]
  );

  // Switch the active agent's thinking-depth at runtime. Decoupled from model
  // config — every model exposes this; unsupported ones silently ignore it.
  const switchThinkingLevel = useCallback(
    async (level: string) => {
      if (level === thinkingLevel || switchingThinking) return;
      setSwitchingThinking(true);
      try {
        await providerApi.setThinking(level);
        setThinkingLevel(level);
        try {
          localStorage.setItem(THINKING_STORAGE_KEY, level);
        } catch {
          /* ignore persistence failure */
        }
        setThinkingMenuOpen(false);
      } catch (e) {
        console.error('[ChatInput] Failed to switch thinking level:', e);
      } finally {
        setSwitchingThinking(false);
      }
    },
    [thinkingLevel, switchingThinking]
  );

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
      className="px-5 pb-5 pt-2"
      onDragOver={handleDragOver}
      onDragLeave={handleDragLeave}
      onDrop={handleDrop}
    >
      {/* Relative container so the absolute-positioned palette anchors here */}
      <div className="relative mx-auto max-w-[920px]">
        {/* ── Slash Command Palette ── */}
        {showPalette && (
          <CommandPalette
            commands={filteredCommands}
            selectedIndex={paletteSelectedIndex}
            onSelect={selectCommand}
          />
        )}

        <div className="relative flex flex-col rounded-[20px] border border-[var(--border-primary)] bg-[var(--bg-input)] shadow-[var(--shadow-md)] transition-shadow focus-within:shadow-[var(--shadow-lg)]">
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
          <div className="flex items-end px-4 pt-3 pb-2">
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
              placeholder="继续输入以排队后续修改，或输入 / 查看命令"
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
                className="ml-2 flex h-8 w-8 shrink-0 items-center justify-center rounded-lg bg-[var(--accent)] text-[var(--text-on-accent)] transition-all hover:shadow-[var(--shadow-glow)] disabled:opacity-20 disabled:hover:shadow-none"
              >
                <ArrowUp size={16} strokeWidth={2.5} />
              </button>
            )}
          </div>
          <div className="flex items-center justify-between border-t border-[var(--border-secondary)] px-4 py-2 text-[11px] text-[var(--text-tertiary)]">
            <div className="flex min-w-0 items-center gap-3">
              <div className="relative">
                <button
                  type="button"
                  onClick={() => {
                    setPermissionMenuOpen((open) => !open);
                    setModelMenuOpen(false);
                    setThinkingMenuOpen(false);
                  }}
                  className="flex max-w-[180px] items-center gap-1.5 rounded-md px-1.5 py-1 text-[var(--text-tertiary)] transition-colors hover:bg-[var(--bg-hover)] hover:text-[var(--text-primary)]"
                  title="切换审批模式"
                >
                  <ShieldCheck size={13} />
                  <span className="truncate">{activePermissionMode.label}</span>
                  <ChevronDown size={12} />
                </button>
                {permissionMenuOpen && (
                  <div className="absolute bottom-full left-0 z-50 mb-2 w-64 overflow-hidden rounded-lg border border-[var(--border-primary)] bg-[var(--bg-primary)] shadow-[var(--shadow-md)]">
                    <div className="border-b border-[var(--border-secondary)] px-3 py-2 text-[10px] font-medium text-[var(--text-tertiary)]">
                      审批模式
                    </div>
                    <div className="p-1">
                      {PERMISSION_MODES.map((mode) => (
                        <button
                          key={mode.id}
                          type="button"
                          onClick={() => switchPermissionMode(mode.id)}
                          disabled={switchingPermissionMode === mode.id}
                          className="flex w-full items-start gap-2 rounded-md px-2 py-2 text-left transition-colors hover:bg-[var(--bg-hover)] disabled:cursor-wait disabled:opacity-70"
                        >
                          <span className="mt-0.5 flex h-4 w-4 shrink-0 items-center justify-center text-[var(--accent)]">
                            {mode.id === permissionMode && <Check size={13} />}
                          </span>
                          <span className="min-w-0 flex-1">
                            <span className="block text-xs text-[var(--text-primary)]">
                              {switchingPermissionMode === mode.id ? '切换中...' : mode.label}
                            </span>
                            <span className="block text-[10px] text-[var(--text-tertiary)]">
                              {mode.description}
                            </span>
                          </span>
                        </button>
                      ))}
                    </div>
                  </div>
                )}
              </div>
              <div className="relative hidden sm:block">
                <button
                  type="button"
                  onClick={() => {
                    setPermissionMenuOpen(false);
                    setThinkingMenuOpen(false);
                    if (visibleModels.length === 0) {
                      openModelSettings();
                    } else {
                      setModelMenuOpen((open) => !open);
                    }
                  }}
                  className="flex max-w-[220px] items-center gap-1.5 rounded-md px-1.5 py-1 text-[var(--text-tertiary)] transition-colors hover:bg-[var(--bg-hover)] hover:text-[var(--text-primary)]"
                  title="切换默认模型"
                >
                  <Cpu size={13} />
                  <span className="truncate">
                    {displayModel?.display_name || displayModel?.model || '配置模型'}
                  </span>
                  {visibleModels.length > 0 && <ChevronDown size={12} />}
                </button>
                {modelMenuOpen && visibleModels.length > 0 && (
                  <div className="absolute bottom-full left-0 z-50 mb-2 w-64 overflow-hidden rounded-lg border border-[var(--border-primary)] bg-[var(--bg-primary)] shadow-[var(--shadow-md)]">
                    <div className="border-b border-[var(--border-secondary)] px-3 py-2 text-[10px] font-medium text-[var(--text-tertiary)]">
                      默认模型
                    </div>
                    <div className="max-h-64 overflow-y-auto p-1">
                      {visibleModels.map((model) => (
                        <button
                          key={model.id}
                          type="button"
                          onClick={() => switchModel(model)}
                          disabled={switchingModelId !== null}
                          className="flex w-full items-center gap-2 rounded-md px-2 py-2 text-left text-xs text-[var(--text-primary)] transition-colors hover:bg-[var(--bg-hover)] disabled:cursor-not-allowed disabled:opacity-50"
                        >
                          <span className="flex h-4 w-4 shrink-0 items-center justify-center text-[var(--accent)]">
                            {model.is_default && <Check size={13} />}
                          </span>
                          <span className="min-w-0 flex-1 truncate">
                            {model.display_name || model.model}
                          </span>
                        </button>
                      ))}
                    </div>
                    <button
                      type="button"
                      onClick={openModelSettings}
                      className="w-full border-t border-[var(--border-secondary)] px-3 py-2 text-left text-xs text-[var(--accent)] transition-colors hover:bg-[var(--bg-hover)]"
                    >
                      管理模型
                    </button>
                  </div>
                )}
	              </div>
              <div
                className="flex shrink-0 items-center rounded-md border border-[var(--border-secondary)] p-0.5"
                title="切换交互模式"
              >
                <Workflow size={12} className="mx-1 text-[var(--text-tertiary)]" />
                {INTERACTION_MODES.map((mode) => (
                  <button
                    key={mode.id}
                    type="button"
                    onClick={() => switchInteractionMode(mode.id)}
                    disabled={switchingInteractionMode !== null}
                    className={`h-6 rounded px-1.5 text-[10px] transition-colors disabled:cursor-wait ${
                      interactionMode === mode.id
                        ? 'bg-[var(--accent)] text-[var(--text-on-accent)]'
                        : 'text-[var(--text-tertiary)] hover:bg-[var(--bg-hover)] hover:text-[var(--text-primary)]'
                    }`}
                  >
                    {switchingInteractionMode === mode.id ? '...' : mode.label}
                  </button>
                ))}
              </div>
              {/* 思考深度 — 运行时每会话控制,与模型解耦。所有模型都展示;
                  不支持的模型后端静默忽略(框架会 warn)。 */}
              <div className="relative">
                <button
                  type="button"
                  onClick={() => {
                    setPermissionMenuOpen(false);
                    setModelMenuOpen(false);
                    setThinkingMenuOpen((open) => !open);
                  }}
                  className="flex max-w-[140px] items-center gap-1.5 rounded-md px-1.5 py-1 text-[var(--text-tertiary)] transition-colors hover:bg-[var(--bg-hover)] hover:text-[var(--text-primary)]"
                  title="切换思考深度"
                >
                  <Brain size={13} />
                  <span className="truncate">
                    {THINKING_LEVELS.find((l) => l.id === thinkingLevel)?.label ?? '自动'}
                  </span>
                  <ChevronDown size={12} />
                </button>
                {thinkingMenuOpen && (
                  <div className="absolute bottom-full left-0 z-50 mb-2 w-56 overflow-hidden rounded-lg border border-[var(--border-primary)] bg-[var(--bg-primary)] shadow-[var(--shadow-md)]">
                    <div className="border-b border-[var(--border-secondary)] px-3 py-2 text-[10px] font-medium text-[var(--text-tertiary)]">
                      思考深度
                    </div>
                    <div className="p-1">
                      {THINKING_LEVELS.map((lvl) => (
                        <button
                          key={lvl.id}
                          type="button"
                          onClick={() => switchThinkingLevel(lvl.id)}
                          disabled={switchingThinking}
                          className="flex w-full items-start gap-2 rounded-md px-2 py-2 text-left transition-colors hover:bg-[var(--bg-hover)] disabled:cursor-wait disabled:opacity-70"
                        >
                          <span className="mt-0.5 flex h-4 w-4 shrink-0 items-center justify-center text-[var(--accent)]">
                            {lvl.id === thinkingLevel && <Check size={13} />}
                          </span>
                          <span className="min-w-0 flex-1">
                            <span className="block text-xs text-[var(--text-primary)]">
                              {switchingThinking && lvl.id === thinkingLevel
                                ? '切换中...'
                                : lvl.label}
                            </span>
                          </span>
                        </button>
                      ))}
                    </div>
                  </div>
                )}
              </div>
            </div>
            <div className="flex items-center gap-3">
              <span>Enter 发送</span>
              {text.length > 0 && <span>{text.length} 字</span>}
            </div>
          </div>
        </div>
      </div>
    </div>
  );
}
