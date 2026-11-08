// @vitest-environment jsdom
import { act, cleanup, fireEvent, render, screen } from '@testing-library/react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { useContextPaneStore } from '../../stores/contextPaneStore';
import { useConversationStore } from '../../stores/conversationStore';
import { useWorkspaceStore } from '../../stores/workspaceStore';
import {
  subagentRunStoreKey,
  useSubagentRunStore,
  type SubagentRunState,
} from '../../stores/subagentRunStore';

vi.mock('./RightRail', () => ({ RightRail: () => <div>task-run-content</div> }));
vi.mock('../browser/BrowserPanel', () => ({ BrowserPanel: () => <div>browser-content</div> }));
vi.mock('../file-browser/FileBrowser', () => ({ FileBrowser: () => <div>file-content</div> }));
vi.mock('../task/SubagentDetailView', async () => {
  const { useState } = await import('react');
  return {
    SubagentDetailView: ({ run }: { run: SubagentRunState }) => {
      const [draft, setDraft] = useState('');
      return (
        <div>
          <span>{run.subagentRunId}</span>
          <input
            aria-label="Subagent 草稿"
            value={draft}
            onChange={(event) => setDraft(event.target.value)}
          />
        </div>
      );
    },
  };
});

Object.defineProperty(window, 'matchMedia', {
  configurable: true,
  value: () => ({ matches: false }),
});

const [{ ContextPane }, { useUiStore }] = await Promise.all([
  import('./ContextPane'),
  import('../../stores/uiStore'),
]);

describe('ContextPane', () => {
  beforeEach(() => {
    Object.defineProperty(window, 'innerWidth', { configurable: true, value: 1440 });
    useContextPaneStore.setState({ target: null, returnTarget: null, width: 520 });
    useConversationStore.setState({ activeId: 'conversation-1' });
    useWorkspaceStore.setState({ current: null });
    useSubagentRunStore.setState({ runs: {} });
    useUiStore.setState({ leftSidebarOpen: true });
  });

  afterEach(() => cleanup());

  it('does not render the removed global workbench tabs', () => {
    render(<ContextPane />);
    act(() => useContextPaneStore.getState().openTasks());

    expect(screen.getByText('task-run-content')).toBeTruthy();
    expect(document.activeElement).toBe(screen.getByRole('button', { name: '关闭上下文面板' }));
    expect(screen.queryByRole('button', { name: '分析' })).toBeNull();
    expect(screen.queryByRole('button', { name: '研究' })).toBeNull();
    expect(screen.queryByRole('button', { name: '自动化' })).toBeNull();
  });

  it('replaces the contextual target instead of adding tabs', () => {
    render(<ContextPane />);
    act(() => useContextPaneStore.getState().openBrowser());
    expect(screen.getByText('browser-content')).toBeTruthy();

    act(() => useContextPaneStore.getState().openFiles());
    expect(screen.queryByText('browser-content')).toBeNull();
    expect(screen.getByText('file-content')).toBeTruthy();
  });

  it('closes stale context when the active conversation changes', () => {
    render(<ContextPane />);
    act(() => useContextPaneStore.getState().openTasks());
    expect(screen.getByText('task-run-content')).toBeTruthy();

    act(() => useConversationStore.setState({ activeId: 'conversation-2' }));
    expect(screen.queryByText('task-run-content')).toBeNull();
    expect(useContextPaneStore.getState().target).toBeNull();
  });

  it('isolates local composer state when selecting another Subagent attempt', () => {
    const first = subagentRun('subagent-a');
    const second = subagentRun('subagent-b');
    useSubagentRunStore.setState({
      runs: {
        [subagentRunStoreKey(first.runId, first.subagentRunId)]: first,
        [subagentRunStoreKey(second.runId, second.subagentRunId)]: second,
      },
    });
    render(<ContextPane />);

    act(() => useContextPaneStore.getState().openSubagent(first.runId, first.subagentRunId));
    fireEvent.change(screen.getByRole('textbox', { name: 'Subagent 草稿' }), {
      target: { value: '只属于 A 的草稿' },
    });
    expect(screen.getByRole<HTMLInputElement>('textbox', { name: 'Subagent 草稿' }).value).toBe(
      '只属于 A 的草稿'
    );

    act(() => useContextPaneStore.getState().openSubagent(second.runId, second.subagentRunId));
    expect(screen.getByText('subagent-b')).toBeTruthy();
    expect(screen.getByRole<HTMLInputElement>('textbox', { name: 'Subagent 草稿' }).value).toBe('');
  });

  it('supports keyboard resizing on the contextual pane control', () => {
    render(<ContextPane />);
    act(() => useContextPaneStore.getState().openTasks());

    const resizeButton = screen.getByRole('button', { name: '调整上下文面板宽度' });
    fireEvent.keyDown(resizeButton, { key: 'ArrowLeft' });
    expect(useContextPaneStore.getState().width).toBe(544);

    fireEvent.keyDown(resizeButton, { key: 'Home' });
    expect(useContextPaneStore.getState().width).toBe(380);
  });

  it('resizes immediately from the rendered width when the preference exceeds viewport space', () => {
    Object.defineProperty(window, 'innerWidth', { configurable: true, value: 1280 });
    useContextPaneStore.setState({ width: 760 });
    render(<ContextPane />);
    act(() => useContextPaneStore.getState().openTasks());

    // 1280 - 左栏 288 - 主栏最小 520 = 右栏实际 472，再向右缩小 24。
    expect(screen.getByRole('complementary', { name: '任务运行上下文面板' }).style.width).toBe(
      '472px'
    );
    const resizeButton = screen.getByRole('button', { name: '调整上下文面板宽度' });
    fireEvent.keyDown(resizeButton, { key: 'ArrowRight' });
    expect(useContextPaneStore.getState().width).toBe(448);
  });
  it('closes a task target when its existing workspace id gets a different product-data generation', () => {
    const workspace = {
      id: 'workspace-1',
      name: '项目',
      root: '/local/data',
      kind: { type: 'coding' },
      metadata: { tags: [], project_location: { kind: 'local' as const, path: '/local/data' } },
      product_data_generation: 'generation-a',
      created_at: '2026-10-25T00:00:00Z',
      last_active: '2026-10-25T00:00:00Z',
    };
    useWorkspaceStore.setState({ current: workspace });
    render(<ContextPane />);
    act(() => useContextPaneStore.getState().openTasks());
    expect(screen.getByText('task-run-content')).toBeTruthy();
    act(() =>
      useWorkspaceStore.setState({
        current: { ...workspace, product_data_generation: 'generation-b' },
      })
    );
    expect(screen.queryByText('task-run-content')).toBeNull();
  });
});

function subagentRun(subagentRunId: string): SubagentRunState {
  return {
    subagentRunId,
    runId: 'run-1',
    workspaceId: 'workspace-1',
    taskId: 'task-1',
    planRevision: 1,
    attempt: subagentRunId === 'subagent-a' ? 1 : 2,
    agent: 'explorer',
    status: 'running',
    startedAt: 1,
    events: [],
  };
}
