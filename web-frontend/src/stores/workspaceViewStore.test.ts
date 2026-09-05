import { beforeEach, describe, expect, it } from 'vitest';
import { useWorkspaceViewStore } from './workspaceViewStore';

describe('useWorkspaceViewStore', () => {
  beforeEach(() => useWorkspaceViewStore.setState({ activeView: 'chat' }));

  it.each(['analysis', 'research', 'workflow', 'extract'] as const)(
    'opens the %s workbench independently from contextual panes',
    (view) => {
      useWorkspaceViewStore.getState().open(view);
      expect(useWorkspaceViewStore.getState().activeView).toBe(view);
      useWorkspaceViewStore.getState().openChat();
      expect(useWorkspaceViewStore.getState().activeView).toBe('chat');
    }
  );
});
