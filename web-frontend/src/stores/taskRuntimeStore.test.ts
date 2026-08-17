import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

const mocks = vi.hoisted(() => ({
  pauseRun: vi.fn(),
  updateGoal: vi.fn(),
  configureContinuation: vi.fn(),
  resumeRun: vi.fn(),
  retryBlockedTask: vi.fn(),
  resolveRecoveryTask: vi.fn(),
  updateTasks: vi.fn(),
  latestRunForConversation: vi.fn(),
  getPlan: vi.fn(),
  listTodos: vi.fn(),
  listArtifacts: vi.fn(),
  listRecoveryBlockers: vi.fn(),
  getContinuation: vi.fn(),
  listBackgroundCells: vi.fn(),
  getRun: vi.fn(),
  listEvents: vi.fn(),
  listToolExecutions: vi.fn(),
}));

vi.mock('../api/endpoints', () => ({
  taskRuntimeApi: mocks,
  toolExecutionApi: { list: mocks.listToolExecutions },
}));

import type { RunContinuationState, TaskPlan, TaskRun } from '../generated';
import { subagentRunStoreKey, useSubagentRunStore } from './subagentRunStore';
import { useTaskRuntimeStore } from './taskRuntimeStore';
import { useToolExecutionStore } from './toolExecutionStore';

const originalRefresh = useTaskRuntimeStore.getState().refresh;

function run(status: TaskRun['status']): TaskRun {
  return {
    run_id: 'run-1',
    workspace_id: 'workspace-1',
    conversation_id: 'conversation-1',
    root_message_id: 'message-1',
    domain_profile: 'ai_coding',
    status,
    goal: 'analyze project',
    goal_revision: 1,
    goal_sha256: 'goal-sha256',
    plan_id: 'plan-1',
    route: 'formal_plan',
    attended_mode: 'attended',
    created_at: '2026-07-24T00:00:00Z',
    updated_at: '2026-07-24T00:00:00Z',
  };
}

function deferred<T>() {
  let resolve: (value: T) => void = () => undefined;
  const promise = new Promise<T>((next) => {
    resolve = next;
  });
  return { promise, resolve };
}

