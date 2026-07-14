import { useCallback, useEffect, useState } from 'react';
import { Chrome, ExternalLink, RefreshCw, X } from 'lucide-react';
import { apiInvoke, errorMessage, isTauri } from '../../lib/tauri-bridge';

export interface ChromeSetupStatus {
  enabled: boolean;
  connected: boolean;
  tokenConfigured: boolean;
  package: string;
  startupError?: string | null;
}

export function ChromeSetupDialog({
  onClose,
  onConnectionChange,
  onUseChrome,
}: {
  onClose: () => void;
  onConnectionChange: (connected: boolean) => void;
  onUseChrome: () => Promise<string | null>;
}) {
  const [status, setStatus] = useState<ChromeSetupStatus | null>(null);
  const [busy, setBusy] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);

  const refresh = useCallback(async () => {
    if (!isTauri()) {
      setError('Chrome 连接仅在 EKO 桌面应用中可用');
      return;
    }
    try {
      const next = await apiInvoke<ChromeSetupStatus>('chrome_setup_status');
      setStatus(next);
      setError(next.startupError ?? null);
      onConnectionChange(next.connected);
    } catch (refreshError) {
      setError(errorMessage(refreshError));
      onConnectionChange(false);
    }
  }, [onConnectionChange]);

  useEffect(() => {
    void refresh();
    const timer = window.setInterval(() => void refresh(), 2000);
    return () => window.clearInterval(timer);
  }, [refresh]);

  const openExtensionPage = async () => {
    setBusy('chrome_open_extensions_page');
    setError(null);
    try {
      await apiInvoke('chrome_open_extensions_page');
    } catch (invokeError) {
      setError(errorMessage(invokeError));
    } finally {
      setBusy(null);
    }
  };

  const useChrome = async () => {
    setBusy('browser_set_backend');
    setError(null);
    const backendError = await onUseChrome();
    if (backendError) {
      setError(backendError);
    } else {
      onConnectionChange(true);
    }
    setBusy(null);
  };

  const buttonClass =
    'inline-flex h-8 items-center justify-center gap-1.5 rounded-md border border-[var(--border-primary)] bg-[var(--bg-primary)] px-3 text-xs text-[var(--text-secondary)] transition-colors hover:bg-[var(--bg-hover)] hover:text-[var(--text-primary)] disabled:cursor-not-allowed disabled:opacity-40';

  return (
    <div
      className="fixed inset-0 z-[70] flex items-center justify-center bg-black/45 p-4"
      role="dialog"
      aria-modal="true"
      aria-labelledby="chrome-setup-title"
      onMouseDown={(event) => event.target === event.currentTarget && onClose()}
    >
      <div className="flex w-full max-w-[520px] flex-col overflow-hidden rounded-lg border border-[var(--border-primary)] bg-[var(--bg-primary)] shadow-2xl">
        <header className="flex h-12 shrink-0 items-center justify-between border-b border-[var(--border-primary)] px-4">
          <div className="flex items-center gap-2">
            <Chrome size={17} className="text-[var(--accent)]" />
            <h2 id="chrome-setup-title" className="text-sm font-medium text-[var(--text-primary)]">
              连接 Chrome
            </h2>
          </div>
          <button
            type="button"
            className="flex h-7 w-7 items-center justify-center rounded-md text-[var(--text-tertiary)] hover:bg-[var(--bg-hover)] hover:text-[var(--text-primary)]"
            onClick={onClose}
            title="关闭"
          >
            <X size={15} />
          </button>
        </header>

        <div className="space-y-4 p-4">
          <div className="grid grid-cols-2 gap-px overflow-hidden rounded-md border border-[var(--border-primary)] bg-[var(--border-primary)]">
            <StatusCell label="扩展后端" ready={Boolean(status?.enabled)} readyLabel="已启用" />
            <StatusCell label="MCP 连接" ready={Boolean(status?.connected)} />
          </div>

          <section className="space-y-2">
            <div className="text-xs font-medium text-[var(--text-primary)]">
              1. 安装 Playwright Extension
            </div>
            <p className="text-xs leading-relaxed text-[var(--text-tertiary)]">
              官方扩展允许 EKO 连接现有 Chrome 标签页，并复用当前登录状态。
            </p>
            <button
              type="button"
              className={buttonClass}
              disabled={busy !== null}
              onClick={() => void openExtensionPage()}
            >
              <ExternalLink size={13} />从 Chrome Web Store 安装
            </button>
          </section>

          <section className="space-y-2">
            <div className="text-xs font-medium text-[var(--text-primary)]">
              2. 连接并选择标签页
            </div>
            <p className="text-xs leading-relaxed text-[var(--text-tertiary)]">
              点击“使用 Chrome”后，Playwright
              会打开标签页选择页面。选择本次任务可以控制的标签页并批准连接。
            </p>
            <div className="rounded-md bg-[var(--bg-secondary)] px-3 py-2 text-[11px] leading-relaxed text-[var(--text-tertiary)]">
              {status?.tokenConfigured
                ? '已配置免确认连接令牌。'
                : '可选：设置 EKO_BROWSER_EXTENSION_TOKEN，避免每次连接都手动批准。'}
            </div>
          </section>

          <section className="flex items-center justify-between gap-3 border-t border-[var(--border-primary)] pt-4">
            <div className="min-w-0 text-xs text-[var(--text-tertiary)]">
              {status?.connected ? 'Playwright Extension 已连接' : status?.package}
            </div>
            <div className="flex shrink-0 gap-2">
              <button
                type="button"
                className={buttonClass}
                disabled={busy !== null}
                onClick={() => void refresh()}
                title="重新检测"
              >
                <RefreshCw size={13} />
              </button>
              <button
                type="button"
                className="h-8 rounded-md bg-[var(--accent)] px-3 text-xs font-medium text-white disabled:cursor-not-allowed disabled:opacity-40"
                disabled={!status?.enabled || busy !== null}
                onClick={() => void useChrome()}
              >
                {busy === 'browser_set_backend' ? '正在连接…' : '使用 Chrome'}
              </button>
            </div>
          </section>

          {error && (
            <div className="rounded-md border border-[var(--color-error)]/30 bg-[var(--color-error)]/5 px-3 py-2 text-xs text-[var(--color-error)]">
              {error}
            </div>
          )}
        </div>
      </div>
    </div>
  );
}

function StatusCell({
  label,
  ready,
  readyLabel = '已连接',
}: {
  label: string;
  ready: boolean;
  readyLabel?: string;
}) {
  return (
    <div className="flex items-center justify-between bg-[var(--bg-chat)] px-3 py-2 text-xs">
      <span className="text-[var(--text-secondary)]">{label}</span>
      <span className={ready ? 'text-[var(--color-success)]' : 'text-[var(--text-tertiary)]'}>
        {ready ? readyLabel : '未连接'}
      </span>
    </div>
  );
}
