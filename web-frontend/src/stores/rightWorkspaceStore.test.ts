import { describe, expect, it } from 'vitest';
import {
  boundRightWorkspaceWidth,
  rightWorkspaceWidthForViewport,
  useRightWorkspaceStore,
} from './rightWorkspaceStore';

describe('boundRightWorkspaceWidth', () => {
  it('keeps the right workspace within usable desktop bounds', () => {
    expect(boundRightWorkspaceWidth(200)).toBe(380);
    expect(boundRightWorkspaceWidth(560)).toBe(560);
    expect(boundRightWorkspaceWidth(1000)).toBe(760);
  });
});

describe('rightWorkspaceWidthForViewport', () => {
  it('uses an overlay-sized panel below the desktop split breakpoint', () => {
    expect(rightWorkspaceWidthForViewport(560, 1100, true)).toBe(560);
  });

  it('preserves a usable chat width in desktop split mode', () => {
    expect(rightWorkspaceWidthForViewport(560, 1280, true)).toBe(488);
    expect(rightWorkspaceWidthForViewport(560, 1440, true)).toBe(560);
  });
});

describe('useRightWorkspaceStore', () => {
  it('opens flat task, analysis, research, browser, file, and automation views', () => {
    const store = useRightWorkspaceStore.getState();

    store.openBrowser();
    expect(useRightWorkspaceStore.getState()).toMatchObject({ open: true, activeTab: 'browser' });

    store.openAnalysis();
    expect(useRightWorkspaceStore.getState()).toMatchObject({ open: true, activeTab: 'analysis' });

    store.openResearch();
    expect(useRightWorkspaceStore.getState()).toMatchObject({ open: true, activeTab: 'research' });

    store.openFiles();
    expect(useRightWorkspaceStore.getState()).toMatchObject({ open: true, activeTab: 'files' });

    store.openTasks();
    expect(useRightWorkspaceStore.getState()).toMatchObject({ open: true, activeTab: 'tasks' });

    store.openWorkflows();
    expect(useRightWorkspaceStore.getState()).toMatchObject({
      open: true,
      activeTab: 'automation',
      automationView: 'workflows',
    });

    store.openExtract();
    expect(useRightWorkspaceStore.getState()).toMatchObject({
      open: true,
      activeTab: 'automation',
      automationView: 'extract',
    });

    store.close();
  });
});
