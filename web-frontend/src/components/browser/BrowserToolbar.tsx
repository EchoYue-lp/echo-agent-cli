import { useEffect, useState } from 'react';
import {
  AppWindow,
  ArrowLeft,
  ArrowRight,
  Chrome,
  RefreshCw,
  RotateCw,
  Square,
  X,
} from 'lucide-react';

export function BrowserToolbar({
  url,
  busy,
  onNavigate,
  onBack,
  onReload,
  onStop,
  onRefreshFrame,
  onClose,
  backend,
  chromeConnected,
  onBackendChange,
}: {
  url: string;
  busy: boolean;
  onNavigate: (url: string) => void;
  onBack: () => void;
  onReload: () => void;
  onStop: () => void;
  onRefreshFrame: () => void;
  onClose: () => void;
  backend: 'managed' | 'chrome';
  chromeConnected: boolean;
  onBackendChange: (backend: 'managed' | 'chrome') => void;
}) {
  const [value, setValue] = useState(url);
  useEffect(() => setValue(url), [url]);
  const submit = () => {
    const trimmed = value.trim();
    if (!trimmed) return;
    onNavigate(
      /^https?:\/\//i.test(trimmed) || trimmed.startsWith('about:') ? trimmed : `https://${trimmed}`
    );
  };
  const iconClass =
    'flex h-7 w-7 shrink-0 items-center justify-center rounded-md text-[var(--text-tertiary)] transition-colors hover:bg-[var(--bg-hover)] hover:text-[var(--text-primary)] disabled:opacity-35';
  return (
    <div className="flex h-10 shrink-0 items-center gap-1 border-b border-[var(--border-primary)] bg-[var(--bg-primary)] px-2">
      <button className={iconClass} onClick={onBack} title="后退">
        <ArrowLeft size={14} />
      </button>
      <button className={iconClass} disabled title="前进暂不可用">
        <ArrowRight size={14} />
      </button>
      <button
        className={iconClass}
        onClick={busy ? onStop : onReload}
        title={busy ? '停止' : '刷新'}
      >
        {busy ? <Square size={12} /> : <RotateCw size={13} />}
      </button>
      <input
        value={value}
        onChange={(event) => setValue(event.target.value)}
        onKeyDown={(event) => {
          if (event.key === 'Enter') submit();
        }}
        className="h-7 min-w-0 flex-1 rounded-md border border-[var(--border-primary)] bg-[var(--bg-chat)] px-2.5 text-xs text-[var(--text-primary)] outline-none focus:border-[var(--accent)]"
        aria-label="地址"
        spellCheck={false}
      />
      <button className={iconClass} onClick={onRefreshFrame} title="刷新画面">
        <RefreshCw size={13} />
      </button>
      <div className="flex h-7 shrink-0 items-center rounded-md border border-[var(--border-primary)] p-0.5">
        <button
          className={`${iconClass} h-6 w-6 ${backend === 'managed' ? 'bg-[var(--bg-hover)] text-[var(--text-primary)]' : ''}`}
          onClick={() => onBackendChange('managed')}
          title="托管 Chromium"
        >
          <AppWindow size={12} />
        </button>
        <button
          className={`${iconClass} h-6 w-6 ${backend === 'chrome' ? 'bg-[var(--bg-hover)] text-[var(--text-primary)]' : ''}`}
          onClick={() => onBackendChange('chrome')}
          title={chromeConnected ? '已连接 Chrome' : 'Chrome 扩展未连接'}
        >
          <Chrome size={12} />
        </button>
      </div>
      <button className={iconClass} onClick={onClose} title="关闭浏览器面板">
        <X size={14} />
      </button>
    </div>
  );
}
