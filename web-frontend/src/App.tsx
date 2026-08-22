import { useCallback, useState, useMemo, useEffect } from 'react';
import { AppLayout } from './components/layout/AppLayout';
import { LeftSidebar } from './components/layout/LeftSidebar';
import { RightWorkspace } from './components/layout/RightWorkspace';
import { ChatPanel } from './components/chat/ChatPanel';
import { SettingsDialog } from './components/layout/SettingsDialog';
import { InterruptPromptDialog } from './components/task/TaskRuntimePanel';
import { ToastContainer } from './components/common/Toast';
import { ErrorBoundary } from './components/common/ErrorBoundary';
import CommandPalette, { type CommandItem } from './components/common/CommandPalette';
import NewTaskDialog from './components/workspace/NewTaskDialog';
import { useKeyboardShortcuts } from './hooks/useKeyboardShortcuts';
import { useConversationStore } from './stores/conversationStore';
import { applyPluginTheme, useUiStore } from './stores/uiStore';
import { pluginApi } from './api/endpoints';
import { useWorkspaceStore } from './stores/workspaceStore';
import { useTaskRuntimeStore } from './stores/taskRuntimeStore';
import { RequireAuth } from './components/Auth/RequireAuth';
import { workspaceIdForView } from './lib/viewAddress';

function App() {
  const initConversations = useConversationStore((s) => s.init);
  const activeId = useConversationStore((s) => s.activeId);
  const loadTaskRun = useTaskRuntimeStore((s) => s.loadByConversation);
  const toggleSidebar = useUiStore((s) => s.toggleLeftSidebar);
  const openSettings = useUiStore((s) => s.openSettings);
  const setActiveSettingsTab = useUiStore((s) => s.setActiveSettingsTab);
  const toggleTheme = useUiStore((s) => s.toggleTheme);
  const toggleTerminal = useUiStore((s) => s.toggleTerminal);
  const initWorkspaces = useWorkspaceStore((s) => s.init);
  const currentWorkspaceId = useWorkspaceStore((s) => workspaceIdForView(s.current?.id));

  const [paletteOpen, setPaletteOpen] = useState(false);
  const [newTaskOpen, setNewTaskOpen] = useState(false);

  // Initialize workspace and conversation stores on mount
  useEffect(() => {
    const initialize = async () => {
      await initWorkspaces();
      const workspaceId = workspaceIdForView(useWorkspaceStore.getState().current?.id);
      await initConversations(workspaceId);
    };
    void initialize();
  }, [initWorkspaces, initConversations]);

  useEffect(() => {
    pluginApi
      .themes()
      .then((result) => {
        const active = result.themes.find((theme) => theme.name === result.active) || null;
        applyPluginTheme(active);
      })
      .catch(() => undefined);
  }, []);

  // Load the active conversation's TaskRuntime run so the main-window
  // ParallelExecutionBlock and the RightRail can render subagent state.
  // (Previously this was triggered by the now-removed TaskRuntimePanel.)
  useEffect(() => {
    if (activeId) {
      void loadTaskRun(currentWorkspaceId, activeId);
    } else {
      useTaskRuntimeStore.getState().reset();
    }
  }, [activeId, currentWorkspaceId, loadTaskRun]);

  const handleNewTask = useCallback(() => {
    setNewTaskOpen(true);
  }, []);

  useKeyboardShortcuts({
    onNewChat: handleNewTask,
    onToggleSidebar: useCallback(() => {
      toggleSidebar();
    }, [toggleSidebar]),
    onCommandPalette: useCallback(() => {
      setPaletteOpen((o) => !o);
    }, []),
  });

  const commands: CommandItem[] = useMemo(
    () => [
      {
        id: 'new-chat',
        label: 'New Task',
        description: 'Create a new task workspace',
        action: () => {
          setNewTaskOpen(true);
        },
        category: 'Chat',
      },
      {
        id: 'open-settings',
        label: 'Open Settings',
        description: 'Open the settings dialog',
        action: () => {
          openSettings();
        },
        category: 'Navigation',
      },
      {
        id: 'settings-tools',
        label: 'Settings: Tools',
        description: 'Manage tool configuration',
        action: () => {
          setActiveSettingsTab('tools');
        },
        category: 'Settings',
      },
      {
        id: 'settings-mcp',
        label: 'Settings: MCP Servers',
        description: 'Manage MCP server connections',
        action: () => {
          setActiveSettingsTab('mcp');
        },
        category: 'Settings',
      },
      {
        id: 'settings-skills',
        label: 'Settings: Skills',
        description: 'Browse and manage skills',
        action: () => {
          setActiveSettingsTab('skills');
        },
        category: 'Settings',
      },
      {
        id: 'settings-memory',
        label: 'Settings: Memory',
        description: 'View agent memory entries',
        action: () => {
          setActiveSettingsTab('memory');
        },
        category: 'Settings',
      },
      {
        id: 'settings-observability',
        label: 'Settings: Observability',
        description: 'Inspect token, cache, model, and session trends',
        action: () => {
          setActiveSettingsTab('observability');
        },
        category: 'Settings',
      },
      {
        id: 'settings-evolution',
        label: 'Settings: Self-Evolution',
        description: 'Memory review, rule proposals, and skill curation',
        action: () => {
          setActiveSettingsTab('evolution');
        },
        category: 'Settings',
      },
      {
        id: 'toggle-theme',
        label: 'Toggle Theme',
        description: 'Switch between light and dark mode',
        action: () => {
          toggleTheme();
        },
        category: 'Appearance',
      },
      {
        id: 'toggle-sidebar',
        label: 'Toggle Sidebar',
        description: 'Show or hide the left sidebar',
        action: () => {
          toggleSidebar();
        },
        category: 'Appearance',
      },
      {
        id: 'toggle-terminal',
        label: 'Toggle Terminal',
        description: 'Show or hide the integrated terminal',
        action: () => {
          toggleTerminal();
        },
        category: 'Navigation',
      },
    ],
    [openSettings, setActiveSettingsTab, toggleTheme, toggleSidebar, toggleTerminal]
  );

  return (
    <ErrorBoundary>
      <RequireAuth>
        <AppLayout
          left={<LeftSidebar onNewTask={handleNewTask} />}
          center={<ChatPanel />}
          right={<RightWorkspace />}
        />
        <SettingsDialog />
        <NewTaskDialog isOpen={newTaskOpen} onClose={() => setNewTaskOpen(false)} />
        <CommandPalette
          isOpen={paletteOpen}
          onClose={() => setPaletteOpen(false)}
          commands={commands}
        />
        <ToastContainer />
        <InterruptPromptDialog />
      </RequireAuth>
    </ErrorBoundary>
  );
}

export default App;
