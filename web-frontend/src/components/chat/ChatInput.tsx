import { useState, useRef, useEffect, useCallback, useMemo, useId } from 'react';
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
  ChevronDown,
  Check,
  FolderOpen,
  FolderPlus,
  Search,
  Server,
} from 'lucide-react';
import { Card } from '../common/Card';
import { workspaceLocationLabel, workspaceIsRemote } from '../../lib/workspacePresentation';
import { useWorkspaceViewStore } from '../../stores/workspaceViewStore';
import { permissionsApi, providerApi } from '../../api/endpoints';
import { useWorkspaceStore } from '../../stores/workspaceStore';
import { useConversationStore } from '../../stores/conversationStore';
import { productDataScope } from '../../lib/productDataScope';
import type { ChatSendSubmission } from '../../hooks/useTauriChat';
import type { ConversationInputReceipt } from '../../generated';
import { workspaceIdForView } from '../../lib/viewAddress';
import { configuredModelPresentation, modelLabel } from '../../lib/modelPresentation';
import { useUiStore } from '../../stores/uiStore';
import { useToastStore } from '../../stores/toastStore';
import { useChatStore } from '../../stores/chatStore';
import { useConversationUsage } from '../../hooks/useConversationUsage';
import { ConversationStatistics } from './ConversationStatistics';
import type { Attachment } from '../../types/api';
import type { ConfiguredModel, ModelProviderView } from '../../generated';
import { CONTEXT_RING_CIRCUMFERENCE, ringDashOffset } from './contextRing';
import { computeContextUsage, estimateDraftTokens, type ContextUsageSource } from './contextUsage';
import { isKnownThinkingLevel, thinkingLevelOptions } from './thinkingLevels';
import { filterCommands, groupByCategory, type SlashCommand } from '../../lib/slashCommands';
import { AgentComposerFrame } from './AgentComposerFrame';
import {
  PERMISSION_MODES,
  PERMISSIONS_CHANGED_EVENT,
  normalizePermissionMode,
  notifyPermissionsChanged,
} from '../../lib/permissionModes';

