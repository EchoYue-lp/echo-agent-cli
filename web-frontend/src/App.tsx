import { useCallback } from 'react';
import { AppLayout } from './components/layout/AppLayout';
import { LeftSidebar } from './components/layout/LeftSidebar';
import { ChatPanel } from './components/chat/ChatPanel';
import { SettingsDialog } from './components/layout/SettingsDialog';
import { ToastContainer } from './components/common/Toast';
import { useKeyboardShortcuts } from './hooks/useKeyboardShortcuts';
import { useConversationStore } from './stores/conversationStore';
import { useUiStore } from './stores/uiStore';
import { RequireAuth } from './components/Auth/RequireAuth';

function App() {
  const startNew = useConversationStore((s) => s.startNew);
  const toggleSidebar = useUiStore((s) => s.toggleLeftSidebar);

  useKeyboardShortcuts({
    onNewChat: useCallback(() => { startNew(); }, [startNew]),
    onToggleSidebar: useCallback(() => { toggleSidebar(); }, [toggleSidebar]),
  });

  return (
    <RequireAuth>
      <AppLayout
        left={<LeftSidebar />}
        center={<ChatPanel />}
      />
      <SettingsDialog />
      <ToastContainer />
    </RequireAuth>
  );
}

export default App;