describe('taskRuntimeStore recovery controls', () => {
  const refresh = vi.fn().mockResolvedValue(undefined);

  beforeEach(() => {
    vi.clearAllMocks();
    mocks.pauseRun.mockResolvedValue({ success: true, run_id: 'run-1' });
    mocks.updateGoal.mockResolvedValue({
      ...run('paused'),
      goal: 'revised goal',
      goal_revision: 2,
      goal_sha256: 'revised-goal-sha256',
    });
    mocks.configureContinuation.mockResolvedValue({
      enabled: true,
      token_budget: 200_000,
      time_budget_seconds: null,
    });
    mocks.resumeRun.mockResolvedValue({ kind: 'resumed', run_id: 'run-1' });
    mocks.retryBlockedTask.mockResolvedValue({ kind: 'recovery_retry_recorded' });
    mocks.resolveRecoveryTask.mockResolvedValue(undefined);
    mocks.updateTasks.mockResolvedValue({ run_id: 'run-1', revision: 4 });
    useTaskRuntimeStore.getState().reset();
    useTaskRuntimeStore.setState({
      activeRun: { run_id: 'run-1', status: 'paused' } as TaskRun,
      error: null,
      refresh,
    });
  });

  afterEach(() => {
    useTaskRuntimeStore.setState({ refresh: originalRefresh });
  });

  it('pauses through the shared runtime API and refreshes the canonical run', async () => {
    await useTaskRuntimeStore.getState().pause('run-1');

    expect(mocks.pauseRun).toHaveBeenCalledWith('run-1');
    expect(refresh).toHaveBeenCalledWith('run-1');
  });

  it('updates continuation budgets and refreshes the canonical projection', async () => {
    await useTaskRuntimeStore.getState().updateContinuationBudgets('run-1', 200_000, null);

    expect(mocks.configureContinuation).toHaveBeenCalledWith('run-1', 200_000, null);
    expect(refresh).toHaveBeenCalledWith('run-1');
  });

  it('updates the Goal with the exact revision and refreshes the canonical projection', async () => {
    await useTaskRuntimeStore.getState().updateGoal('run-1', 1, 'revised goal', 'scope changed');

    expect(mocks.updateGoal).toHaveBeenCalledWith('run-1', 1, 'revised goal', 'scope changed');
    expect(refresh).toHaveBeenCalledWith('run-1');
  });

  it('surfaces a recovery barrier when resume is rejected', async () => {
    mocks.resumeRun.mockRejectedValue(new Error('unresolved recovery barriers'));

    await useTaskRuntimeStore.getState().resumeTaskRun();

    expect(useTaskRuntimeStore.getState().error).toBe('unresolved recovery barriers');
    expect(refresh).not.toHaveBeenCalled();
  });

  it('routes recovery retry through the supervised retry facade', async () => {
    await useTaskRuntimeStore.getState().retryBlockedTask('task-1');

    expect(mocks.retryBlockedTask).toHaveBeenCalledWith('run-1', 'task-1');
    expect(mocks.resolveRecoveryTask).not.toHaveBeenCalled();
    expect(refresh).toHaveBeenCalledWith('run-1');
  });

  it('keeps skip as an explicit recovery decision', async () => {
    await useTaskRuntimeStore.getState().resolveRecoveryTask('task-1', 'skip');

    expect(mocks.resolveRecoveryTask).toHaveBeenCalledWith('run-1', 'task-1', 'skip');
    expect(refresh).toHaveBeenCalledWith('run-1');
  });

  it('surfaces rejected supervised recovery retry without refreshing', async () => {
    mocks.retryBlockedTask.mockRejectedValueOnce(new Error('TaskRuntime admission is closed'));

    await useTaskRuntimeStore.getState().retryBlockedTask('task-1');

    expect(useTaskRuntimeStore.getState().error).toBe('TaskRuntime admission is closed');
    expect(refresh).not.toHaveBeenCalled();
  });

  it('updates a task through one revisioned plan patch', async () => {
    useTaskRuntimeStore.setState({
      plan: { run_id: 'run-1', revision: 3 } as TaskPlan,
    });

    await useTaskRuntimeStore.getState().updateTask('task-1', { title: 'Refined title' });

    expect(mocks.updateTasks).toHaveBeenCalledWith('run-1', {
      base_revision: 3,
      reason: '更新任务：task-1',
      operations: [
        {
          op: 'update',
          task_id: 'task-1',
          patch: {
            title: 'Refined title',
            description: null,
            kind: null,
            agent_role: null,
            depends_on: null,
            files: null,
            allowed_tools: null,
            required_artifacts: null,
            execution_checks: null,
            acceptance_criteria: null,
            max_retries: null,
          },
        },
      ],
    });
    expect(refresh).toHaveBeenCalledWith('run-1');
  });
});