const THINKING_STORAGE_KEY = 'echo_thinking_level';
function loadThinkingLevel(): string {
  try {
    const v = localStorage.getItem(THINKING_STORAGE_KEY);
    if (v && isKnownThinkingLevel(v)) return v;
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
  source: NonNullable<Attachment['source']>;
}

interface ChatInputProps {
  onSend: (
    text: string,
    attachments?: Attachment[],
    submission?: ChatSendSubmission
  ) => Promise<boolean>;
  isStreaming?: boolean;
  onCancel?: () => void;
  queuedCount?: number;
  home?: boolean;
}

type DraftTarget = Pick<
  ChatSendSubmission,
  'workspaceId' | 'workspaceGeneration' | 'conversationId' | 'newConversationEpoch'
>;

type DraftSubmission = {
  readonly externalId: string;
  readonly target: DraftTarget;
  readonly revision: number;
  readonly draftText: string;
  readonly files: readonly PendingFile[];
  payload: ChatSendSubmission['payload'] | null;
  inFlight: boolean;
  ackSeen: boolean;
  directSeen: boolean;
};

function readDraftTarget(): DraftTarget {
  const conversation = useConversationStore.getState();
  return {
    ...productDataScope(useWorkspaceStore.getState().current),
    conversationId: conversation.activeId,
    newConversationEpoch: conversation.newConversationEpoch,
  };
}

function sameDraftTarget(left: DraftTarget, right: DraftTarget, allowAdmission = false): boolean {
  return (
    left.workspaceId === right.workspaceId &&
    left.workspaceGeneration === right.workspaceGeneration &&
    left.newConversationEpoch === right.newConversationEpoch &&
    (allowAdmission || left.conversationId === right.conversationId)
  );
}

function draftTargetKey(target: DraftTarget): string {
  return JSON.stringify([
    target.workspaceId,
    target.workspaceGeneration,
    target.conversationId,
    target.newConversationEpoch,
  ]);
}

const MAX_FILE_SIZE = 10 * 1024 * 1024; // 10MB
const PASTE_ATTACHMENT_CHAR_THRESHOLD = 1_000;

function fileToBase64(file: File): Promise<string> {
  return new Promise((resolve, reject) => {
    const reader = new FileReader();
    reader.onload = () => {
      const result = reader.result;
      const separator = typeof result === 'string' ? result.indexOf(',') : -1;
      if (typeof result !== 'string' || separator < 0) {
        reject(new Error('附件读取未返回有效内容'));
        return;
      }
      resolve(result.slice(separator + 1));
    };
    reader.onerror = reject;
    reader.onabort = () => reject(new Error('附件读取已取消'));
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
        </div>

        {/* Command list */}
        <div ref={listRef} className="max-h-[280px] overflow-y-auto px-2 py-2">
          {Array.from(grouped.entries()).map(([category, cmds]) => {
            return (
              <div key={category} className="mb-1">
                <div className="flex items-center gap-1.5 px-2 py-1 text-[10px] font-semibold uppercase text-[var(--text-tertiary)]">
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
                        className={`shrink-0 rounded-md px-1.5 py-0.5 text-xs font-semibold ${
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

const MODELS_CHANGED_EVENT = 'eko:models-changed';

function notifyModelsChanged() {
  window.dispatchEvent(new Event(MODELS_CHANGED_EVENT));
}

/** token 数格式化：≥1000 用 k 单位（128000 → 128k，1500 → 1.5k，1999 → 2k）。
 *  与 Rust 侧 format_token_count 保持一致：先四舍五入到 1 位小数，整 k 时省略小数。 */
function formatTokens(n: number): string {
  if (n < 1000) return String(n);
  // 先固定 1 位小数，再去掉末尾 ".0"（2.0k → 2k，但保留 1.5k）。镜像 Rust 实现。
  const formatted = (n / 1000).toFixed(1);
  return formatted.endsWith('.0') ? `${Math.round(n / 1000)}k` : `${formatted}k`;
}

function formatPercent(pct: number): string {
  if (pct > 0 && pct < 1) return '<1%';
  if (pct < 10) return `${Math.round(pct * 10) / 10}%`;
  return `${Math.round(pct)}%`;
}

function ContextRingIndicator({
  pct,
  used,
  win,
  tier,
  cacheRate,
  source,
}: {
  pct: number | null;
  used: number | null;
  win: number | null;
  tier: 'normal' | 'high' | 'critical' | 'unknown';
  cacheRate: number | null;
  source: ContextUsageSource;
}) {
  const [hover, setHover] = useState(false);
  const [pinned, setPinned] = useState(false);
  const tooltipId = useId();
  const showDetails = hover || pinned;
  const color =
    tier === 'critical'
      ? 'var(--color-error)'
      : tier === 'high'
        ? 'var(--color-warning)'
        : tier === 'normal'
          ? 'var(--accent)'
          : 'var(--text-tertiary)';
  const ratio = pct != null ? pct / 100 : null;
  const visualRatio = ratio != null && ratio > 0 ? Math.max(ratio, 0.045) : ratio;
  const offset = ringDashOffset(visualRatio);
  const centerLabel = pct != null ? formatPercent(pct) : '--';
  const cacheLabel = cacheRate != null ? `${Math.round(cacheRate * 1000) / 10}%` : '--';
  const sourceLabel =
    source === 'reported'
      ? '模型回填 + 草稿'
      : source === 'draft-only'
        ? '仅未发送内容'
        : '等待首轮';
  const capacityLine =
    used != null && win != null && pct != null
      ? `上下文容量：${formatTokens(used)} / ${formatTokens(win)} (${formatPercent(pct)})`
      : used != null
        ? `上下文容量：${formatTokens(used)} tokens`
        : '上下文容量：--';

  return (
    <span
      className="relative inline-flex h-7 w-7 shrink-0 items-center justify-center"
      onMouseEnter={() => setHover(true)}
      onMouseLeave={() => setHover(false)}
    >
      <button
        type="button"
        aria-label={`${capacityLine}；来源：${sourceLabel}`}
        aria-expanded={showDetails}
        aria-describedby={showDetails ? tooltipId : undefined}
        className="inline-flex h-7 w-7 items-center justify-center rounded-full focus-visible:outline-2 focus-visible:outline-[var(--accent)]"
        onClick={() => {
          setHover(false);
          setPinned((value) => !value);
        }}
        onBlur={() => {
          setHover(false);
          setPinned(false);
        }}
        onKeyDown={(event) => {
          if (event.key === 'Escape') {
            setHover(false);
            setPinned(false);
          }
        }}
      >
        <svg width="28" height="28" viewBox="0 0 28 28" aria-hidden="true">
          <circle
            cx="14"
            cy="14"
            r="10"
            fill="none"
            stroke={
              source === 'unknown'
                ? 'var(--border-primary)'
                : `color-mix(in srgb, ${color} 22%, var(--border-primary))`
            }
            strokeWidth="3"
          />
          <circle
            cx="14"
            cy="14"
            r="10"
            fill="none"
            stroke={color}
            strokeWidth="3.4"
            strokeLinecap="round"
            strokeDasharray={CONTEXT_RING_CIRCUMFERENCE}
            strokeDashoffset={offset}
            transform="rotate(-90 14 14)"
            style={{
              filter:
                source === 'unknown'
                  ? undefined
                  : `drop-shadow(0 0 2px color-mix(in srgb, ${color} 70%, transparent))`,
            }}
          />
          <text
            x="14"
            y="14"
            textAnchor="middle"
            dominantBaseline="central"
            fill={color}
            fontSize="7.2"
            fontWeight="700"
            fontFamily="ui-monospace, SFMono-Regular, Menlo, monospace"
          >
            {centerLabel}
          </text>
        </svg>
      </button>
      {showDetails && (
        <span
          id={tooltipId}
          className="absolute bottom-full right-0 z-50 mb-2 w-max max-w-[calc(100vw-2rem)] whitespace-normal rounded-md border border-[var(--border-primary)] bg-[var(--bg-primary)] px-2.5 py-1.5 text-left text-[11px] leading-relaxed text-[var(--text-secondary)] shadow-[var(--shadow-md)]"
          role="tooltip"
        >
          <span className="block">{capacityLine}</span>
          <span className="block">来源：{sourceLabel}</span>
          <span className="block">平均缓存命中率：{cacheLabel}</span>
        </span>
      )}
    </span>
  );
}

export function ChatInput({
  onSend,
  isStreaming,
  onCancel,
  queuedCount = 0,
  home = false,
}: ChatInputProps) {
  const [text, setText] = useState('');
  const [pendingFiles, setPendingFiles] = useState<PendingFile[]>([]);
  const textRef = useRef('');
  const filesRef = useRef<PendingFile[]>([]);
  const draftRevisionRef = useRef(0);
  const submissionRef = useRef<DraftSubmission | null>(null);
  const mountedRef = useRef(true);
  const [pendingSubmissionId, setPendingSubmissionId] = useState<string | null>(null);
  const [receiptNotice, setReceiptNotice] = useState<{
    target: string;
    receipt: ConversationInputReceipt;
  } | null>(null);
  const selectedConversationId = useConversationStore((state) => state.activeId);
  const selectedConversationEpoch = useConversationStore((state) => state.newConversationEpoch);
  const selectedWorkspaceGeneration = useWorkspaceStore(
    (state) => state.current?.product_data_generation ?? 'global'
  );
  const selectedWorkspaceId = useWorkspaceStore((state) => workspaceIdForView(state.current?.id));
  const currentDraftTargetKey = JSON.stringify([
    selectedWorkspaceId,
    selectedWorkspaceGeneration,
    selectedConversationId,
    selectedConversationEpoch,
  ]);
  useEffect(() => {
    mountedRef.current = true;
    return () => {
      mountedRef.current = false;
      for (const file of filesRef.current)
        if (file.previewUrl) URL.revokeObjectURL(file.previewUrl);
    };
  }, []);

  const updateDraftText = useCallback((value: string) => {
    draftRevisionRef.current += 1;
    textRef.current = value;
    setText(value);
    setReceiptNotice(null);
  }, []);
  const [paletteSelectedIndex, setPaletteSelectedIndex] = useState(0);
  const [configuredModels, setConfiguredModels] = useState<ConfiguredModel[]>([]);
  const [modelProviders, setModelProviders] = useState<ModelProviderView[]>([]);
  const [modelsLoaded, setModelsLoaded] = useState(false);
  const [modelMenuOpen, setModelMenuOpen] = useState(false);
  const [switchingModelId, setSwitchingModelId] = useState<string | null>(null);
  const [permissionMode, setPermissionMode] = useState('default');
  const [permissionMenuOpen, setPermissionMenuOpen] = useState(false);
  const [switchingPermissionMode, setSwitchingPermissionMode] = useState<string | null>(null);
  const [thinkingLevel, setThinkingLevel] = useState<string>(loadThinkingLevel);
  const [thinkingMenuOpen, setThinkingMenuOpen] = useState(false);
  const [switchingThinking, setSwitchingThinking] = useState(false);
  const textareaRef = useRef<HTMLTextAreaElement>(null);
  const fileInputRef = useRef<HTMLInputElement>(null);
  const thinkingPublicationRef = useRef<string | null>(null);
  const controlsRef = useRef<HTMLDivElement>(null);
  const controlLabelId = useId();
  useEffect(() => {
    if (!modelMenuOpen && !permissionMenuOpen && !thinkingMenuOpen) return;
    const close = () => {
      setModelMenuOpen(false);
      setPermissionMenuOpen(false);
      setThinkingMenuOpen(false);
    };
    const pointer = (event: PointerEvent) => {
      if (event.target instanceof Node && !controlsRef.current?.contains(event.target)) close();
    };
    const keyboard = (event: KeyboardEvent) => {
      if (event.key === 'Escape') {
        close();
        controlsRef.current?.querySelector<HTMLButtonElement>('[aria-expanded="true"]')?.focus();
      }
    };
    document.addEventListener('pointerdown', pointer);
    document.addEventListener('keydown', keyboard);
    return () => {
      document.removeEventListener('pointerdown', pointer);
      document.removeEventListener('keydown', keyboard);
    };
  }, [modelMenuOpen, permissionMenuOpen, thinkingMenuOpen]);
  const setActiveSettingsTab = useUiStore((s) => s.setActiveSettingsTab);
  // Reasoning-depth publication is workspace-scoped: it must reach the pooled
  // conversation Agents of the active workspace, not just the process primary.
  const currentWorkspaceId = useWorkspaceStore((state) => workspaceIdForView(state.current?.id));
  const activeModel = configuredModels.find((model) => model.is_default);
  const visibleModels = configuredModels.filter((model) => model.enabled);
  const displayModel = activeModel ?? visibleModels[0] ?? null;
  const thinkingLevels = thinkingLevelOptions(displayModel?.thinking_levels ?? []);
  // 上下文窗口占用（来自 llm_usage 事件 → chatStore）。
  const contextWindow = useChatStore((s) => s.contextWindow);
  const statistics = useConversationUsage();
  const contextInvalidated = useChatStore((s) => s.contextInvalidated);
  // 后端返回显式配置或模型推断后的有效窗口；无默认标记时与展示模型保持一致。
  const contextWindowSize = displayModel?.context_window ?? null;
  const activePermissionMode =
    PERMISSION_MODES.find((mode) => mode.id === permissionMode) ?? PERMISSION_MODES[0];

  const loadConfiguredModels = useCallback(async () => {
    try {
      const res = await configuredModelPresentation();
      setConfiguredModels(res.models);
      setModelProviders(res.providers);
    } catch (e) {
      console.error('[ChatInput] Failed to load configured models:', e);
    } finally {
      setModelsLoaded(true);
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
      const previousMode = permissionMode;
      setPermissionMode(mode);
      setPermissionMenuOpen(false);
      setSwitchingPermissionMode(mode);
      try {
        await permissionsApi.setMode(mode);
        notifyPermissionsChanged();
      } catch (e) {
        setPermissionMode(previousMode);
        console.error('[ChatInput] Failed to switch permission mode:', e);
      } finally {
        setSwitchingPermissionMode(null);
      }
    },
    [permissionMode, switchingPermissionMode]
  );

  useEffect(() => {
    if (!modelsLoaded) return;

    let effectiveLevel = thinkingLevel;
    if (!thinkingLevels.some((level) => level.id === thinkingLevel)) {
      effectiveLevel = 'auto';
      setThinkingLevel(effectiveLevel);
      setThinkingMenuOpen(false);
      try {
        localStorage.setItem(THINKING_STORAGE_KEY, effectiveLevel);
      } catch {
        /* ignore persistence failure */
      }
    }

    const publicationKey = JSON.stringify([currentWorkspaceId, effectiveLevel]);
    if (thinkingPublicationRef.current === publicationKey) return;
    thinkingPublicationRef.current = publicationKey;
    void providerApi.setThinking(effectiveLevel, currentWorkspaceId).catch((error) => {
      if (thinkingPublicationRef.current === publicationKey) {
        thinkingPublicationRef.current = null;
      }
      console.error('[ChatInput] Failed to apply thinking level:', error);
    });
  }, [thinkingLevel, thinkingLevels, currentWorkspaceId, modelsLoaded]);

  // The backend returns only the levels verified for the active model.
  const switchThinkingLevel = useCallback(
    async (level: string) => {
      if (level === thinkingLevel || switchingThinking) return;
      setSwitchingThinking(true);
      try {
        await providerApi.setThinking(level, currentWorkspaceId);
        thinkingPublicationRef.current = JSON.stringify([currentWorkspaceId, level]);
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
    [thinkingLevel, switchingThinking, currentWorkspaceId]
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

  const addFiles = useCallback(
    (files: FileList | File[], source: NonNullable<Attachment['source']> = 'upload') => {
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
          source,
        });
      }
      if (newFiles.length === 0) return;
      draftRevisionRef.current += 1;
      filesRef.current = [...filesRef.current, ...newFiles];
      setPendingFiles(filesRef.current);
      setReceiptNotice(null);
    },
    []
  );

  const removeFile = useCallback((id: string) => {
    const file = filesRef.current.find((entry) => entry.id === id);
    if (file?.previewUrl) URL.revokeObjectURL(file.previewUrl);
    draftRevisionRef.current += 1;
    filesRef.current = filesRef.current.filter((entry) => entry.id !== id);
    setPendingFiles(filesRef.current);
    setReceiptNotice(null);
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
      for (const item of Array.from(items)) {
        if (item.type.startsWith('image/')) {
          const file = item.getAsFile();
          if (file) imageFiles.push(file);
        }
      }
      if (imageFiles.length > 0) {
        e.preventDefault();
        addFiles(imageFiles, 'paste');
        return;
      }

      const pastedText = e.clipboardData.getData('text/plain');
      if (Array.from(pastedText).length >= PASTE_ATTACHMENT_CHAR_THRESHOLD) {
        e.preventDefault();
        const file = new globalThis.File(
          [pastedText],
          `pasted-text-${pendingFiles.length + 1}.txt`,
          { type: 'text/plain' }
        );
        addFiles([file], 'paste');
      }
    },
    [addFiles, pendingFiles.length]
  );

  // ── Send ──

  const handleSend = async () => {
    const draftText = textRef.current;
    const trimmed = draftText.trim();
    const files = filesRef.current;
    if (!trimmed && files.length === 0) return;
    const target = readDraftTarget();
    const previous = submissionRef.current;
    // A first send can acquire its canonical conversation before Started comes
    // back. That assignment must not turn repeated Enter on the same draft into
    // a second input. A changed draft in a different target is a new intent.
    if (
      previous?.inFlight &&
      (previous.revision === draftRevisionRef.current || sameDraftTarget(previous.target, target))
    )
      return;
    if (!displayModel) {
      openModelSettings();
      useToastStore.getState().addToast('info', '请先添加 Provider 和模型');
      return;
    }
    if (showPalette && filteredCommands.length > 0) {
      const command = filteredCommands[paletteSelectedIndex];
      if (command) {
        selectCommand(command);
        return;
      }
    }
    const revision = draftRevisionRef.current;
    const submission: DraftSubmission =
      previous && previous.revision === revision && sameDraftTarget(previous.target, target)
        ? previous
        : {
            externalId: crypto.randomUUID(),
            target,
            revision,
            draftText,
            files: [...files],
            payload: null,
            inFlight: false,
            ackSeen: false,
            directSeen: false,
          };
    submissionRef.current = submission;
    submission.inFlight = true;
    setPendingSubmissionId(submission.externalId);
    setReceiptNotice(null);

    const isOwnDraft = (allowAdmission = false) =>
      mountedRef.current &&
      submissionRef.current === submission &&
      draftRevisionRef.current === submission.revision &&
      textRef.current === submission.draftText &&
      filesRef.current.length === submission.files.length &&
      filesRef.current.every((file, index) => file.id === submission.files[index]?.id) &&
      sameDraftTarget(submission.target, readDraftTarget(), allowAdmission);
    const releaseAdmission = () => {
      submission.inFlight = false;
      if (mountedRef.current && submissionRef.current === submission) setPendingSubmissionId(null);
    };
    const clearOwnDraft = (allowAdmission = false) => {
      if (!isOwnDraft(allowAdmission)) return;
      for (const file of submission.files)
        if (file.previewUrl) URL.revokeObjectURL(file.previewUrl);
      draftRevisionRef.current += 1;
      textRef.current = '';
      filesRef.current = [];
      submissionRef.current = null;
      setText('');
      setPendingFiles([]);
      setPendingSubmissionId(null);
      requestAnimationFrame(() => {
        if (mountedRef.current && textareaRef.current) textareaRef.current.style.height = 'auto';
      });
    };
    try {
      if (!submission.payload) {
        const attachments = await Promise.all(
          submission.files.map(async (file) => ({
            name: file.name,
            mime_type: file.mime_type,
            data: await fileToBase64(file.file),
            size: file.size,
            source: file.source,
          }))
        );
        submission.payload = {
          text: trimmed,
          attachments:
            attachments.length > 0
              ? attachments.map((attachment) => Object.freeze(attachment))
              : undefined,
        };
      }
      // Preparing an attachment must never choose a newer sidebar target.
      if (!mountedRef.current || !sameDraftTarget(submission.target, readDraftTarget())) return;
      const options: ChatSendSubmission = {
        externalId: submission.externalId,
        ...submission.target,
        payload: submission.payload,
        onPersisted: (receipt, canProject) => {
          // Observe the original fact before checking current presentation. A
          // late boolean must not clear another draft after this callback.
          submission.ackSeen = true;
          releaseAdmission();
          if (
            !canProject() ||
            !isOwnDraft() ||
            receipt.identity.address.workspace_id !== submission.target.workspaceId ||
            receipt.identity.address.conversation_id !== submission.target.conversationId
          )
            return;
          setReceiptNotice({ target: draftTargetKey(submission.target), receipt });
          clearOwnDraft();
        },
        onDirectAccepted: (canProject) => {
          submission.directSeen = true;
          releaseAdmission();
          // Only the original sender can prove a normal first-message bootstrap
          // or a successful local command changed the conversation legitimately.
          if (canProject()) clearOwnDraft(true);
        },
      };
      const payload = submission.payload;
      const accepted = await onSend(
        payload.text,
        payload.attachments
          ? payload.attachments.map((attachment) => ({ ...attachment }))
          : undefined,
        options
      );
      // Local/legacy boolean success is not a persisted acknowledgement. The
      // strict same-target fallback cannot adopt an unrelated new conversation.
      if (accepted && !submission.ackSeen && !submission.directSeen) clearOwnDraft();
    } catch (error) {
      if (isOwnDraft())
        useToastStore
          .getState()
          .addToast(
            'error',
            `${submission.payload ? '发送失败' : '附件处理失败'}：${error instanceof Error ? error.message : String(error)}`
          );
    } finally {
      releaseAdmission();
    }
  };

  // ── Command selection ──

  const selectCommand = useCallback(
    (cmd: SlashCommand) => {
      // Place the command name into the text input with a trailing space
      // so the user can append arguments, or just press Enter to send.
      updateDraftText(cmd.name + ' ');
      setPaletteSelectedIndex(0);
      // Refocus the textarea
      requestAnimationFrame(() => {
        textareaRef.current?.focus();
      });
    },
    [updateDraftText]
  );

  // ── Keyboard handling ──

  const handleKeyDown = (e: React.KeyboardEvent<HTMLTextAreaElement>) => {
    // Palette navigation takes priority when palette is visible
    if (showPalette) {
      if (e.key === 'Escape') {
        e.preventDefault();
        // Clear the slash query by removing the text
        updateDraftText('');
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
  const currentSubmissionPending =
    pendingSubmissionId !== null &&
    submissionRef.current?.externalId === pendingSubmissionId &&
    (submissionRef.current.revision === draftRevisionRef.current ||
      sameDraftTarget(submissionRef.current.target, readDraftTarget()));
  const draftTokens = useMemo(() => estimateDraftTokens(text, pendingFiles), [text, pendingFiles]);
  const contextUsage = computeContextUsage(
    contextWindow?.inputTokens ??
      (!contextInvalidated ? (statistics.value?.main.latest_input_tokens ?? null) : null),
    draftTokens,
    contextWindowSize
  );
  const ctxUsage = {
    ...contextUsage,
    win: contextWindowSize,
    cacheRate:
      statistics.value && statistics.value.main.input_tokens > 0 && !statistics.value.main.conflicts
        ? statistics.value.main.cached_tokens / statistics.value.main.input_tokens
        : null,
  };

  return (
    <div
      className="w-full px-3 pb-4 sm:px-6"
      onDragOver={handleDragOver}
      onDragLeave={handleDragLeave}
      onDrop={handleDrop}
    >
      {/* Relative container so the absolute-positioned palette anchors here */}
      <div className="relative mx-auto max-w-[980px]">
        {/* ── Slash Command Palette ── */}
        {showPalette && (
          <CommandPalette
            commands={filteredCommands}
            selectedIndex={paletteSelectedIndex}
            onSelect={selectCommand}
          />
        )}

        <div className="relative overflow-visible rounded-[24px] bg-[var(--bg-secondary)] p-1 shadow-[var(--shadow-sm)]">
          {/* Drag overlay */}
          {isDragging && (
            <div className="absolute inset-0 z-10 flex items-center justify-center rounded-lg border-2 border-dashed border-[var(--accent)] bg-[var(--accent)]/5">
              <span className="text-sm font-medium text-[var(--accent)]">
                Drop files here to upload
              </span>
            </div>
          )}
          {/* Attachment previews */}
          {pendingFiles.length > 0 && (
            <div className="flex flex-wrap gap-2 px-1">
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
                    aria-label={`移除附件 ${pf.name}`}
                    onClick={() => removeFile(pf.id)}
                    className="absolute right-1 top-1 flex h-5 w-5 items-center justify-center rounded-full bg-[var(--bg-primary)] text-[var(--text-tertiary)] opacity-0 shadow-[var(--shadow-sm)] transition-opacity group-hover:opacity-100 focus-visible:opacity-100 hover:text-[var(--text-primary)]"
                  >
                    <X size={10} />
                  </button>
                </div>
              ))}
            </div>
          )}

          {/* Input row */}
          <AgentComposerFrame variant="primary">
            {/* Attachment button */}
            <button
              onClick={() => fileInputRef.current?.click()}
              className="absolute bottom-3 left-4 flex h-8 w-8 items-center justify-center rounded-md text-[var(--text-tertiary)] transition-colors hover:bg-[var(--bg-hover)] hover:text-[var(--text-primary)]"
              title="添加附件"
              aria-label="添加附件"
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
              aria-label="消息"
              aria-busy={currentSubmissionPending}
              value={text}
              onChange={(e) => updateDraftText(e.target.value)}
              onKeyDown={handleKeyDown}
              onPaste={handlePaste}
              rows={1}
              placeholder={
                displayModel
                  ? home
                    ? '今天想做什么？描述任务，或 / 调用技能与指令'
                    : '继续对话，或补充当前任务…'
                  : '请先配置模型'
              }
              className="max-h-[240px] min-h-[64px] w-full min-w-0 flex-1 resize-none bg-transparent text-sm leading-relaxed text-[var(--text-primary)] outline-none placeholder:text-[var(--text-tertiary)]"
            />
            {isStreaming ? (
              <button
                onClick={onCancel}
                className="absolute bottom-3 right-4 flex h-8 w-8 items-center justify-center rounded-full bg-[var(--text-secondary)] text-white transition-all hover:bg-[var(--text-primary)]"
                title="停止生成"
                aria-label="停止生成"
              >
                <Square size={14} fill="white" color="white" />
              </button>
            ) : (
              <button
                onClick={handleSend}
                disabled={!hasContent || !displayModel || currentSubmissionPending}
                aria-label={displayModel ? '发送' : '请先配置模型'}
                title={displayModel ? '发送' : '请先配置模型'}
                className="absolute bottom-3 right-4 flex h-8 w-8 items-center justify-center rounded-full bg-[var(--accent)] text-[var(--text-on-accent)] transition-all hover:shadow-[var(--shadow-glow)] disabled:opacity-20 disabled:hover:shadow-none"
              >
                <ArrowUp size={16} strokeWidth={2.5} />
              </button>
            )}
            <div
              ref={controlsRef}
              className="relative flex min-w-0 items-center justify-between gap-x-1 pl-8 pr-9 text-xs text-[var(--text-tertiary)]"
            >
              <div className="flex min-w-0 shrink-0 items-center gap-0.5 sm:gap-1">
                <WorkspaceMenuControl
                  onOpen={() => {
                    setPermissionMenuOpen(false);
                    setModelMenuOpen(false);
                    setThinkingMenuOpen(false);
                  }}
                />
                <div>
                  <button
                    type="button"
                    onClick={() => {
                      setPermissionMenuOpen((open) => !open);
                      setModelMenuOpen(false);
                      setThinkingMenuOpen(false);
                    }}
                    className="flex max-w-[180px] items-center gap-1 rounded-full px-1 py-1 sm:gap-1.5 sm:px-2 text-[var(--text-tertiary)] transition-colors hover:bg-[var(--bg-hover)] hover:text-[var(--text-primary)]"
                    title="权限配置"
                    aria-label="权限配置"
                    aria-describedby={`${controlLabelId}-permission`}
                    aria-expanded={permissionMenuOpen}
                  >
                    <span id={`${controlLabelId}-permission`} className="sr-only">
                      当前权限：
                      {permissionMode === 'default' ? '默认权限' : activePermissionMode.label}
                    </span>
                    <ShieldCheck size={13} />
                    <span className="hidden truncate sm:inline">
                      {permissionMode === 'default' ? '默认权限' : activePermissionMode.label}
                    </span>
                    <ChevronDown size={12} />
                  </button>
                  {permissionMenuOpen && (
                    <MenuOverlay title="审批模式" width="w-64">
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
                    </MenuOverlay>
                  )}
                </div>
                <div>
                  <button
                    type="button"
                    onClick={() => {
                      setPermissionMenuOpen(false);
                      setModelMenuOpen(false);
                      setThinkingMenuOpen((open) => !open);
                    }}
                    disabled={!displayModel || switchingThinking}
                    className="flex max-w-[140px] items-center gap-1 rounded-full px-1 py-1 sm:gap-1.5 sm:px-2 text-[var(--text-tertiary)] transition-colors hover:bg-[var(--bg-hover)] hover:text-[var(--text-primary)] disabled:cursor-not-allowed disabled:opacity-50 disabled:hover:bg-transparent disabled:hover:text-[var(--text-tertiary)]"
                    title={displayModel ? '切换思考深度' : '配置模型后可用'}
                    aria-label="思考深度"
                    aria-describedby={`${controlLabelId}-thinking`}
                    aria-expanded={thinkingMenuOpen}
                  >
                    <span id={`${controlLabelId}-thinking`} className="sr-only">
                      当前思考深度：
                      {thinkingLevels.find((level) => level.id === thinkingLevel)?.label ?? '自动'}
                    </span>
                    <Brain size={13} />
                    <span className="hidden truncate sm:inline">
                      {thinkingLevels.find((level) => level.id === thinkingLevel)?.label ?? '自动'}
                    </span>
                    {displayModel && <ChevronDown size={12} />}
                  </button>
                  {thinkingMenuOpen && displayModel && (
                    <MenuOverlay title="思考深度" width="w-56">
                      <div className="p-1">
                        {thinkingLevels.map((lvl) => (
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
                    </MenuOverlay>
                  )}
                </div>
              </div>
              <div className="ml-auto flex min-w-0 flex-1 items-center justify-end gap-1">
                {receiptNotice?.target === currentDraftTargetKey && (
                  <span
                    role="status"
                    title="输入已保存，后续处理状态由原任务回执呈现"
                    className="px-1 text-[var(--text-secondary)]"
                    data-input-id={receiptNotice.receipt.identity.input_id}
                  >
                    已收到
                  </span>
                )}
                <div className="min-w-0 max-w-full">
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
                    className="flex max-w-full min-w-0 sm:max-w-[min(32vw,300px)] items-center gap-1 rounded-full px-1 py-1 text-[var(--text-tertiary)] transition-colors hover:bg-[var(--bg-hover)] hover:text-[var(--text-primary)] sm:px-2"
                    title={
                      displayModel ? `${displayModel.provider} / ${displayModel.model}` : '选择模型'
                    }
                    aria-label="选择模型"
                    aria-expanded={modelMenuOpen}
                  >
                    <Cpu size={13} />
                    <span className="truncate">
                      {displayModel ? modelLabel(displayModel, modelProviders) : '选择模型'}
                    </span>
                    {visibleModels.length > 0 && <ChevronDown size={12} />}
                  </button>
                  {modelMenuOpen && visibleModels.length > 0 && (
                    <MenuOverlay title="默认模型" width="w-64" align="right">
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
                            <span className="min-w-0 flex-1 break-words">
                              {modelLabel(model, modelProviders)}
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
                    </MenuOverlay>
                  )}
                </div>
                {queuedCount > 0 && (
                  <span className="hidden lg:inline">已排队 {queuedCount} 条</span>
                )}
              </div>
            </div>
          </AgentComposerFrame>
          <div
            className="flex min-h-7 items-center justify-center gap-x-1 px-2 pt-1 text-[10px] text-[var(--text-tertiary)] sm:gap-x-3 sm:text-xs"
            aria-label="运行统计"
          >
            <ConversationStatistics
              streaming={isStreaming === true}
              value={statistics.value}
              error={statistics.error}
            />
            {ctxUsage.source !== 'unknown' && (
              <ContextRingIndicator
                pct={ctxUsage.pct}
                used={ctxUsage.used}
                win={ctxUsage.win}
                tier={ctxUsage.tier}
                cacheRate={ctxUsage.cacheRate}
                source={ctxUsage.source}
              />
            )}
            {text.length > 0 && <span className="hidden shrink-0 sm:inline">{text.length} 字</span>}
          </div>
        </div>
      </div>
    </div>
  );
}

/**
 * MenuOverlay — local dropdown/popover shell reused by the permission / model /
 * thinking menus in the composer footer. Collapses the three previously
 * hand-rolled `rounded-lg border bg-primary shadow-md` blocks into one source
 * of truth (built on the Card overlay variant). Children are the menu body
 * (callers control their own padding / scrolling / footer).
 */
function MenuOverlay({
  title,
  width = 'w-64',
  children,
  align = 'left',
}: {
  title: string;
  width?: string;
  align?: 'left' | 'right';
  children: React.ReactNode;
}) {
  return (
    <Card
      variant="overlay"
      className={`absolute bottom-full z-50 mb-2 max-w-full overflow-hidden ${align === 'right' ? 'right-0' : 'left-0'} ${width}`}
    >
      <div className="border-b border-[var(--border-secondary)] px-3 py-2 text-[10px] font-medium text-[var(--text-tertiary)]">
        {title}
      </div>
      {children}
    </Card>
  );
}

function WorkspaceMenuControl({ onOpen }: { onOpen: () => void }) {
  const current = useWorkspaceStore((state) => state.current);
  const workspaces = useWorkspaceStore((state) => state.workspaces);
  const isLoading = useWorkspaceStore((state) => state.isLoading);
  const openWorkspaceDialog = useUiStore((state) => state.openWorkspaceDialog);
  const [open, setOpen] = useState(false);
  const [query, setQuery] = useState('');
  const [error, setError] = useState('');
  const container = useRef<HTMLDivElement>(null);
  const trigger = useRef<HTMLButtonElement>(null);
  const search = useRef<HTMLInputElement>(null);
  const close = useCallback(() => {
    setOpen(false);
    trigger.current?.focus();
  }, []);
  useEffect(() => {
    if (!open) return;
    search.current?.focus();
    const pointer = (event: PointerEvent) => {
      if (event.target instanceof Node && !container.current?.contains(event.target))
        setOpen(false);
    };
    const keyboard = (event: KeyboardEvent) => {
      if (event.key === 'Escape') {
        event.preventDefault();
        close();
      }
    };
    document.addEventListener('pointerdown', pointer);
    document.addEventListener('keydown', keyboard);
    return () => {
      document.removeEventListener('pointerdown', pointer);
      document.removeEventListener('keydown', keyboard);
    };
  }, [open, close]);
  const choose = async (id: string, generation: string) => {
    const epoch = useUiStore.getState().beginNavigation();
    setError('');
    try {
      if (useWorkspaceStore.getState().current?.id !== id)
        await useWorkspaceStore.getState().switchTo(id);
      const actual = useWorkspaceStore.getState().current;
      if (useUiStore.getState().navigationEpoch !== epoch) return;
      if (actual?.id !== id || actual.product_data_generation !== generation) {
        throw new Error('工作空间已变化，请重新选择');
      }
      useWorkspaceViewStore.getState().openChat();
      close();
    } catch (cause) {
      if (useUiStore.getState().navigationEpoch === epoch)
        setError(cause instanceof Error ? cause.message : '切换工作空间失败');
    }
  };
  const filtered = (workspaces ?? []).filter((workspace) =>
    `${workspace.name} ${workspaceLocationLabel(workspace)}`
      .toLowerCase()
      .includes(query.trim().toLowerCase())
  );
  return (
    <div ref={container} className="min-w-0 shrink-0">
      <button
        ref={trigger}
        type="button"
        aria-label="选择工作空间"
        aria-expanded={open}
        onClick={() => {
          if (!open) onOpen();
          setOpen((value) => !value);
          setError('');
        }}
        title={current ? workspaceLocationLabel(current) : '选择工作空间'}
        className="flex max-w-[min(18vw,160px)] min-w-0 items-center gap-1 rounded-lg px-1 py-1.5 text-[var(--text-secondary)] hover:bg-[var(--bg-hover)] sm:px-2"
      >
        {current && workspaceIsRemote(current) ? <Server size={14} /> : <FolderOpen size={14} />}
        <span className="truncate">{current?.name || '选择工作空间'}</span>
        <ChevronDown size={12} className="shrink-0" />
      </button>
      {open && (
        <Card
          variant="overlay"
          role="dialog"
          aria-label="工作空间选择"
          className="absolute bottom-full left-0 z-50 mb-3 w-[330px] max-w-full overflow-hidden rounded-2xl"
        >
          <label className="eko-workspace-search m-2 flex items-center gap-2 rounded-xl bg-[var(--bg-secondary)] px-3 py-2.5">
            <Search size={15} className="shrink-0" />
            <input
              ref={search}
              aria-label="搜索工作空间"
              value={query}
              onChange={(event) => setQuery(event.target.value)}
              placeholder="搜索工作空间"
              className="min-w-0 flex-1 bg-transparent text-sm text-[var(--text-primary)] outline-none"
            />
          </label>
          <div className="max-h-[min(240px,40vh)] overflow-y-auto px-2 pb-2">
            {filtered.map((workspace) => (
              <button
                key={workspace.id}
                type="button"
                disabled={isLoading}
                onClick={() => void choose(workspace.id, workspace.product_data_generation)}
                className="flex w-full items-center gap-2.5 rounded-xl px-3 py-2.5 text-left hover:bg-[var(--bg-hover)] disabled:opacity-50"
              >
                {workspaceIsRemote(workspace) ? (
                  <Server size={16} className="shrink-0" />
                ) : (
                  <FolderOpen size={16} className="shrink-0" />
                )}
                <span className="min-w-0 flex-1">
                  <span className="block truncate text-sm text-[var(--text-primary)]">
                    {workspace.name}
                  </span>
                  <span
                    className="block truncate text-[11px]"
                    title={workspaceLocationLabel(workspace)}
                  >
                    {workspaceLocationLabel(workspace)}
                  </span>
                </span>
                {current?.id === workspace.id && <Check size={14} className="shrink-0" />}
              </button>
            ))}
            {filtered.length === 0 && (
              <p className="px-3 py-3 text-xs">{query ? '没有匹配的工作空间' : '还没有工作空间'}</p>
            )}
          </div>
          {error && (
            <p role="alert" className="px-4 pb-2 text-xs text-[var(--color-error)]">
              {error}
            </p>
          )}
          <div className="border-t border-[var(--border-secondary)] p-2">
            {(
              [
                ['new_local', '新建工作空间', FolderPlus],
                ['existing_local', '打开本地文件夹', FolderOpen],
                ['ssh', '连接远程 SSH 设备', Server],
              ] as const
            ).map(([source, label, Icon]) => (
              <button
                key={source}
                type="button"
                onClick={() => {
                  close();
                  openWorkspaceDialog(source);
                }}
                className="flex w-full items-center gap-2.5 rounded-xl px-3 py-2.5 text-left text-sm text-[var(--text-primary)] hover:bg-[var(--bg-hover)]"
              >
                <Icon size={16} />
                {label}
              </button>
            ))}
          </div>
        </Card>
      )}
    </div>
  );
}
