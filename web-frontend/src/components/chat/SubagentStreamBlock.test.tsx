import { cleanup, fireEvent, render, screen } from '@testing-library/react';
import { afterEach, beforeEach, describe, expect, it } from 'vitest';
import type { SubagentRunState } from '../../stores/subagentRunStore';
import { useSubagentDetailStore } from '../../stores/subagentDetailStore';
import { useRightWorkspaceStore } from '../../stores/rightWorkspaceStore';
import { SubagentStreamBlock } from './SubagentStreamBlock';

describe('SubagentStreamBlock', () => {
  const run: SubagentRunState = {
    subagentRunId: 'subagent-1',
    runId: 'run-1',
    agent: 'explorer',
    task: '分析项目结构',
    status: 'running',
    startedAt: 1,
    events: [],
  };

  beforeEach(() => {
    useSubagentDetailStore.setState({ selected: null });
    useRightWorkspaceStore.setState({ open: false });
  });

  // vitest globals are off, so testing-library auto-cleanup does not register.
  afterEach(() => cleanup());

  it('renders as a one-line status row without inline tabs', () => {
    render(<SubagentStreamBlock run={run} taskTitle="核证 Agent 实例并发问题" />);

    expect(screen.getByText('Subagent')).toBeTruthy();
    expect(screen.getByText('explorer')).toBeTruthy();
    expect(screen.getByText('核证 Agent 实例并发问题')).toBeTruthy();
    expect(screen.queryByText('提示词 / 任务')).toBeNull();
    expect(screen.queryByText('执行过程')).toBeNull();
    expect(screen.queryByText('结果')).toBeNull();
  });

  it('opens the right-workspace detail on row click', () => {
    render(<SubagentStreamBlock run={run} />);

    fireEvent.click(screen.getByTitle('在右侧边栏查看完整执行过程'));

    expect(useSubagentDetailStore.getState().selected).toEqual({
      runId: 'run-1',
      subagentRunId: 'subagent-1',
    });
    expect(useRightWorkspaceStore.getState().open).toBe(true);
  });
});
// @vitest-environment jsdom
