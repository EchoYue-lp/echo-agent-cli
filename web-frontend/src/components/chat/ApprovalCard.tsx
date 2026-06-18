import { useState } from 'react';
import type { ApprovalRequest } from '../../types/api';
import { ShieldCheck, Check, X, Pencil, Unlock, ChevronDown } from 'lucide-react';

type InputMode = 'none' | 'reject' | 'modify';
type MaybePromise<T> = T | Promise<T>;

export function ApprovalCard({
  request,
  onApprove,
  onReject,
  onModify,
  onApproveAll,
}: {
  request: ApprovalRequest;
  onApprove: () => MaybePromise<void>;
  onReject: (reason?: string) => MaybePromise<void>;
  onModify: (feedback: string) => MaybePromise<void>;
  onApproveAll: () => MaybePromise<void>;
}) {
  const [feedback, setFeedback] = useState('');
  const [inputMode, setInputMode] = useState<InputMode>('none');
  const [isSubmitting, setIsSubmitting] = useState(false);

  const runAction = async (action: () => MaybePromise<void>) => {
    if (isSubmitting) return;
    setIsSubmitting(true);
    try {
      await action();
    } finally {
      setIsSubmitting(false);
    }
  };

  const handleSubmitFeedback = async () => {
    if (inputMode === 'reject') {
      await runAction(() => onReject(feedback || undefined));
    } else if (inputMode === 'modify') {
      await runAction(() => onModify(feedback));
    }
    setFeedback('');
    setInputMode('none');
  };

  const handleCancel = () => {
    setFeedback('');
    setInputMode('none');
  };

  return (
    <div className="rounded-lg border border-[var(--border-primary)] bg-[var(--bg-primary)] px-3 py-2 shadow-[var(--shadow-sm)]">
      <div className="flex items-start gap-2.5">
        <div className="mt-0.5 flex h-7 w-7 shrink-0 items-center justify-center rounded-md bg-[var(--bg-secondary)] text-[var(--accent)]">
          <ShieldCheck size={15} />
        </div>
        <div className="min-w-0 flex-1">
          <div className="flex flex-wrap items-center gap-2">
            <p className="text-xs font-semibold text-[var(--text-primary)]">需要审批</p>
            <code className="rounded bg-[var(--bg-secondary)] px-1.5 py-0.5 font-mono text-[11px] text-[var(--text-secondary)]">
              {request.toolName}
            </code>
            {request.prompt && (
              <span className="min-w-0 flex-1 truncate text-[11px] text-[var(--text-tertiary)]">
                {request.prompt}
              </span>
            )}
          </div>
          <details className="mt-1 group">
            <summary className="inline-flex cursor-pointer list-none items-center gap-1 text-[11px] text-[var(--text-tertiary)] transition-colors hover:text-[var(--text-secondary)]">
              查看参数
              <ChevronDown size={12} className="transition-transform group-open:rotate-180" />
            </summary>
            <pre className="mt-1 max-h-28 overflow-auto rounded-md bg-[var(--bg-code)] p-2 text-[11px] text-[var(--color-code-text)]">
              {JSON.stringify(request.args, null, 2)}
            </pre>
          </details>

          {inputMode === 'none' ? (
            <div className="mt-2 flex flex-wrap items-center gap-1.5">
              <button
                type="button"
                onClick={() => runAction(onApprove)}
                disabled={isSubmitting}
                className="flex items-center gap-1 rounded-md bg-[var(--accent)] px-2.5 py-1.5 text-xs font-medium text-[var(--text-on-accent)] transition-opacity hover:opacity-90"
              >
                <Check size={13} /> {isSubmitting ? '处理中' : '同意'}
              </button>
              <button
                type="button"
                onClick={() => setInputMode('reject')}
                disabled={isSubmitting}
                className="flex items-center gap-1 rounded-md border border-[var(--border-primary)] px-2.5 py-1.5 text-xs font-medium text-[var(--text-secondary)] transition-colors hover:bg-[var(--bg-hover)] hover:text-[var(--text-primary)]"
              >
                <X size={13} /> 拒绝
              </button>
              <button
                type="button"
                onClick={() => setInputMode('modify')}
                disabled={isSubmitting}
                className="flex items-center gap-1 rounded-md border border-[var(--border-primary)] px-2.5 py-1.5 text-xs font-medium text-[var(--text-secondary)] transition-colors hover:bg-[var(--bg-hover)] hover:text-[var(--text-primary)]"
              >
                <Pencil size={13} /> 修改
              </button>
              <button
                type="button"
                onClick={() => runAction(onApproveAll)}
                disabled={isSubmitting}
                className="flex items-center gap-1 rounded-md border border-[var(--border-primary)] px-2.5 py-1.5 text-xs font-medium text-[var(--text-secondary)] transition-colors hover:bg-[var(--bg-hover)] hover:text-[var(--text-primary)]"
              >
                <Unlock size={13} /> 本会话同意
              </button>
            </div>
          ) : (
            <div className="mt-2 flex flex-1 gap-2">
              <input
                value={feedback}
                onChange={(e) => setFeedback(e.target.value)}
                onKeyDown={(e) => {
                  if (e.key === 'Enter') handleSubmitFeedback();
                  if (e.key === 'Escape') handleCancel();
                }}
                className="flex-1 rounded-md border border-[var(--border-primary)] bg-[var(--bg-input)] px-2.5 py-1.5 text-xs text-[var(--text-primary)] outline-none focus:border-[var(--border-focus)]"
                placeholder={
                  inputMode === 'reject'
                    ? '请输入拒绝原因...'
                    : '请输入修改意见（Agent 将据此调整方案）...'
                }
                autoFocus
              />
              <button
                type="button"
                onClick={handleSubmitFeedback}
                disabled={isSubmitting}
                className="rounded-md bg-[var(--accent)] px-2.5 py-1.5 text-xs font-medium text-[var(--text-on-accent)] transition-opacity hover:opacity-90"
              >
                {isSubmitting ? '提交中' : '提交'}
              </button>
              <button
                type="button"
                onClick={handleCancel}
                disabled={isSubmitting}
                className="rounded-md border border-[var(--border-primary)] px-2.5 py-1.5 text-xs text-[var(--text-secondary)] transition-colors hover:bg-[var(--bg-hover)]"
              >
                取消
              </button>
            </div>
          )}
        </div>
      </div>
    </div>
  );
}
