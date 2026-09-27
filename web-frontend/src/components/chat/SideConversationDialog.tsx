import { useEffect, useRef, useState } from 'react';
import { GitFork, Loader2, X } from 'lucide-react';
import { providerApi } from '../../api/endpoints';
import { useConversationStore } from '../../stores/conversationStore';
import type { ConfiguredModel } from '../../generated';
import { Modal } from '../common/Modal';

interface SideConversationDialogProps {
  isOpen: boolean;
  onClose: () => void;
}

function newSideConversationRequestId(): string {
  return typeof globalThis.crypto?.randomUUID === 'function'
    ? globalThis.crypto.randomUUID()
    : `side-${Date.now()}-${Math.random().toString(36).slice(2, 10)}`;
}

export function SideConversationDialog({ isOpen, onClose }: SideConversationDialogProps) {
  const createSideConversation = useConversationStore((state) => state.createSideConversation);
  const promptRef = useRef<HTMLTextAreaElement>(null);
  const requestIdRef = useRef(newSideConversationRequestId());
  const [prompt, setPrompt] = useState('');
  const [title, setTitle] = useState('');
  const [modelId, setModelId] = useState('');
  const [models, setModels] = useState<ConfiguredModel[]>([]);
  const [submitting, setSubmitting] = useState(false);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    if (!isOpen) return;
    let active = true;
    setPrompt('');
    setTitle('');
    setModelId('');
    setError(null);
    requestIdRef.current = newSideConversationRequestId();
    providerApi
      .listConfigured()
      .then((response) => {
        if (active) setModels(response.models.filter((model) => model.enabled));
      })
      .catch((cause) => {
        if (active) setError(cause instanceof Error ? cause.message : String(cause));
      });
    return () => {
      active = false;
    };
  }, [isOpen]);

  if (!isOpen) return null;

  const handleSubmit = async () => {
    const text = prompt.trim();
    if (!text || submitting) return;
    setSubmitting(true);
    setError(null);
    try {
      await createSideConversation({
        requestId: requestIdRef.current,
        prompt: text,
        title: title.trim() || undefined,
        modelId: modelId || undefined,
      });
      onClose();
    } catch (cause) {
      setError(cause instanceof Error ? cause.message : String(cause));
    } finally {
      setSubmitting(false);
    }
  };

  return (
    <Modal
      onClose={() => {
        if (!submitting) onClose();
      }}
      ariaLabel="新建支线对话"
      initialFocusRef={promptRef}
      className="flex w-[min(560px,calc(100vw-2rem))] flex-col overflow-hidden rounded-lg border border-[var(--border-primary)] bg-[var(--bg-primary)] shadow-[var(--shadow-xl)]"
    >
      <header className="flex h-12 items-center justify-between border-b border-[var(--border-primary)] px-4">
        <div className="flex items-center gap-2">
          <GitFork size={15} className="text-[var(--accent)]" aria-hidden="true" />
          <h2 className="text-sm font-semibold text-[var(--text-primary)]">新建支线对话</h2>
        </div>
        <button
          type="button"
          onClick={onClose}
          disabled={submitting}
          className="flex h-8 w-8 items-center justify-center rounded-md text-[var(--text-tertiary)] hover:bg-[var(--bg-hover)] hover:text-[var(--text-primary)] disabled:cursor-not-allowed disabled:opacity-50"
          title="关闭"
          aria-label="关闭支线对话窗口"
        >
          <X size={16} />
        </button>
      </header>

      <div className="space-y-4 p-4">
        <label className="block space-y-1.5">
          <span className="text-xs font-medium text-[var(--text-secondary)]">要处理的问题</span>
          <textarea
            ref={promptRef}
            value={prompt}
            onChange={(event) => setPrompt(event.target.value)}
            rows={6}
            placeholder="输入支线任务..."
            className="w-full resize-y rounded-md border border-[var(--border-primary)] bg-[var(--bg-secondary)] px-3 py-2 text-sm text-[var(--text-primary)] outline-none focus:border-[var(--accent)]"
          />
        </label>

        <div className="grid gap-3 sm:grid-cols-2">
          <label className="block space-y-1.5">
            <span className="text-xs font-medium text-[var(--text-secondary)]">标题</span>
            <input
              value={title}
              onChange={(event) => setTitle(event.target.value)}
              placeholder="自动生成"
              className="h-9 w-full rounded-md border border-[var(--border-primary)] bg-[var(--bg-secondary)] px-2.5 text-xs text-[var(--text-primary)] outline-none focus:border-[var(--accent)]"
            />
          </label>
          <label className="block space-y-1.5">
            <span className="text-xs font-medium text-[var(--text-secondary)]">模型</span>
            <select
              value={modelId}
              onChange={(event) => setModelId(event.target.value)}
              className="h-9 w-full rounded-md border border-[var(--border-primary)] bg-[var(--bg-secondary)] px-2.5 text-xs text-[var(--text-primary)] outline-none focus:border-[var(--accent)]"
            >
              <option value="">继承主对话</option>
              {models.map((model) => (
                <option key={model.id} value={model.id}>
                  {model.display_name || model.model}
                </option>
              ))}
            </select>
          </label>
        </div>

        {error && (
          <div className="rounded-md bg-[var(--color-error-bg)] px-3 py-2 text-xs text-[var(--color-error-text)]">
            {error}
          </div>
        )}
      </div>

      <footer className="flex items-center justify-end gap-2 border-t border-[var(--border-primary)] px-4 py-3">
        <button
          type="button"
          onClick={onClose}
          disabled={submitting}
          className="h-8 rounded-md px-3 text-xs text-[var(--text-secondary)] hover:bg-[var(--bg-hover)] disabled:cursor-not-allowed disabled:opacity-50"
        >
          取消
        </button>
        <button
          type="button"
          onClick={() => void handleSubmit()}
          disabled={!prompt.trim() || submitting}
          className="flex h-8 items-center gap-1.5 rounded-md bg-[var(--accent)] px-3 text-xs font-medium text-white disabled:cursor-not-allowed disabled:opacity-50"
        >
          {submitting && <Loader2 size={13} className="animate-spin" aria-hidden="true" />}
          创建
        </button>
      </footer>
    </Modal>
  );
}
