import { useCallback, useEffect, useState } from 'react';
import {
  CheckCircle2,
  Chrome,
  ExternalLink,
  FolderOpen,
  LoaderCircle,
  RefreshCw,
  X,
} from 'lucide-react';
import { apiInvoke, errorMessage, isTauri } from '../../lib/tauri-bridge';

export interface ChromeSetupStatus {
  enabled: boolean;
  connected: boolean;
  extensionOrigin?: string | null;
  endpointFile: string;
  startupError?: string | null;
  nativeHostInstalled: boolean;
  extensionPath?: string | null;
}

export function isChromeExtensionId(value: string): boolean {
  return /^[a-p]{32}$/.test(value);
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
  const [extensionId, setExtensionId] = useState('');
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

  const invoke = async (command: string) => {
    setBusy(command);
    setError(null);
    try {
      await apiInvoke(command);
    } catch (invokeError) {
      setError(errorMessage(invokeError));
    } finally {
      setBusy(null);
    }
  };

  const installHost = async () => {
    const normalizedId = extensionId.trim().toLowerCase();
    if (!isChromeExtensionId(normalizedId)) {
      setError('扩展 ID 应为 32 位 a-p 小写字母');
      return;
    }
    setBusy('chrome_install_native_host');
    setError(null);
    try {
      await apiInvoke('chrome_install_native_host', { extensionId: normalizedId });
      await refresh();
    } catch (installError) {
      setError(errorMessage(installError));
    } finally {
      setBusy(null);
    }
  };

  const useChrome = async () => {
    setBusy('browser_set_backend');
    setError(null);
    const backendError = await onUseChrome();
    if (backendError) setError(backendError);
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
            <StatusCell label="桌面桥接" ready={Boolean(status?.connected)} />
            <StatusCell
              label="Native Host"
              ready={Boolean(status?.nativeHostInstalled)}
              readyLabel="已注册"
            />
          </div>

          <section className="space-y-2">
            <div className="text-xs font-medium text-[var(--text-primary)]">1. 加载 EKO 扩展</div>
            <div className="flex flex-wrap gap-2">
              <button
                type="button"
                className={buttonClass}
                disabled={busy !== null}
                onClick={() => void invoke('chrome_open_extensions_page')}
              >
                <ExternalLink size={13} />
                打开 Chrome 扩展页
              </button>
              <button
                type="button"
                className={buttonClass}
                disabled={busy !== null || !status?.extensionPath}
                onClick={() => void invoke('chrome_open_extension_dir')}
              >
                <FolderOpen size={13} />
                打开扩展目录
              </button>
            </div>
          </section>

          <section className="space-y-2">
            <label
              htmlFor="chrome-extension-id"
              className="block text-xs font-medium text-[var(--text-primary)]"
            >
              2. 注册扩展 ID
            </label>
            <div className="flex gap-2">
              <input
                id="chrome-extension-id"
                value={extensionId}
                onChange={(event) => setExtensionId(event.target.value)}
                maxLength={32}
                spellCheck={false}
                placeholder="Chrome 扩展 ID"
                className="h-8 min-w-0 flex-1 rounded-md border border-[var(--border-primary)] bg-[var(--bg-chat)] px-2.5 font-mono text-xs text-[var(--text-primary)] outline-none focus:border-[var(--accent)]"
              />
              <button
                type="button"
                className={buttonClass}
                disabled={busy !== null || !isChromeExtensionId(extensionId.trim().toLowerCase())}
                onClick={() => void installHost()}
              >
                {busy === 'chrome_install_native_host' ? (
                  <LoaderCircle size={13} className="animate-spin" />
                ) : (
                  <CheckCircle2 size={13} />
                )}
                注册
              </button>
            </div>
          </section>

          <section className="flex items-center justify-between gap-3 border-t border-[var(--border-primary)] pt-4">
            <div className="min-w-0 text-xs text-[var(--text-tertiary)]">
              {status?.connected ? '3. 在扩展中授权当前标签页' : '等待 Chrome 扩展连接'}
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
                disabled={!status?.connected || busy !== null}
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
