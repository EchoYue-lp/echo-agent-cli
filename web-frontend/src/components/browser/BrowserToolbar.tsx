import { useEffect, useState } from 'react';
import { ArrowLeft, Camera, CornerDownLeft, Plus, RotateCw, Square } from 'lucide-react';

export function BrowserToolbar({
  url,
  busy,
  onNavigate,
  onBack,
  onReload,
  onStop,
  onRefreshFrame,
  onNewTab,
  backend,
  chromeConnected,
  onBackendChange,
  onChromeSetup,
}: {
  url: string;
  busy: boolean;
  onNavigate: (url: string) => void;
  onBack: () => void;
  onReload: () => void;
  onStop: () => void;
  onRefreshFrame: () => void;
  onNewTab: () => void;
  backend: 'managed' | 'chrome';
  chromeConnected: boolean;
  onBackendChange: (backend: 'managed' | 'chrome') => void;
  onChromeSetup: () => void;
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
    <form
      className="flex h-10 shrink-0 items-center gap-1 border-b border-[var(--border-primary)] bg-[var(--bg-primary)] px-2"
      onSubmit={(event) => {
        event.preventDefault();
        submit();
      }}
    >
      <button type="button" className={iconClass} onClick={onBack} title="后退">
        <ArrowLeft size={14} />
      </button>
      <button
        type="button"
        className={iconClass}
        onClick={busy ? onStop : onReload}
        title={busy ? '停止' : '刷新'}
      >
        {busy ? <Square size={12} /> : <RotateCw size={13} />}
      </button>
      <input
        value={value}
        onChange={(event) => setValue(event.target.value)}
        className="h-7 min-w-0 flex-1 rounded-md border border-[var(--border-primary)] bg-[var(--bg-chat)] px-2.5 text-xs text-[var(--text-primary)] outline-none focus:border-[var(--accent)]"
        aria-label="地址"
        placeholder="输入网址"
        spellCheck={false}
      />
      <button type="submit" className={iconClass} disabled={busy || !value.trim()} title="前往">
        <CornerDownLeft size={13} />
      </button>
      <button type="button" className={iconClass} onClick={onRefreshFrame} title="更新页面截图">
        <Camera size={13} />
      </button>
      <button type="button" className={iconClass} onClick={onNewTab} title="新建标签页">
        <Plus size={13} />
      </button>
      <label className="sr-only" htmlFor="browser-backend">
        浏览器模式
      </label>
      <select
        id="browser-backend"
        value={backend}
        onChange={(event) => {
          const next = event.target.value as 'managed' | 'chrome';
          if (next === 'chrome' && !chromeConnected) {
            onChromeSetup();
            return;
          }
          onBackendChange(next);
        }}
        className="h-7 w-[88px] shrink-0 rounded-md border border-[var(--border-primary)] bg-[var(--bg-chat)] px-1.5 text-[11px] text-[var(--text-secondary)] outline-none focus:border-[var(--accent)]"
        title={chromeConnected ? '选择浏览器模式' : '连接 Chrome'}
        aria-label="浏览器模式"
      >
        <option value="managed">内置</option>
        <option value="chrome">{chromeConnected ? 'Chrome' : '连接 Chrome…'}</option>
      </select>
    </form>
  );
}
