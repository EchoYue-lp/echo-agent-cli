import { useEffect } from 'react';
import { isTauri } from '../lib/tauri-bridge';
import { type BrowserEvent, useBrowserStore } from '../stores/browserStore';

export function useBrowserEvents() {
  useEffect(() => {
    if (!isTauri()) return;
    let disposed = false;
    let unlisten: (() => void) | null = null;
    import('@tauri-apps/api/event')
      .then(({ listen }) =>
        listen<BrowserEvent>('browser://event', ({ payload }) => {
          if (!disposed) useBrowserStore.getState().ingest(payload);
        })
      )
      .then((cleanup) => {
        if (disposed) cleanup();
        else unlisten = cleanup;
      })
      .catch((error) => console.warn('[Browser] event listener failed:', error));
    return () => {
      disposed = true;
      unlisten?.();
    };
  }, []);
}
