import { beforeEach, describe, expect, it } from 'vitest';
import {
  boundContextPaneWidth,
  contextPaneWidthForViewport,
  useContextPaneStore,
} from './contextPaneStore';

describe('context pane geometry', () => {
  it('keeps the contextual split within readable desktop bounds', () => {
    expect(boundContextPaneWidth(200)).toBe(380);
    expect(boundContextPaneWidth(560)).toBe(560);
    expect(boundContextPaneWidth(1000)).toBe(760);
    expect(contextPaneWidthForViewport(700, 1440, true)).toBe(656);
    expect(contextPaneWidthForViewport(520, 1000, false)).toBe(520);
  });
});

describe('useContextPaneStore', () => {
  beforeEach(() => {
    useContextPaneStore.setState({ target: null, returnTarget: null, width: 520 });
  });

  it('expresses exactly one contextual target', () => {
    const store = useContextPaneStore.getState();
    store.openTasks();
    expect(useContextPaneStore.getState().target).toEqual({ kind: 'tasks' });

    store.openSubagent('run-1', 'subagent-1');
    expect(useContextPaneStore.getState().target).toEqual({
      kind: 'subagent',
      runId: 'run-1',
      subagentRunId: 'subagent-1',
    });
  });

  it('returns from contextual tools to the selected Subagent', () => {
    const store = useContextPaneStore.getState();
    store.openSubagent('run-1', 'subagent-1');
    store.openFiles();
    expect(useContextPaneStore.getState()).toMatchObject({
      target: { kind: 'files' },
      returnTarget: { kind: 'subagent', runId: 'run-1', subagentRunId: 'subagent-1' },
    });

    useContextPaneStore.getState().close();
    expect(useContextPaneStore.getState()).toMatchObject({
      target: { kind: 'subagent', runId: 'run-1', subagentRunId: 'subagent-1' },
      returnTarget: null,
    });
  });

  it('clears all contextual selection on reset', () => {
    const store = useContextPaneStore.getState();
    store.openSubagent('run-1', 'subagent-1');
    store.openBrowser();
    store.reset();
    expect(useContextPaneStore.getState()).toMatchObject({ target: null, returnTarget: null });
  });
});
