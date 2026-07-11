import { AlertCircle, Loader2, WifiOff } from 'lucide-react';
import type { BrowserStatus as Status } from '../../stores/browserStore';

export function BrowserStatus({ status, error }: { status?: Status; error?: string | null }) {
  if (error) {
    return (
      <div className="flex min-w-0 items-center gap-1.5 text-[11px] text-[var(--color-error)]">
        <AlertCircle size={12} />
        <span className="truncate">{error}</span>
      </div>
    );
  }
  if (status === 'navigating' || status === 'acting' || status === 'starting') {
    return (
      <div className="flex items-center gap-1.5 text-[11px] text-[var(--text-secondary)]">
        <Loader2 size={12} className="animate-spin" />
        <span>{status === 'navigating' ? '正在加载' : '正在操作'}</span>
      </div>
    );
  }
  if (status === 'waiting_confirmation') {
    return (
      <div className="flex items-center gap-1.5 text-[11px] text-[var(--color-warning)]">
        <AlertCircle size={12} />
        <span>等待确认</span>
      </div>
    );
  }
  if (!status || status === 'closed') {
    return (
      <div className="flex items-center gap-1.5 text-[11px] text-[var(--text-tertiary)]">
        <WifiOff size={12} />
        <span>未连接</span>
      </div>
    );
  }
  return (
    <div className="flex items-center gap-1.5 text-[11px] text-[var(--text-tertiary)]">
      <span className="h-1.5 w-1.5 rounded-full bg-[var(--color-success)]" />
      <span>就绪</span>
    </div>
  );
}
