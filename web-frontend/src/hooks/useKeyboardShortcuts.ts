import { useEffect } from 'react';

interface ShortcutsConfig {
  onNewChat?: () => void;
  onToggleSidebar?: () => void;
  onCommandPalette?: () => void;
}

export function useKeyboardShortcuts({
  onNewChat,
  onToggleSidebar,
  onCommandPalette,
}: ShortcutsConfig) {
  useEffect(() => {
    const handleKeyDown = (e: KeyboardEvent) => {
      // Cmd/Ctrl + Shift + O: New Chat
      if ((e.metaKey || e.ctrlKey) && e.shiftKey && e.key === 'o') {
        e.preventDefault();
        onNewChat?.();
      }

      // Cmd/Ctrl + Shift + S: Toggle sidebar
      if ((e.metaKey || e.ctrlKey) && e.shiftKey && e.key === 's') {
        e.preventDefault();
        onToggleSidebar?.();
      }

      // Cmd/Ctrl + K: Command Palette
      if ((e.metaKey || e.ctrlKey) && e.key === 'k') {
        e.preventDefault();
        onCommandPalette?.();
      }

      // Escape: handled by individual components (SettingsDialog, etc.)
    };

    window.addEventListener('keydown', handleKeyDown);
    return () => {
      window.removeEventListener('keydown', handleKeyDown);
    };
  }, [onNewChat, onToggleSidebar, onCommandPalette]);
}
