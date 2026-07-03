/**
 * Tauri Bridge — abstraction layer.
 *
 * In Tauri environment, critical paths use IPC (low latency, native experience).
 * In Web environment, everything goes through HTTP.
 */

declare global {
  interface Window {
    __TAURI__?: unknown;
    __TAURI_INTERNALS__?: unknown;
  }
}

// Detect Tauri environment. In dev mode the app is served from Vite over http,
// so protocol-only checks are not enough.
// P2-8: 环境在运行期不变, 一次性 memoize。此前每次调用重算 5 个检测, isTauri()
// 在 endpoints.ts 每个 API 方法里内联调用 (20+ 处), 属热路径反复重算。
const IS_TAURI: boolean = (() => {
  if (typeof window === 'undefined') return false;
  const hasTauriGlobals =
    typeof window.__TAURI_INTERNALS__ !== 'undefined' || typeof window.__TAURI__ !== 'undefined';
  const hasTauriProtocol = window.location.protocol === 'tauri:';
  const hasTauriUserAgent = navigator.userAgent.toLowerCase().includes('tauri');
  const hasTauriDevFlag = new URLSearchParams(window.location.search).has('tauri');
  const hasTauriViteMode = import.meta.env.VITE_EKO_TAURI === '1';
  return (
    hasTauriGlobals || hasTauriProtocol || hasTauriUserAgent || hasTauriDevFlag || hasTauriViteMode
  );
})();

// 保留 isTauri() 函数形式以兼容现有调用点 (大量 `isTauri() ?` 内联)。
const isTauri = (): boolean => IS_TAURI;

// Dynamic import for Tauri invoke
async function invoke<T>(cmd: string, args?: Record<string, unknown>): Promise<T> {
  if (!isTauri()) {
    throw new Error('Not in Tauri environment');
  }
  const { invoke: tauriInvoke } = await import('@tauri-apps/api/core');
  return tauriInvoke<T>(cmd, args);
}

export function errorMessage(error: unknown): string {
  if (error instanceof Error) return error.message;
  if (typeof error === 'string') return error;
  if (error && typeof error === 'object') {
    const record = error as Record<string, unknown>;
    if (typeof record.message === 'string') return record.message;
    if (typeof record.error === 'string') return record.error;
    try {
      return JSON.stringify(record);
    } catch {
      return 'Unknown error';
    }
  }
  return String(error);
}

// ── File System (IPC in Tauri, HTTP in Web) ──

export const fileSystem = {
  async readFile(path: string): Promise<{ content: string; size: number; path: string }> {
    if (isTauri()) {
      return invoke<{ content: string; size: number; path: string }>('native_read_file', {
        path,
      });
    }
    const resp = await fetch(`/api/files/read?path=${encodeURIComponent(path)}`);
    return resp.json();
  },

  async writeFile(path: string, content: string): Promise<void> {
    if (isTauri()) {
      return invoke('native_write_file', { path, content });
    }
    await fetch('/api/files/write', {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ path, content }),
    });
  },

  /**
   * Open a native directory picker dialog.
   * Returns the selected path, or null if cancelled.
   * In Web mode, returns null (caller should show text input fallback).
   */
  async selectDirectory(title?: string): Promise<string | null> {
    if (isTauri()) {
      const { open } = await import('@tauri-apps/plugin-dialog');
      const selected = await open({
        directory: true,
        multiple: false,
        title: title || '选择工作区目录',
      });
      return selected as string | null;
    }
    return null;
  },

  /**
   * Open a path in the system file explorer.
   * In Tauri, uses the shell plugin to open the folder.
   * In Web mode, does nothing (not supported).
   */
  async openPath(path: string): Promise<void> {
    if (isTauri()) {
      try {
        const shell = await import('@tauri-apps/plugin-shell');
        await shell.open(path);
      } catch (e) {
        console.error('Failed to open path via Tauri shell:', e, 'Path:', path);
        // Fallback: try using invoke directly
        try {
          await invoke('native_open_path', { path });
        } catch (e2) {
          console.error('Fallback also failed:', e2);
          alert(`无法打开文件夹: ${path}\n请手动在 Finder 中打开`);
        }
      }
    }
    // Web mode: not supported, silently ignore
  },
};

// ── Notifications (IPC in Tauri, browser fallback in Web) ──

export const notifications = {
  async notify(title: string, body: string): Promise<void> {
    if (isTauri()) {
      return invoke('native_notify', { title, body });
    }
    // Web fallback: browser notification
    if ('Notification' in window && Notification.permission === 'granted') {
      new Notification(title, { body });
    }
  },
};

// ── System Info ──

export const system = {
  async getInfo(): Promise<{ os: string; arch: string; home_dir: string }> {
    if (isTauri()) {
      return invoke<{ os: string; arch: string; home_dir: string }>('get_system_info');
    }
    return { os: 'web', arch: 'web', home_dir: '' };
  },
};

/**
 * Generic Tauri IPC invoke for API endpoints.
 * Use this in endpoints.ts to replace HTTP calls with Tauri commands.
 */
export async function apiInvoke<T>(command: string, args?: Record<string, unknown>): Promise<T> {
  if (!isTauri()) {
    throw new Error('apiInvoke requires Tauri environment');
  }
  try {
    return await invoke<T>(command, args);
  } catch (error) {
    throw new Error(errorMessage(error));
  }
}

export { isTauri };
