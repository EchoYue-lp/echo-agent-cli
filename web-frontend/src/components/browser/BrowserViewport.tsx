import { Globe2 } from 'lucide-react';

export function BrowserViewport({ frame, busy }: { frame?: string | null; busy: boolean }) {
  return (
    <div className="relative min-h-0 flex-1 overflow-auto bg-white">
      {frame ? (
        <img
          src={frame}
          alt="浏览器页面截图"
          className="block h-auto min-h-full w-full object-contain object-top"
        />
      ) : (
        <div className="flex h-full min-h-[240px] flex-col items-center justify-center gap-2 bg-[var(--bg-chat)] text-[var(--text-tertiary)]">
          <Globe2 size={28} strokeWidth={1.4} />
          <span className="text-xs">浏览器画面将在操作后显示</span>
        </div>
      )}
      {busy && (
        <div className="pointer-events-none absolute inset-x-0 top-0 h-0.5 overflow-hidden bg-[var(--border-primary)]">
          <div className="h-full w-1/3 animate-pulse bg-[var(--accent)]" />
        </div>
      )}
    </div>
  );
}
