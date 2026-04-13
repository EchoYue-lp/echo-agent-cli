import { useCallback } from 'react';
import { AppLayout } from './components/layout/AppLayout';
import { LeftSidebar } from './components/layout/LeftSidebar';
import { ChatPanel } from './components/chat/ChatPanel';
import { RightPanel } from './components/layout/RightPanel';
import { useKeyboardShortcuts } from './hooks/useKeyboardShortcuts';
import { useConversationStore } from './stores/conversationStore';
import { useUiStore } from './stores/uiStore';

function App() {
  const startNew = useConversationStore((s) => s.startNew);
  const toggleSidebar = useUiStore((s) => s.toggleLeftSidebar);

  useKeyboardShortcuts({
    onNewChat: useCallback(() => { startNew(); }, [startNew]),
    onToggleSidebar: useCallback(() => { toggleSidebar(); }, [toggleSidebar]),
  });

  return (
    <AppLayout
      left={<LeftSidebar />}
      center={<ChatPanel />}
      right={<RightPanel />}
    />
  );
}

export default App;
