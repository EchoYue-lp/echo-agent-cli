/**
 * Tauri Bridge — abstraction layer.
 *
 * In Tauri environment, critical paths use IPC (low latency, native experience).
 * In Web environment, everything goes through HTTP.
 */

// Detect Tauri environment
const isTauri = (): boolean => {
  return typeof window !== 'undefined' && '__TAURI_INTERNALS__' in window;
};

// Dynamic import for Tauri invoke
async function invoke<T>(cmd: string, args?: Record<string, unknown>): Promise<T> {
  if (!isTauri()) {
    throw new Error('Not in Tauri environment');
  }
  const { invoke: tauriInvoke } = await import('@tauri-apps/api/core');
  return tauriInvoke<T>(cmd, args);
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
        // @ts-ignore - @tauri-apps/plugin-shell may not be installed in dev
        const shell = await import('@tauri-apps/plugin-shell');
        await shell.open(path);
      } catch (e) {
        console.warn('Failed to open path via Tauri shell:', e);
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

export { isTauri };
