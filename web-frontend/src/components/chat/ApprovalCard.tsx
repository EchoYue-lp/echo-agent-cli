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
    <div
      className="animate-pulse-border rounded-xl border-2 p-4"
      style={{
        background: '#fef2f2',
        borderColor: '#fca5a5',
      }}
    >
      <div className="flex items-start gap-3">
        <div className="flex h-8 w-8 shrink-0 items-center justify-center rounded-lg" style={{ background: '#fee2e2' }}>
          <ShieldAlert size={16} style={{ color: '#dc2626' }} />
        </div>
        <div className="flex-1">
          <p className="text-sm font-semibold" style={{ color: '#991b1b' }}>Approval Required</p>
          <p className="mt-1 text-sm" style={{ color: '#b91c1c' }}>
            Tool: <code className="rounded px-1.5 py-0.5 text-xs font-mono" style={{ background: '#fee2e2' }}>{request.toolName}</code>
          </p>
          <pre
            className="mt-2 max-h-32 overflow-auto rounded-lg p-3 text-xs"
            style={{ background: 'var(--bg-code)', color: '#e2e8f0' }}
          >
            {JSON.stringify(request.args, null, 2)}
          </pre>
          {request.prompt && <p className="mt-2 text-sm" style={{ color: '#b91c1c' }}>{request.prompt}</p>}

          <div className="mt-3 flex items-center gap-2">
            <button
              onClick={onApprove}
              className="flex items-center gap-1.5 rounded-lg px-4 py-2 text-sm font-medium text-white transition-colors"
              style={{ background: '#16a34a' }}
            >
              <Check size={14} /> Approve
            </button>
            {!showReject ? (
              <button
                onClick={() => setShowReject(true)}
                className="flex items-center gap-1.5 rounded-lg px-4 py-2 text-sm font-medium text-white transition-colors"
                style={{ background: '#dc2626' }}
              >
                <X size={14} /> Reject
              </button>
            ) : (
              <div className="flex flex-1 gap-2">
                <input
                  value={reason}
                  onChange={(e) => setReason(e.target.value)}
                  className="flex-1 rounded-lg px-3 py-1.5 text-sm outline-none"
                  style={{ border: '1px solid #fca5a5' }}
                  placeholder="Reason..."
                  autoFocus
                />
                <button
                  onClick={() => onReject(reason || undefined)}
                  className="rounded-lg px-3 py-1.5 text-sm font-medium text-white"
                  style={{ background: '#dc2626' }}
                >
                  Confirm
                </button>
                <button
                  onClick={() => setShowReject(false)}
                  className="rounded-lg px-3 py-1.5 text-sm"
                  style={{ background: '#f3f4f6', color: '#374151' }}
                >
                  Cancel
                </button>
              </div>
            )}
          </div>
        </div>
      </div>
    </div>
  );
}