describe('taskRuntimeStore conversation loading', () => {
  beforeEach(() => {
    useTaskRuntimeStore.setState({ refresh: originalRefresh });
    useTaskRuntimeStore.getState().reset();
    vi.clearAllMocks();
    mocks.getPlan.mockResolvedValue(null);
    mocks.listTodos.mockResolvedValue([]);
    mocks.listArtifacts.mockResolvedValue([]);
    mocks.listRecoveryBlockers.mockResolvedValue([]);
    mocks.getContinuation.mockResolvedValue(null);
    mocks.listBackgroundCells.mockResolvedValue([]);
    mocks.listEvents.mockResolvedValue([]);
    mocks.listToolExecutions.mockResolvedValue([]);
    useSubagentRunStore.getState().clear();
    useToolExecutionStore.getState().clear();
  });

  afterEach(() => {
    useTaskRuntimeStore.getState().reset();
  });

  it('starts polling for an active run and stops after loading its terminal snapshot', async () => {
    mocks.latestRunForConversation.mockResolvedValueOnce(run('running'));

    await useTaskRuntimeStore.getState().loadByConversation('conversation-1');

    expect(useTaskRuntimeStore.getState().activeRun?.status).toBe('running');
    expect(useTaskRuntimeStore.getState().pollingInterval).not.toBeNull();

    mocks.latestRunForConversation.mockResolvedValueOnce(run('completed'));
    await useTaskRuntimeStore.getState().loadByConversation('conversation-1');

    expect(useTaskRuntimeStore.getState().activeRun?.status).toBe('completed');
    expect(useTaskRuntimeStore.getState().pollingInterval).toBeNull();
  });

  it('hydrates token and time budgets from the canonical continuation snapshot', async () => {
    const continuation = {
      enabled: true,
      auto_resume_after_restart: false,
      token_budget: 100_000,
      time_budget_seconds: 7_200,
      tokens_used: 12_000,
      time_used_seconds: 90,
      compaction_count: 2,
      next_turn_ordinal: 4,
      active_turn: null,
      last_turn: null,
      pause: null,
      blocker_audit: null,
      provider_retry: null,
      deferred: false,
      deferred_reason: null,
    } satisfies RunContinuationState;
    mocks.latestRunForConversation.mockResolvedValueOnce(run('completed'));
    mocks.getContinuation.mockResolvedValueOnce(continuation);

    await useTaskRuntimeStore.getState().loadByConversation('conversation-1');

    expect(mocks.getContinuation).toHaveBeenCalledWith('run-1');
    expect(useTaskRuntimeStore.getState().continuation).toEqual(continuation);
  });

  it('hydrates the prior Subagent GUI from the run event history', async () => {
    mocks.latestRunForConversation.mockResolvedValueOnce(run('running'));
    mocks.getPlan.mockResolvedValueOnce({
      tasks: [
        {
          id: 'task-1',
          title: 'CLI 层架构分析',
          description: '分析 CLI 层',
          agent_role: 'explorer',
        },
      ],
    } as TaskPlan);
    mocks.listEvents.mockResolvedValueOnce([
      {
        seq: '5',
        run_id: 'run-1',
        task_id: 'task-1',
        step_id: 'run-1:task-1:1:1',
        event_type: 'subagent_assigned',
        payload: { execution_id: 'run-1:task-1:1:1', agent_name: 'explorer' },
        timestamp: '2026-07-30T01:02:03Z',
      },
    ]);

    await useTaskRuntimeStore.getState().loadByConversation('conversation-1');

    expect(mocks.listEvents).toHaveBeenCalledWith('run-1', '0');
    expect(useTaskRuntimeStore.getState().lastSeq).toBe('5');
    expect(
      useSubagentRunStore.getState().runs[subagentRunStoreKey('run-1', 'run-1:task-1:1:1')]
    ).toMatchObject({
      status: 'running',
      agent: 'explorer',
      messageId: 'message-1',
      conversationId: 'conversation-1',
    });
  });

  it('restores persisted Subagent tool execution summaries with the run', async () => {
    mocks.latestRunForConversation.mockResolvedValueOnce(run('completed'));
    mocks.listToolExecutions.mockResolvedValueOnce([
      {
        id: 'tool-detail-1',
        call_id: 'call-1',
        owner: { kind: 'subagent', subagent_run_id: 'run-1:task-1:1:1' },
        conversation_id: 'conversation-1',
        run_id: 'run-1',
        name: 'read_file',
        args_preview: '{"path":"src/main.rs"}',
        status: 'succeeded',
        started_at: 100,
        finished_at: 120,
        duration_ms: 20,
        detail_ref: 'tool-detail-1',
      },
    ]);

    await useTaskRuntimeStore.getState().loadByConversation('conversation-1');

    expect(mocks.listToolExecutions).toHaveBeenCalledWith('conversation-1');
    expect(useToolExecutionStore.getState().tools['tool-detail-1']).toMatchObject({
      name: 'read_file',
      status: 'succeeded',
      owner: { kind: 'subagent', subagent_run_id: 'run-1:task-1:1:1' },
    });
  });

  it('ignores a stale conversation load that resolves after the active conversation', async () => {
    const first = deferred<TaskRun | null>();
    const secondRun = {
      ...run('completed'),
      run_id: 'run-2',
      conversation_id: 'conversation-2',
      root_message_id: 'message-2',
    };
    mocks.latestRunForConversation
      .mockReturnValueOnce(first.promise)
      .mockResolvedValueOnce(secondRun);

    const firstLoad = useTaskRuntimeStore.getState().loadByConversation('conversation-1');
    const secondLoad = useTaskRuntimeStore.getState().loadByConversation('conversation-2');
    await secondLoad;
    first.resolve(run('completed'));
    await firstLoad;

    expect(useTaskRuntimeStore.getState().activeRun?.run_id).toBe('run-2');
  });

  it('ignores an older refresh that resolves after a newer terminal refresh', async () => {
    const staleRun = deferred<TaskRun | null>();
    mocks.getRun.mockReturnValueOnce(staleRun.promise).mockResolvedValueOnce(run('completed'));
    mocks.listEvents.mockResolvedValue([]);

    const staleRefresh = useTaskRuntimeStore.getState().refresh('run-1');
    const terminalRefresh = useTaskRuntimeStore.getState().refresh('run-1');
    await terminalRefresh;
    staleRun.resolve(run('running'));
    await staleRefresh;

    expect(useTaskRuntimeStore.getState().activeRun?.status).toBe('completed');
  });
});
