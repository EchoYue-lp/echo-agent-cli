import { useState } from 'react';
import type { ApprovalRequest } from '../../types/api';
import { ShieldAlert, Check, X } from 'lucide-react';

export function ApprovalCard({
  request,
  onApprove,
  onReject,
}: {
  request: ApprovalRequest;
  onApprove: () => void;
  onReject: (reason?: string) => void;
}) {
  const [reason, setReason] = useState('');
  const [showReject, setShowReject] = useState(false);

  return (
    <div className="animate-pulse-border rounded-xl border-2 border-red-300 bg-red-50 p-4 dark:border-red-800 dark:bg-red-950/30">
      <div className="flex items-start gap-3">
        <div className="flex h-8 w-8 shrink-0 items-center justify-center rounded-lg bg-red-100 dark:bg-red-900/40">
          <ShieldAlert size={16} className="text-red-600 dark:text-red-400" />
        </div>
        <div className="flex-1">
          <p className="text-sm font-semibold text-red-800 dark:text-red-300">需要批准</p>
          <p className="mt-1 text-sm text-red-700 dark:text-red-400">
            工具：<code className="rounded bg-red-100 px-1.5 py-0.5 font-mono text-xs dark:bg-red-900/40">{request.toolName}</code>
          </p>
          <pre className="mt-2 max-h-32 overflow-auto rounded-lg bg-[var(--bg-code)] p-3 text-xs text-[var(--color-code-text)]">
            {JSON.stringify(request.args, null, 2)}
          </pre>
          {request.prompt && <p className="mt-2 text-sm text-red-700 dark:text-red-400">{request.prompt}</p>}

          <div className="mt-3 flex items-center gap-2">
            <button
              onClick={onApprove}
              className="flex items-center gap-1.5 rounded-lg bg-green-600 px-4 py-2 text-sm font-medium text-white transition-colors hover:bg-green-700"
            >
              <Check size={14} /> 批准
            </button>
            {!showReject ? (
              <button
                onClick={() => setShowReject(true)}
                className="flex items-center gap-1.5 rounded-lg bg-red-600 px-4 py-2 text-sm font-medium text-white transition-colors hover:bg-red-700"
              >
                <X size={14} /> 拒绝
              </button>
            ) : (
              <div className="flex flex-1 gap-2">
                <input
                  value={reason}
                  onChange={(e) => setReason(e.target.value)}
                  className="flex-1 rounded-lg border border-red-300 px-3 py-1.5 text-sm outline-none dark:border-red-700 dark:bg-[var(--bg-input)] dark:text-[var(--text-primary)]"
                  placeholder="拒绝原因..."
                  autoFocus
                />
                <button
                  onClick={() => onReject(reason || undefined)}
                  className="rounded-lg bg-red-600 px-3 py-1.5 text-sm font-medium text-white"
                >
                  确认
                </button>
                <button
                  onClick={() => setShowReject(false)}
                  className="rounded-lg bg-gray-100 px-3 py-1.5 text-sm text-gray-700 dark:bg-gray-700 dark:text-gray-200"
                >
                  取消
                </button>
              </div>
            )}
          </div>
        </div>
      </div>
    </div>
  );
}
