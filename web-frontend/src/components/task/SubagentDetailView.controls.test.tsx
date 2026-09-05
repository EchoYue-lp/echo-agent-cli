// @vitest-environment jsdom
import { cleanup, fireEvent, render, screen, waitFor } from '@testing-library/react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import type { SubagentRunState } from '../../stores/subagentRunStore';

const mocks = vi.hoisted(() => ({
  sendSubagentMessage: vi.fn(),
  queueSubagentGuidance: vi.fn(),
  interruptSubagent: vi.fn(),
}));

vi.mock('../../api/endpoints', () => ({
  taskRuntimeApi: {
    sendSubagentMessage: mocks.sendSubagentMessage,
    queueSubagentGuidance: mocks.queueSubagentGuidance,
    interruptSubagent: mocks.interruptSubagent,
  },
}));

import { SubagentDetailView } from './SubagentDetailView';

function run(status: SubagentRunState['status']): SubagentRunState {
  return {
    subagentRunId: 'run-1:task-1:3:2:claim-1',
    runId: 'run-1',
    workspaceId: 'workspace-1',
    taskId: 'task-1',
    planRevision: 3,
    attempt: 2,
    agent: 'explorer',
    status,
    startedAt: 1,
    events: [],
  };
}

describe('SubagentDetailView controls', () => {
  beforeEach(() => {
    vi.clearAllMocks();
    mocks.sendSubagentMessage.mockResolvedValue({ status: 'accepted' });
    mocks.queueSubagentGuidance.mockResolvedValue({ status: 'pending' });
    mocks.interruptSubagent.mockResolvedValue({ status: 'accepted' });
  });

  afterEach(cleanup);

  it('sends running input to the exact active attempt', async () => {
    render(<SubagentDetailView run={run('running')} onBack={vi.fn()} />);

    const composer = screen.getByRole('textbox', { name: 'Subagent 消息' });
    expect(document.activeElement).toBe(composer);
    fireEvent.change(composer, {
      target: { value: '检查当前调用链' },
    });
    fireEvent.click(screen.getByRole('button', { name: '发送 Subagent 消息' }));

    await waitFor(() => expect(mocks.sendSubagentMessage).toHaveBeenCalledOnce());
    expect(mocks.sendSubagentMessage).toHaveBeenCalledWith(
      'workspace-1',
      expect.objectContaining({
        run_id: 'run-1',
        task_id: 'task-1',
        execution_id: 'run-1:task-1:3:2:claim-1',
        plan_revision: 3,
        attempt: 2,
      }),
      '检查当前调用链'
    );
    expect(mocks.queueSubagentGuidance).not.toHaveBeenCalled();
  });

  it('queues settled input for the next attempt', async () => {
    render(<SubagentDetailView run={run('failed')} onBack={vi.fn()} />);

    fireEvent.change(screen.getByRole('textbox', { name: 'Subagent 后续任务' }), {
      target: { value: '基于失败证据继续' },
    });
    fireEvent.click(screen.getByRole('button', { name: '发送 Subagent 后续任务' }));

    await waitFor(() => expect(mocks.queueSubagentGuidance).toHaveBeenCalledOnce());
    expect(mocks.queueSubagentGuidance).toHaveBeenCalledWith(
      'workspace-1',
      expect.objectContaining({
        run_id: 'run-1',
        task_id: 'task-1',
        execution_id: 'pending:run-1:task-1:3:3',
        plan_revision: 3,
        attempt: 3,
      }),
      '基于失败证据继续'
    );
    expect(mocks.sendSubagentMessage).not.toHaveBeenCalled();
  });

  it('interrupts only the selected running attempt', async () => {
    render(<SubagentDetailView run={run('running')} onBack={vi.fn()} />);

    fireEvent.click(screen.getByRole('button', { name: '中断 Subagent' }));

    await waitFor(() => expect(mocks.interruptSubagent).toHaveBeenCalledOnce());
    expect(mocks.interruptSubagent).toHaveBeenCalledWith(
      'workspace-1',
      expect.objectContaining({ execution_id: 'run-1:task-1:3:2:claim-1', attempt: 2 })
    );
  });
});
