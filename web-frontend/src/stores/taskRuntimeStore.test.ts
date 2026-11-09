import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

const mocks = vi.hoisted(() => ({
  pauseRun: vi.fn(),
  cancelRun: vi.fn(),
  listRuns: vi.fn(),
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
  getCompletionGate: vi.fn(),
  skipGoalRequirement: vi.fn(),
  getRun: vi.fn(),
  listEvents: vi.fn(),
  listToolExecutions: vi.fn(),
}));

vi.mock('../api/endpoints', () => ({
  taskRuntimeApi: mocks,
  toolExecutionApi: { list: mocks.listToolExecutions },
}));

import type { PlanRevision, RunContinuationState, TaskRun, RuntimeTaskEvent } from '../generated';
import type { ProductDataScope, TaskEventPage, TaskRunPage } from '../api/endpoints';
import { subagentRunStoreKey, useSubagentRunStore } from './subagentRunStore';
import { useTaskRuntimeStore, taskRuntimePresentation } from './taskRuntimeStore';
import { useToolExecutionStore } from './toolExecutionStore';

const originalRefresh = useTaskRuntimeStore.getState().refresh;

function run(status: TaskRun['status']): TaskRun {
  return {
    run_id: 'run-1',
    workspace_id: 'workspace-1',
    conversation_id: 'conversation-1',
    root_message_id: 'message-1',
    domain_profile: 'ai_coding',
    domain_binding: null,
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
  let reject: (reason: Error) => void = () => undefined;
  const promise = new Promise<T>((next, fail) => {
    resolve = next;
    reject = fail;
  });
  return { promise, resolve, reject };
}

function eventPage(events: RuntimeTaskEvent[] = [], after = '0', hasMore = false): TaskEventPage {
  return { events, next_after_sequence: events.at(-1)?.seq ?? after, has_more: hasMore };
}

function note(seq: string, runId = 'run-1'): RuntimeTaskEvent {
  return {
    seq,
    run_id: runId,
    task_id: null,
    step_id: null,
    event_type: 'note',
    payload: { message: `记录 ${seq} 中文 🧭` },
    timestamp: '2026-07-30T01:00:00Z',
  };
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
    mocks.retryBlockedTask.mockResolvedValue({
      kind: 'recovery_retry_recorded',
      run_id: 'run-1',
      task_id: 'task-1',
      next_attempt: 2,
    });
    mocks.resolveRecoveryTask.mockResolvedValue(undefined);
    mocks.skipGoalRequirement.mockResolvedValue({
      run_id: 'run-1',
      goal_revision: 1,
      plan_revision: 1,
      ready: true,
      requirements: [],
      blockers: [],
    });
    mocks.updateTasks.mockResolvedValue({ run_id: 'run-1', revision: 4 });
    useTaskRuntimeStore.getState().reset();
    useTaskRuntimeStore.setState({
      activeRun: run('paused'),
      taskScope: { workspaceId: 'workspace-1', workspaceGeneration: 'generation-1' },
      error: null,
      refresh,
    });
  });

  afterEach(() => {
    useTaskRuntimeStore.setState({ refresh: originalRefresh });
  });

  it('pauses through the shared runtime API and refreshes the canonical run', async () => {
    await useTaskRuntimeStore.getState().pause('run-1');

    expect(mocks.pauseRun).toHaveBeenCalledWith('workspace-1', 'run-1', 'generation-1');
    expect(refresh).toHaveBeenCalledWith('workspace-1', 'run-1');
  });

  it('preserves a typed no-op receipt without claiming a new pause or terminal state', async () => {
    mocks.pauseRun.mockResolvedValue({ success: false, run_id: 'run-1' });
    useTaskRuntimeStore.setState({ activeRun: run('completed') });

    const receipt = await useTaskRuntimeStore.getState().pause('run-1');

    expect(receipt).toEqual({ success: false, run_id: 'run-1' });
    expect(useTaskRuntimeStore.getState().activeRun?.status).toBe('completed');
    expect(refresh).toHaveBeenCalledWith('workspace-1', 'run-1');
  });

  it('updates continuation budgets and refreshes the canonical projection', async () => {
    await useTaskRuntimeStore.getState().updateContinuationBudgets('run-1', 200_000, null);

    expect(mocks.configureContinuation).toHaveBeenCalledWith(
      'workspace-1',
      'run-1',
      200_000,
      null,
      'generation-1'
    );
    expect(refresh).toHaveBeenCalledWith('workspace-1', 'run-1');
  });

  it('updates the Goal with the exact revision and refreshes the canonical projection', async () => {
    await expect(
      useTaskRuntimeStore.getState().updateGoal('run-1', 1, 'revised goal', 'scope changed')
    ).resolves.toBe(true);

    expect(mocks.updateGoal).toHaveBeenCalledWith(
      'workspace-1',
      'run-1',
      1,
      'revised goal',
      'scope changed',
      'generation-1'
    );
    expect(refresh).toHaveBeenCalledWith('workspace-1', 'run-1');
  });

  it('rejects Goal update failures instead of reporting a successful revision', async () => {
    mocks.updateGoal.mockRejectedValue(new Error('stale Goal revision'));

    await expect(
      useTaskRuntimeStore.getState().updateGoal('run-1', 1, 'revised goal', 'scope changed')
    ).rejects.toThrow('stale Goal revision');
    expect(useTaskRuntimeStore.getState().error).toBe('stale Goal revision');
  });

  it('rejects cancellation failures so Goal deletion cannot be shown as complete', async () => {
    mocks.cancelRun.mockRejectedValue(new Error('cancel rejected'));

    await expect(useTaskRuntimeStore.getState().cancel('run-1')).rejects.toThrow('cancel rejected');
    expect(useTaskRuntimeStore.getState().error).toBe('cancel rejected');
  });

  it('surfaces a recovery barrier when resume is rejected', async () => {
    mocks.resumeRun.mockRejectedValue(new Error('unresolved recovery barriers'));

    await useTaskRuntimeStore.getState().resumeTaskRun();

    expect(useTaskRuntimeStore.getState().error).toBe('unresolved recovery barriers');
    expect(refresh).not.toHaveBeenCalled();
  });

  it('routes recovery retry through the supervised retry facade', async () => {
    await useTaskRuntimeStore.getState().retryBlockedTask('task-1');

    expect(mocks.retryBlockedTask).toHaveBeenCalledWith(
      'workspace-1',
      'run-1',
      'task-1',
      'generation-1'
    );
    expect(mocks.resolveRecoveryTask).not.toHaveBeenCalled();
    expect(refresh).toHaveBeenCalledWith('workspace-1', 'run-1');
  });

  it('keeps skip as an explicit recovery decision', async () => {
    await useTaskRuntimeStore.getState().resolveRecoveryTask('task-1', 'skip');

    expect(mocks.resolveRecoveryTask).toHaveBeenCalledWith(
      'workspace-1',
      'run-1',
      'task-1',
      'skip',
      'generation-1'
    );
    expect(refresh).toHaveBeenCalledWith('workspace-1', 'run-1');
  });

  it('binds a requirement skip to the current Goal revision and refreshes the gate', async () => {
    useTaskRuntimeStore.setState({
      activeRun: run('paused'),
    });

    await useTaskRuntimeStore
      .getState()
      .skipGoalRequirement('req:manual-review', '用户确认该项不适用');

    expect(mocks.skipGoalRequirement).toHaveBeenCalledWith(
      'workspace-1',
      'run-1',
      1,
      'req:manual-review',
      '用户确认该项不适用',
      'generation-1'
    );
    expect(useTaskRuntimeStore.getState().completionGate?.ready).toBe(true);
    expect(refresh).toHaveBeenCalledWith('workspace-1', 'run-1');
  });

  it('surfaces rejected supervised recovery retry without refreshing', async () => {
    mocks.retryBlockedTask.mockRejectedValueOnce(new Error('TaskRuntime admission is closed'));

    await useTaskRuntimeStore.getState().retryBlockedTask('task-1');

    expect(useTaskRuntimeStore.getState().error).toBe('TaskRuntime admission is closed');
    expect(refresh).not.toHaveBeenCalled();
  });

  it('updates a task through one revisioned plan patch', async () => {
    useTaskRuntimeStore.setState({
      plan: { run_id: 'run-1', revision: 3 } as PlanRevision,
    });

    await useTaskRuntimeStore.getState().updateTask('task-1', { title: 'Refined title' });

    expect(mocks.updateTasks).toHaveBeenCalledWith(
      'workspace-1',
      'run-1',
      {
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
      },
      'generation-1'
    );
    expect(refresh).toHaveBeenCalledWith('workspace-1', 'run-1');
  });
});

describe('taskRuntimeStore conversation loading', () => {
  beforeEach(() => {
    useTaskRuntimeStore.setState({ refresh: originalRefresh });
    useTaskRuntimeStore.getState().reset();
    vi.resetAllMocks();
    mocks.pauseRun.mockResolvedValue({ success: true, run_id: 'run-1' });
    mocks.getPlan.mockResolvedValue(null);
    mocks.listTodos.mockResolvedValue([]);
    mocks.listArtifacts.mockResolvedValue([]);
    mocks.listRecoveryBlockers.mockResolvedValue([]);
    mocks.getContinuation.mockResolvedValue(null);
    mocks.listBackgroundCells.mockResolvedValue([]);
    mocks.getCompletionGate.mockResolvedValue({
      run_id: 'run-1',
      goal_revision: 1,
      plan_revision: 0,
      ready: false,
      requirements: [],
      blockers: [],
    });
    mocks.listEvents.mockImplementation((_workspace: string, _run: string, after = '0') =>
      Promise.resolve(eventPage([], after))
    );
    mocks.listToolExecutions.mockResolvedValue([]);
    useSubagentRunStore.getState().clear();
    useToolExecutionStore.getState().clear();
  });

  afterEach(() => {
    useTaskRuntimeStore.getState().reset();
  });

  it('starts polling for an active run and stops after loading its terminal snapshot', async () => {
    mocks.latestRunForConversation.mockResolvedValueOnce(run('running'));

    await useTaskRuntimeStore.getState().loadByConversation('workspace-1', 'conversation-1');

    expect(useTaskRuntimeStore.getState().activeRun?.status).toBe('running');
    expect(useTaskRuntimeStore.getState().pollingInterval).not.toBeNull();

    mocks.latestRunForConversation.mockResolvedValueOnce(run('completed'));
    await useTaskRuntimeStore.getState().loadByConversation('workspace-1', 'conversation-1');

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

    await useTaskRuntimeStore.getState().loadByConversation('workspace-1', 'conversation-1');

    expect(mocks.getContinuation).toHaveBeenCalledWith('workspace-1', 'run-1', undefined);
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
    } as PlanRevision);
    mocks.listEvents.mockResolvedValueOnce(
      eventPage([
        ...[1, 2, 3, 4].map((seq) => note(String(seq))),
        {
          seq: '5',
          run_id: 'run-1',
          task_id: 'task-1',
          step_id: 'run-1:task-1:1:1',
          event_type: 'subagent_assigned',
          payload: { execution_id: 'run-1:task-1:1:1', agent_name: 'explorer' },
          timestamp: '2026-07-30T01:02:03Z',
        },
      ])
    );

    await useTaskRuntimeStore.getState().loadByConversation('workspace-1', 'conversation-1');

    expect(mocks.listEvents).toHaveBeenCalledWith('workspace-1', 'run-1', '0', undefined);
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
        workspace_id: 'workspace-1',
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

    await useTaskRuntimeStore.getState().loadByConversation('workspace-1', 'conversation-1');

    expect(mocks.listToolExecutions).toHaveBeenCalledWith('workspace-1', 'conversation-1');
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

    const firstLoad = useTaskRuntimeStore
      .getState()
      .loadByConversation('workspace-1', 'conversation-1');
    const secondLoad = useTaskRuntimeStore
      .getState()
      .loadByConversation('workspace-1', 'conversation-2');
    await secondLoad;
    first.resolve(run('completed'));
    await firstLoad;

    expect(useTaskRuntimeStore.getState().activeRun?.run_id).toBe('run-2');
  });

  it('ignores an older refresh that resolves after a newer terminal refresh', async () => {
    const staleRun = deferred<TaskRun | null>();
    mocks.getRun.mockReturnValueOnce(staleRun.promise).mockResolvedValueOnce(run('completed'));
    mocks.listEvents.mockImplementation((_workspace: string, _run: string, after = '0') =>
      Promise.resolve(eventPage([], after))
    );

    const staleRefresh = useTaskRuntimeStore.getState().refresh('workspace-1', 'run-1');
    const terminalRefresh = useTaskRuntimeStore.getState().refresh('workspace-1', 'run-1');
    await terminalRefresh;
    staleRun.resolve(run('running'));
    await staleRefresh;

    expect(useTaskRuntimeStore.getState().activeRun?.status).toBe('completed');
  });
  const scope: ProductDataScope = {
    workspaceId: 'workspace-1',
    workspaceGeneration: 'generation-1',
  };

  it.each(['conversation', 'history'] as const)(
    'keeps the canonical Goal refresh after an older initial %s snapshot resolves',
    async (view) => {
      const original = { ...run('paused'), domain_profile: 'general' as const };
      const live = { ...original, run_id: 'run-live' };
      const plan: PlanRevision = {
        plan_id: 'plan-1',
        run_id: original.run_id,
        revision: 1,
        domain_profile: 'general',
        domain_binding: null,
        goal_revision: 1,
        goal_sha256: original.goal_sha256,
        assumptions: [],
        risks: [],
        execution_mode: 'parallel',
        tasks: [],
      };
      const oldGate = {
        run_id: original.run_id,
        goal_revision: 1,
        plan_revision: 1,
        ready: true,
        requirements: [],
        blockers: [],
      };
      mocks.getPlan.mockImplementation((_workspace: string, runId: string) =>
        Promise.resolve({ ...plan, run_id: runId })
      );
      mocks.getCompletionGate.mockImplementation((_workspace: string, runId: string) =>
        Promise.resolve({ ...oldGate, run_id: runId })
      );
      mocks.latestRunForConversation.mockResolvedValue(view === 'history' ? live : original);
      useTaskRuntimeStore.getState().bindScope(scope);
      await useTaskRuntimeStore.getState().loadByConversation('workspace-1', 'conversation-1');
      if (view === 'history') {
        mocks.getRun.mockResolvedValue(original);
        await useTaskRuntimeStore.getState().loadByRun(scope, original.run_id);
      }

      const oldPlan = deferred<PlanRevision>();
      const readsBefore = mocks.getPlan.mock.calls.length;
      mocks.getPlan.mockReturnValueOnce(oldPlan.promise);
      const oldLoad =
        view === 'history'
          ? useTaskRuntimeStore.getState().loadByRun(scope, original.run_id)
          : useTaskRuntimeStore.getState().loadByConversation('workspace-1', 'conversation-1');
      await vi.waitFor(() => expect(mocks.getPlan).toHaveBeenCalledTimes(readsBefore + 1));
      // 除 plan 外的旧快照已返回；后面只释放这一个旧请求，不手写更新后的store状态。
      expect(mocks.getCompletionGate).toHaveBeenLastCalledWith(
        'workspace-1',
        original.run_id,
        'generation-1'
      );
      const revised = {
        ...original,
        goal: '修订后的目标',
        goal_revision: 2,
        goal_sha256: 'revised-goal-sha256',
      };
      const currentGate = {
        ...oldGate,
        goal_revision: 2,
        ready: false,
        blockers: [
          { code: 'plan_goal_mismatch', task_id: null, requirement_id: null, detail: '旧计划' },
        ],
      };
      mocks.updateGoal.mockResolvedValue(revised);
      mocks.getRun.mockResolvedValue(revised);
      mocks.getCompletionGate.mockResolvedValue(currentGate);
      mocks.listEvents.mockResolvedValue(eventPage([note('1')], '0'));

      await expect(
        useTaskRuntimeStore.getState().updateGoal(original.run_id, 1, revised.goal, '用户修订目标')
      ).resolves.toBe(true);
      expect(mocks.updateGoal).toHaveBeenLastCalledWith(
        'workspace-1',
        original.run_id,
        1,
        revised.goal,
        '用户修订目标',
        'generation-1'
      );
      expect(taskRuntimePresentation(useTaskRuntimeStore.getState()).activeRun).toEqual(revised);
      oldPlan.resolve(plan);
      await oldLoad;

      const current = taskRuntimePresentation(useTaskRuntimeStore.getState());
      expect(current.activeRun).toEqual(revised);
      expect(current.completionGate).toEqual(currentGate);
      expect(current.lastSeq).toBe('1');
      expect(current.events.map((event) => event.seq)).toEqual(['1']);
      expect(current.error).toBeNull();
      expect(useTaskRuntimeStore.getState().loadingRun).toBe(false);
      expect(useTaskRuntimeStore.getState().pollingInterval).not.toBeNull();
      expect(useTaskRuntimeStore.getState().activeRun?.run_id).toBe(
        view === 'history' ? live.run_id : original.run_id
      );
    }
  );

  it.each(['conversation', 'history'] as const)(
    'settles a still-current initial %s snapshot after a failed refresh',
    async (view) => {
      const original = run('paused');
      mocks.latestRunForConversation.mockResolvedValue(original);
      mocks.getRun.mockResolvedValue(original);
      useTaskRuntimeStore.getState().bindScope(scope);
      await useTaskRuntimeStore.getState().loadByConversation('workspace-1', 'conversation-1');
      if (view === 'history') {
        useTaskRuntimeStore.setState({ activeRun: { ...original, run_id: 'live-other' } });
        await useTaskRuntimeStore.getState().loadByRun(scope, original.run_id);
      }
      const pendingPlan = deferred<PlanRevision | null>();
      const readsBefore = mocks.getPlan.mock.calls.length;
      mocks.getPlan.mockReturnValueOnce(pendingPlan.promise);
      const initial =
        view === 'history'
          ? useTaskRuntimeStore.getState().loadByRun(scope, original.run_id)
          : useTaskRuntimeStore.getState().loadByConversation('workspace-1', 'conversation-1');
      await vi.waitFor(() => expect(mocks.getPlan).toHaveBeenCalledTimes(readsBefore + 1));
      mocks.getRun.mockRejectedValueOnce(new Error('受控刷新失败'));
      await useTaskRuntimeStore.getState().refresh('workspace-1', original.run_id);
      expect(taskRuntimePresentation(useTaskRuntimeStore.getState()).error).toBe('受控刷新失败');

      pendingPlan.resolve(null);
      await initial;
      expect(taskRuntimePresentation(useTaskRuntimeStore.getState()).activeRun).toEqual(original);
      expect(taskRuntimePresentation(useTaskRuntimeStore.getState()).error).toBeNull();
      expect(useTaskRuntimeStore.getState().loadingRun).toBe(false);
      expect(useTaskRuntimeStore.getState().pollingInterval).not.toBeNull();
    }
  );

  it.each(['conversation', 'history'] as const)(
    'reports an initial %s snapshot failure without leaving a loading owner',
    async (view) => {
      mocks.latestRunForConversation.mockResolvedValue(run('paused'));
      mocks.getRun.mockResolvedValue(run('paused'));
      mocks.getPlan.mockRejectedValueOnce(new Error('受控初始载入失败'));
      useTaskRuntimeStore.getState().bindScope(scope);
      if (view === 'history') {
        await useTaskRuntimeStore.getState().loadByRun(scope, 'run-1');
        expect(useTaskRuntimeStore.getState().historyError).toBe('受控初始载入失败');
      } else {
        await useTaskRuntimeStore.getState().loadByConversation('workspace-1', 'conversation-1');
        expect(useTaskRuntimeStore.getState().error).toBe('受控初始载入失败');
      }
      expect(useTaskRuntimeStore.getState().loadingRun).toBe(false);
      expect(useTaskRuntimeStore.getState().pollingInterval).toBeNull();
    }
  );

  it.each(['conversation', 'history', 'shared', 'shared-unloaded'] as const)(
    'retires r1 initial data after an accepted Goal CAS even when refresh fails (%s)',
    async (view) => {
      const original = run('paused');
      const revised = {
        ...original,
        goal: '已持久提交的 Goal 2',
        goal_revision: 2,
        goal_sha256: 'goal-2',
      };
      const plan: PlanRevision = {
        plan_id: 'plan-1',
        run_id: original.run_id,
        revision: 1,
        domain_profile: 'general',
        domain_binding: null,
        goal_revision: 1,
        goal_sha256: original.goal_sha256,
        assumptions: [],
        risks: [],
        execution_mode: 'parallel',
        tasks: [],
      };
      mocks.getPlan.mockResolvedValue(plan);
      mocks.getRun.mockResolvedValue(original);
      mocks.latestRunForConversation.mockResolvedValue(
        view === 'history' ? { ...original, run_id: 'run-live' } : original
      );
      useTaskRuntimeStore.getState().bindScope(scope);
      await useTaskRuntimeStore.getState().loadByConversation('workspace-1', 'conversation-1');
      if (view === 'history' || view === 'shared')
        await useTaskRuntimeStore.getState().loadByRun(scope, original.run_id);
      const oldPlan = deferred<PlanRevision>();
      const readsBefore = mocks.getPlan.mock.calls.length;
      mocks.getPlan.mockReturnValueOnce(oldPlan.promise);
      const pending =
        view === 'conversation'
          ? useTaskRuntimeStore.getState().loadByConversation('workspace-1', 'conversation-1')
          : useTaskRuntimeStore.getState().loadByRun(scope, original.run_id);
      await vi.waitFor(() => expect(mocks.getPlan).toHaveBeenCalledTimes(readsBefore + 1));

      mocks.updateGoal.mockResolvedValue(revised);
      mocks.getRun.mockRejectedValueOnce(new Error('受控 CAS 后刷新失败'));
      await useTaskRuntimeStore.getState().updateGoal(original.run_id, 1, revised.goal, '明确修订');
      const accepted = taskRuntimePresentation(useTaskRuntimeStore.getState());
      const loadingAfterAccepted = useTaskRuntimeStore.getState().loadingRun;
      oldPlan.resolve(plan);
      await pending;

      expect(accepted.activeRun).toEqual(revised);
      expect(accepted.error).toBe('受控 CAS 后刷新失败');
      expect(loadingAfterAccepted).toBe(false);
      const current = taskRuntimePresentation(useTaskRuntimeStore.getState());
      expect(current.activeRun).toEqual(revised);
      expect(current.error).toBe('受控 CAS 后刷新失败');
      expect(current.plan?.goal_revision).toBe(1);
      expect(current.completionGate?.ready).toBe(false);
      expect(useTaskRuntimeStore.getState().pollingInterval).not.toBeNull();
      mocks.getRun.mockResolvedValue(revised);
      mocks.getCompletionGate.mockResolvedValue({
        run_id: original.run_id,
        goal_revision: 2,
        plan_revision: 1,
        ready: false,
        requirements: [],
        blockers: [],
      });
      if (view === 'history' || view === 'shared')
        await useTaskRuntimeStore.getState().refreshHistory();
      else await useTaskRuntimeStore.getState().refresh('workspace-1', original.run_id);
      expect(taskRuntimePresentation(useTaskRuntimeStore.getState()).error).toBeNull();
      if (view === 'shared' || view === 'shared-unloaded') {
        expect(useTaskRuntimeStore.getState().error).toBeNull();
        expect(useTaskRuntimeStore.getState().historySnapshot?.error).toBeNull();
      }
      if (view === 'shared' || view === 'shared-unloaded') {
        mocks.latestRunForConversation.mockResolvedValue({ ...original, run_id: 'run-next' });
        await useTaskRuntimeStore.getState().loadByConversation('workspace-1', 'conversation-1');
        expect(taskRuntimePresentation(useTaskRuntimeStore.getState()).activeRun).toEqual(revised);
      }
    }
  );

  it('keeps a selected chat run canonical while retaining its independent history cursor', async () => {
    const original = run('paused');
    const revised = { ...original, goal: '修订 Goal 2', goal_revision: 2, goal_sha256: 'goal-2' };
    mocks.latestRunForConversation.mockResolvedValue(original);
    mocks.getRun.mockResolvedValue(original);
    useTaskRuntimeStore.getState().bindScope(scope);
    mocks.listEvents.mockResolvedValueOnce(eventPage([note('1'), note('2'), note('3')]));
    await useTaskRuntimeStore.getState().loadByConversation('workspace-1', 'conversation-1');
    mocks.listEvents.mockResolvedValueOnce(eventPage([note('1')], '0', true));
    await useTaskRuntimeStore.getState().loadByRun(scope, original.run_id);
    const oldPlan = deferred<PlanRevision | null>();
    mocks.getPlan.mockReturnValueOnce(oldPlan.promise);
    mocks.listEvents.mockResolvedValueOnce(eventPage([note('1')], '0', true));
    const readsBefore = mocks.getPlan.mock.calls.length;
    const pending = useTaskRuntimeStore.getState().loadByRun(scope, original.run_id);
    await vi.waitFor(() => expect(mocks.getPlan).toHaveBeenCalledTimes(readsBefore + 1));
    mocks.updateGoal.mockResolvedValue(revised);
    mocks.getRun.mockResolvedValue(revised);
    const gate = {
      run_id: original.run_id,
      goal_revision: 2,
      plan_revision: 0,
      ready: false,
      requirements: [],
      blockers: [],
    };
    mocks.getCompletionGate.mockResolvedValue(gate);
    mocks.listEvents.mockResolvedValueOnce(eventPage([note('4')], '3'));
    await useTaskRuntimeStore
      .getState()
      .updateGoal(original.run_id, 1, revised.goal, '保持同一目标');
    const loadingAfterAccepted = useTaskRuntimeStore.getState().loadingRun;
    oldPlan.resolve(null);
    await pending;

    expect(useTaskRuntimeStore.getState().historySnapshot?.activeRun).toEqual(revised);
    expect(useTaskRuntimeStore.getState().historySnapshot?.completionGate).toEqual(gate);
    expect(useTaskRuntimeStore.getState().historySnapshot?.lastSeq).toBe('1');
    expect(useTaskRuntimeStore.getState().historySnapshot?.hasMoreEvents).toBe(true);
    expect(useTaskRuntimeStore.getState().lastSeq).toBe('4');
    expect(loadingAfterAccepted).toBe(false);
    mocks.latestRunForConversation.mockResolvedValue({ ...original, run_id: 'run-next' });
    await useTaskRuntimeStore.getState().loadByConversation('workspace-1', 'conversation-1');
    expect(taskRuntimePresentation(useTaskRuntimeStore.getState()).activeRun).toEqual(revised);
    expect(taskRuntimePresentation(useTaskRuntimeStore.getState()).completionGate).toEqual(gate);
  });

  it.each(['conversation', 'history'] as const)(
    'does not downgrade a newer canonical %s projection when an older Goal receipt arrives',
    async (view) => {
      const original = run('paused');
      const receipt = deferred<TaskRun>();
      const revised = { ...original, goal_revision: 2, goal_sha256: 'goal-2', goal: 'Goal 2' };
      const newer = { ...original, goal_revision: 3, goal_sha256: 'goal-3', goal: 'Goal 3' };
      const plan: PlanRevision = {
        plan_id: 'plan-1',
        run_id: original.run_id,
        revision: 8,
        domain_profile: 'general',
        domain_binding: null,
        goal_revision: 3,
        goal_sha256: newer.goal_sha256,
        assumptions: [],
        risks: [],
        execution_mode: 'parallel',
        tasks: [],
      };
      mocks.latestRunForConversation.mockResolvedValue(original);
      mocks.getRun.mockResolvedValue(original);
      useTaskRuntimeStore.getState().bindScope(scope);
      await useTaskRuntimeStore.getState().loadByConversation('workspace-1', 'conversation-1');
      await useTaskRuntimeStore.getState().loadByRun(scope, original.run_id);
      mocks.updateGoal.mockReturnValueOnce(receipt.promise);
      const action = useTaskRuntimeStore
        .getState()
        .updateGoal(original.run_id, 1, revised.goal, '较早提交');
      mocks.getRun.mockResolvedValue(newer);
      mocks.getPlan.mockResolvedValue(plan);
      mocks.getCompletionGate.mockResolvedValue({
        run_id: original.run_id,
        goal_revision: 3,
        plan_revision: 8,
        ready: false,
        requirements: [],
        blockers: [],
      });
      if (view === 'history') await useTaskRuntimeStore.getState().refreshHistory();
      else await useTaskRuntimeStore.getState().refresh('workspace-1', original.run_id);
      const observed: Array<{ goal: number; plan: number }> = [];
      const unsubscribe = useTaskRuntimeStore.subscribe((state) => {
        const current = view === 'history' ? state.historySnapshot : state;
        observed.push({
          goal: current?.activeRun?.goal_revision ?? 0,
          plan: current?.plan?.revision ?? 0,
        });
      });
      try {
        receipt.resolve(revised);
        await action;
      } finally {
        unsubscribe();
      }
      expect(observed.length).toBeGreaterThan(0);
      expect(observed.every((item) => item.goal >= 3 && item.plan >= 8)).toBe(true);
      const current =
        view === 'history'
          ? useTaskRuntimeStore.getState().historySnapshot
          : useTaskRuntimeStore.getState();
      expect(current?.activeRun).toEqual(newer);
      expect(current?.plan).toEqual(plan);
    }
  );

  it.each(['success', 'failure'] as const)(
    'keeps a captured history visit %s out of a newer A-B-A visit even when A is live chat',
    async (outcome) => {
      mocks.latestRunForConversation.mockResolvedValue(run('paused'));
      mocks.getRun.mockImplementation((_workspace: string, runId: string) =>
        Promise.resolve({ ...run('paused'), run_id: runId })
      );
      useTaskRuntimeStore.getState().bindScope(scope);
      await useTaskRuntimeStore.getState().loadByConversation(scope.workspaceId, 'conversation-1');
      await useTaskRuntimeStore.getState().loadByRun(scope, 'run-1');
      const first = deferred<{ success: boolean; run_id: string }>();
      const next = deferred<{ success: boolean; run_id: string }>();
      mocks.pauseRun.mockReturnValueOnce(first.promise).mockReturnValueOnce(next.promise);
      const oldAction = useTaskRuntimeStore.getState().pause({
        workspace_id: scope.workspaceId,
        workspace_generation: scope.workspaceGeneration,
        run_id: 'run-1',
      });
      await useTaskRuntimeStore.getState().loadByRun(scope, 'run-b');
      await useTaskRuntimeStore.getState().loadByRun(scope, 'run-1');
      let newSettled = false;
      const newAction = useTaskRuntimeStore
        .getState()
        .pause('run-1')
        .finally(() => {
          newSettled = true;
        });
      const calls = mocks.getRun.mock.calls.length;
      if (outcome === 'success') first.resolve({ success: true, run_id: 'run-1' });
      else first.reject(new Error('OLD_VISIT_FAILURE'));
      await oldAction;
      expect(useTaskRuntimeStore.getState().error).toBeNull();
      expect(useTaskRuntimeStore.getState().historySnapshot?.error).toBeNull();
      expect(mocks.getRun).toHaveBeenCalledTimes(calls);
      expect(newSettled).toBe(false);
      expect(taskRuntimePresentation(useTaskRuntimeStore.getState()).activeRun?.run_id).toBe(
        'run-1'
      );
      next.resolve({ success: true, run_id: 'run-1' });
      await newAction;
      expect(newSettled).toBe(true);
      expect(useTaskRuntimeStore.getState().error).toBeNull();
    }
  );

  it('keeps an accepted canonical Goal receipt after a same-run history revisit', async () => {
    const original = run('paused');
    const revised = {
      ...original,
      goal: '用户规范新目标',
      goal_revision: 2,
      goal_sha256: 'goal-2',
    };
    mocks.latestRunForConversation.mockResolvedValue(original);
    mocks.getRun.mockImplementation((_workspace: string, runId: string) =>
      Promise.resolve({ ...original, run_id: runId })
    );
    useTaskRuntimeStore.getState().bindScope(scope);
    await useTaskRuntimeStore.getState().loadByConversation(scope.workspaceId, 'conversation-1');
    await useTaskRuntimeStore.getState().loadByRun(scope, 'run-1');
    const committed = deferred<TaskRun>();
    mocks.updateGoal.mockReturnValueOnce(committed.promise);
    const action = useTaskRuntimeStore
      .getState()
      .updateGoal('run-1', 1, revised.goal, '用户明确修改');
    await useTaskRuntimeStore.getState().loadByRun(scope, 'run-b');
    await useTaskRuntimeStore.getState().loadByRun(scope, 'run-1');
    mocks.getRun.mockResolvedValue(revised);
    mocks.getCompletionGate.mockResolvedValue({
      run_id: revised.run_id,
      goal_revision: 2,
      plan_revision: 0,
      ready: false,
      requirements: [],
      blockers: [],
    });
    committed.resolve(revised);
    await expect(action).resolves.toBe(true);
    expect(useTaskRuntimeStore.getState().activeRun).toEqual(revised);
    expect(taskRuntimePresentation(useTaskRuntimeStore.getState()).activeRun).toEqual(revised);
    expect(useTaskRuntimeStore.getState().historySnapshot?.activeRun).toEqual(revised);
  });

  it('does not retire a pending inventory read when next page has no continuation', async () => {
    const page = deferred<TaskRunPage>();
    mocks.listRuns.mockReturnValueOnce(page.promise);
    const pending = useTaskRuntimeStore.getState().loadRuns(scope, ['completed']);
    await useTaskRuntimeStore.getState().loadRuns(scope, [], true);
    page.resolve({ items: [run('completed')], next_continuation: null });
    await pending;
    expect(useTaskRuntimeStore.getState().loadingRuns).toBe(false);
    expect(useTaskRuntimeStore.getState().runPage).toHaveLength(1);
    expect(useTaskRuntimeStore.getState().runStatuses).toEqual(['completed']);
  });

  it('loads an exact historical run without asking for the newer run in its conversation', async () => {
    mocks.getRun.mockResolvedValue(run('completed'));
    mocks.latestRunForConversation.mockResolvedValue({ ...run('running'), run_id: 'newer-run' });

    await useTaskRuntimeStore.getState().loadByRun(scope, 'run-1');

    expect(mocks.getRun).toHaveBeenCalledWith('workspace-1', 'run-1', 'generation-1');
    expect(mocks.latestRunForConversation).not.toHaveBeenCalled();
    expect(taskRuntimePresentation(useTaskRuntimeStore.getState()).activeRun?.run_id).toBe('run-1');
    await useTaskRuntimeStore.getState().loadByConversation('workspace-1', 'conversation-1');
    expect(mocks.latestRunForConversation).toHaveBeenCalledTimes(1);
    expect(useTaskRuntimeStore.getState().activeRun?.run_id).toBe('newer-run');
    expect(taskRuntimePresentation(useTaskRuntimeStore.getState()).activeRun?.run_id).toBe('run-1');
    expect(useTaskRuntimeStore.getState().selectedRunId).toBe('run-1');
  });

  it('keeps event history incomplete until its explicit next page has been consumed', async () => {
    mocks.getRun.mockResolvedValue(run('completed'));
    mocks.listEvents
      .mockResolvedValueOnce(eventPage([note('1')], '0', true))
      .mockResolvedValueOnce(eventPage([note('2')], '1', false));

    await useTaskRuntimeStore.getState().loadByRun(scope, 'run-1');
    expect(taskRuntimePresentation(useTaskRuntimeStore.getState()).hasMoreEvents).toBe(true);
    expect(taskRuntimePresentation(useTaskRuntimeStore.getState()).lastSeq).toBe('1');
    await useTaskRuntimeStore.getState().loadMoreEvents();
    expect(mocks.listEvents).toHaveBeenLastCalledWith('workspace-1', 'run-1', '1', 'generation-1');
    expect(taskRuntimePresentation(useTaskRuntimeStore.getState()).hasMoreEvents).toBe(false);
    expect(
      taskRuntimePresentation(useTaskRuntimeStore.getState()).events.map((event) => event.seq)
    ).toEqual(['1', '2']);
  });

  it('uses the original run continuation and retains only the current inventory page', async () => {
    const older = { ...run('completed'), run_id: 'older' };
    mocks.listRuns
      .mockResolvedValueOnce({ items: [run('completed')], next_continuation: 'opaque-next' })
      .mockResolvedValueOnce({ items: [older], next_continuation: null });
    await useTaskRuntimeStore.getState().loadRuns(scope, ['completed']);
    await useTaskRuntimeStore.getState().loadRuns(scope, [], true);
    expect(mocks.listRuns).toHaveBeenLastCalledWith(scope, ['completed'], 'opaque-next');
    expect(useTaskRuntimeStore.getState().runPage.map((item) => item.run_id)).toEqual(['older']);
    expect(useTaskRuntimeStore.getState().nextRunContinuation).toBeNull();
  });

  it('fences a late inventory result when the same workspace gets another generation', async () => {
    const old = deferred<TaskRunPage>();
    const changed = { ...scope, workspaceGeneration: 'generation-2' };
    mocks.listRuns.mockReturnValueOnce(old.promise).mockResolvedValueOnce({
      items: [{ ...run('completed'), run_id: 'new-generation' }],
      next_continuation: null,
    });
    const oldRead = useTaskRuntimeStore.getState().loadRuns(scope);
    await useTaskRuntimeStore.getState().loadRuns(changed);
    old.resolve({ items: [run('running')], next_continuation: 'old-next' });
    await oldRead;
    expect(useTaskRuntimeStore.getState().taskScope).toEqual(changed);
    expect(useTaskRuntimeStore.getState().runPage.at(0)?.run_id).toBe('new-generation');
    expect(useTaskRuntimeStore.getState().nextRunContinuation).toBeNull();
  });

  it('preserves a displayed run page when the following query fails instead of showing an empty complete list', async () => {
    mocks.listRuns
      .mockResolvedValueOnce({ items: [run('completed')], next_continuation: 'next' })
      .mockRejectedValueOnce(new Error('journal recovery failed'));
    await useTaskRuntimeStore.getState().loadRuns(scope);
    await useTaskRuntimeStore.getState().loadRuns(scope, [], true);
    expect(useTaskRuntimeStore.getState().runPage).toHaveLength(1);
    expect(useTaskRuntimeStore.getState().nextRunContinuation).toBe('next');
    expect(useTaskRuntimeStore.getState().runListError).toContain('journal recovery failed');
  });

  it('does not advance the event cursor or clear the known page when a journal gap is reported', async () => {
    mocks.getRun.mockResolvedValue(run('completed'));
    mocks.listEvents
      .mockResolvedValueOnce(eventPage([note('1')], '0', true))
      .mockResolvedValueOnce(eventPage([note('3')], '1', false));
    await useTaskRuntimeStore.getState().loadByRun(scope, 'run-1');
    await useTaskRuntimeStore.getState().loadMoreEvents();
    expect(taskRuntimePresentation(useTaskRuntimeStore.getState()).error).toContain('缺口');
    expect(taskRuntimePresentation(useTaskRuntimeStore.getState()).lastSeq).toBe('1');
    expect(taskRuntimePresentation(useTaskRuntimeStore.getState()).hasMoreEvents).toBe(true);
    expect(
      taskRuntimePresentation(useTaskRuntimeStore.getState()).events.map((event) => event.seq)
    ).toEqual(['1']);
  });

  it('ignores a late first event page after a different historical run was selected', async () => {
    const firstPage = deferred<TaskEventPage>();
    const other = { ...run('completed'), run_id: 'run-2' };
    mocks.getRun.mockResolvedValueOnce(run('completed')).mockResolvedValueOnce(other);
    mocks.listEvents
      .mockReturnValueOnce(firstPage.promise)
      .mockResolvedValueOnce(eventPage([], '0'));
    const first = useTaskRuntimeStore.getState().loadByRun(scope, 'run-1');
    await vi.waitFor(() => expect(mocks.listEvents).toHaveBeenCalledTimes(1));
    await useTaskRuntimeStore.getState().loadByRun(scope, 'run-2');
    firstPage.resolve(eventPage([note('1')], '0', true));
    await first;
    expect(taskRuntimePresentation(useTaskRuntimeStore.getState()).activeRun?.run_id).toBe('run-2');
    expect(taskRuntimePresentation(useTaskRuntimeStore.getState()).lastSeq).toBe('0');
    expect(taskRuntimePresentation(useTaskRuntimeStore.getState()).hasMoreEvents).toBe(false);
  });
  it('does not refresh the old run when its accepted control settles after another selection', async () => {
    const accepted = deferred<{ success: boolean; run_id: string }>();
    mocks.getRun
      .mockResolvedValueOnce(run('running'))
      .mockResolvedValueOnce({ ...run('completed'), run_id: 'run-2' });
    await useTaskRuntimeStore.getState().loadByRun(scope, 'run-1');
    mocks.pauseRun.mockReturnValueOnce(accepted.promise);
    const action = useTaskRuntimeStore.getState().pause('run-1');
    await useTaskRuntimeStore.getState().loadByRun(scope, 'run-2');
    accepted.resolve({ success: true, run_id: 'run-1' });
    await action;
    expect(mocks.getRun).toHaveBeenCalledTimes(2);
    expect(taskRuntimePresentation(useTaskRuntimeStore.getState()).activeRun?.run_id).toBe('run-2');
    expect(taskRuntimePresentation(useTaskRuntimeStore.getState()).error).toBeNull();
  });
  it('keeps chat progress and status on its live run while caching only one history detail', async () => {
    const live = { ...run('paused'), run_id: 'run-live' };
    const old = { ...run('failed'), run_id: 'run-old' };
    mocks.latestRunForConversation.mockResolvedValueOnce(live);
    await useTaskRuntimeStore.getState().loadByConversation('workspace-1', 'conversation-1');
    const chatTodos = useTaskRuntimeStore.getState().todos;
    mocks.getRun.mockResolvedValueOnce(old);
    await useTaskRuntimeStore.getState().loadByRun(scope, old.run_id);
    expect(useTaskRuntimeStore.getState().activeRun?.run_id).toBe('run-live');
    expect(useTaskRuntimeStore.getState().activeRun?.status).toBe('paused');
    expect(useTaskRuntimeStore.getState().todos).toBe(chatTodos);
    expect(taskRuntimePresentation(useTaskRuntimeStore.getState()).activeRun?.run_id).toBe(
      'run-old'
    );
    expect(useTaskRuntimeStore.getState().historySnapshot?.activeRun?.run_id).toBe('run-old');
  });

  it('targets an historical control without replacing the live chat run or dismissing another prompt', async () => {
    const live = { ...run('paused'), run_id: 'run-live' };
    const old = { ...run('paused'), run_id: 'run-old' };
    mocks.latestRunForConversation.mockResolvedValueOnce(live);
    await useTaskRuntimeStore.getState().loadByConversation('workspace-1', 'conversation-1');
    mocks.getRun.mockResolvedValue(old);
    await useTaskRuntimeStore.getState().loadByRun(scope, old.run_id);
    const prompt = {
      runId: live.run_id,
      goal: 'current',
      newMessage: 'input',
      resolve: vi.fn(async () => {}),
    };
    useTaskRuntimeStore.getState().openInterruptPrompt(prompt);
    mocks.resumeRun.mockResolvedValueOnce({ kind: 'resumed', run_id: old.run_id, turn_id: null });
    await useTaskRuntimeStore.getState().resumeTaskRun({
      workspace_id: 'workspace-1',
      run_id: old.run_id,
      workspace_generation: scope.workspaceGeneration,
    });
    expect(mocks.resumeRun).toHaveBeenCalledWith('workspace-1', 'run-old', 'generation-1');
    expect(useTaskRuntimeStore.getState().activeRun?.run_id).toBe('run-live');
    expect(useTaskRuntimeStore.getState().interruptPrompt).toBe(prompt);
  });
  it.each([
    ['conversation', 'refresh'],
    ['conversation', 'goal-receipt'],
    ['history', 'refresh'],
    ['history', 'goal-receipt'],
  ] as const)(
    'pins a selected run while a new %s run loads across the old run %s',
    async (view, update) => {
      const first = { ...run('paused'), run_id: 'run-live' };
      const successor = { ...first, run_id: 'run-successor' };
      mocks.latestRunForConversation.mockResolvedValueOnce(first);
      useTaskRuntimeStore.getState().bindScope(scope);
      await useTaskRuntimeStore.getState().loadByConversation('workspace-1', 'conversation-1');
      mocks.getRun.mockResolvedValue(first);
      await useTaskRuntimeStore.getState().loadByRun(scope, first.run_id);
      const accepted = deferred<TaskRun>();
      mocks.updateGoal.mockReturnValueOnce(accepted.promise);
      const action =
        update === 'goal-receipt'
          ? useTaskRuntimeStore.getState().updateGoal(first.run_id, 1, 'revised A', '用户修改')
          : null;
      const nextPlan = deferred<PlanRevision | null>();
      mocks.getPlan.mockImplementation((_workspace: string, runId: string) =>
        runId === successor.run_id ? nextPlan.promise : Promise.resolve(null)
      );
      mocks.latestRunForConversation.mockResolvedValueOnce(successor);
      mocks.getRun.mockImplementation((_workspace: string, runId: string) =>
        Promise.resolve(runId === successor.run_id ? successor : first)
      );
      const nextRead =
        view === 'conversation'
          ? useTaskRuntimeStore.getState().loadByConversation('workspace-1', 'conversation-1')
          : useTaskRuntimeStore.getState().loadByRun(scope, successor.run_id);
      await vi.waitFor(() =>
        expect(mocks.getPlan).toHaveBeenCalledWith('workspace-1', successor.run_id, 'generation-1')
      );
      if (action) {
        const revised = { ...first, goal: 'revised A', goal_revision: 2 };
        mocks.getRun.mockImplementation((_workspace: string, runId: string) =>
          Promise.resolve(runId === successor.run_id ? successor : revised)
        );
        accepted.resolve(revised);
        await action;
        expect(useTaskRuntimeStore.getState().activeRun?.goal_revision).toBe(2);
      } else {
        await useTaskRuntimeStore.getState().refresh('workspace-1', first.run_id);
      }
      nextPlan.resolve(null);
      await nextRead;
      const state = useTaskRuntimeStore.getState();
      expect(state.activeRun?.run_id).toBe(
        view === 'conversation' ? successor.run_id : first.run_id
      );
      expect(state.historySnapshot?.activeRun?.run_id).toBe(
        view === 'conversation' ? first.run_id : successor.run_id
      );
      expect(taskRuntimePresentation(state).activeRun?.run_id).toBe(
        view === 'conversation' ? first.run_id : successor.run_id
      );
      expect(state.loadingRun).toBe(false);
      expect(state.error).toBeNull();
      expect(state.historyError).toBeNull();
      expect(state.pollingInterval).not.toBeNull();
    }
  );

  it('settles only the new detail busy owner after following chat supersedes a pending detail', async () => {
    const old = deferred<TaskRun | null>();
    const next = deferred<TaskRun | null>();
    mocks.getRun.mockReturnValueOnce(old.promise).mockReturnValueOnce(next.promise);
    const oldRead = useTaskRuntimeStore.getState().loadByRun(scope, 'run-1');
    expect(useTaskRuntimeStore.getState().loadingRun).toBe(true);
    await useTaskRuntimeStore.getState().followConversation('workspace-1', 'conversation-1');
    expect(useTaskRuntimeStore.getState().loadingRun).toBe(false);
    const nextRead = useTaskRuntimeStore.getState().loadByRun(scope, 'run-2');
    old.resolve(run('completed'));
    await oldRead;
    expect(useTaskRuntimeStore.getState().loadingRun).toBe(true);
    next.resolve({ ...run('completed'), run_id: 'run-2' });
    await nextRead;
    expect(useTaskRuntimeStore.getState().loadingRun).toBe(false);
    expect(taskRuntimePresentation(useTaskRuntimeStore.getState()).activeRun?.run_id).toBe('run-2');
  });

  it.each([false, true])(
    'releases superseded event busy state without clearing a new reader, replacedScope=%s',
    async (replacedScope) => {
      const oldPage = deferred<TaskEventPage>();
      const nextPage = deferred<TaskEventPage>();
      mocks.getRun.mockResolvedValue(run('completed'));
      mocks.listEvents.mockResolvedValueOnce(eventPage([note('1')], '0', true));
      await useTaskRuntimeStore.getState().loadByRun(scope, 'run-1');
      mocks.listEvents.mockReturnValueOnce(oldPage.promise);
      const oldRead = useTaskRuntimeStore.getState().loadMoreEvents();
      expect(useTaskRuntimeStore.getState().loadingEvents).toBe(true);
      const nextScope = replacedScope ? { ...scope, workspaceGeneration: 'generation-2' } : scope;
      mocks.getRun.mockResolvedValue({ ...run('completed'), run_id: 'run-2' });
      mocks.listEvents.mockResolvedValueOnce(eventPage([note('1', 'run-2')], '0', true));
      await useTaskRuntimeStore.getState().loadByRun(nextScope, 'run-2');
      expect(useTaskRuntimeStore.getState().loadingEvents).toBe(false);
      mocks.listEvents.mockReturnValueOnce(nextPage.promise);
      const nextRead = useTaskRuntimeStore.getState().loadMoreEvents();
      expect(useTaskRuntimeStore.getState().loadingEvents).toBe(true);
      oldPage.resolve(eventPage([note('2')], '1', false));
      await oldRead;
      expect(useTaskRuntimeStore.getState().loadingEvents).toBe(true);
      nextPage.resolve(eventPage([note('2', 'run-2')], '1', false));
      await nextRead;
      expect(useTaskRuntimeStore.getState().loadingEvents).toBe(false);
      expect(taskRuntimePresentation(useTaskRuntimeStore.getState()).lastSeq).toBe('2');
    }
  );

  it('sends the captured generation and does not project its late receipt into a replacement same-id run', async () => {
    mocks.getRun.mockResolvedValue(run('paused'));
    await useTaskRuntimeStore.getState().loadByRun(scope, 'run-1');
    const old = deferred<{ success: boolean; run_id: string }>();
    mocks.pauseRun.mockReturnValueOnce(old.promise);
    const action = useTaskRuntimeStore.getState().pause('run-1');
    expect(mocks.pauseRun).toHaveBeenLastCalledWith('workspace-1', 'run-1', 'generation-1');
    const changed = { ...scope, workspaceGeneration: 'generation-2' };
    await useTaskRuntimeStore.getState().loadByRun(changed, 'run-1');
    const successor = {
      runId: 'run-1',
      goal: 'new generation',
      newMessage: 'B',
      resolve: vi.fn(async () => {}),
    };
    useTaskRuntimeStore.getState().openInterruptPrompt(successor);
    const readsBefore = mocks.getRun.mock.calls.length;
    old.resolve({ success: true, run_id: 'run-1' });
    await action;
    expect(mocks.getRun).toHaveBeenCalledTimes(readsBefore);
    expect(useTaskRuntimeStore.getState().interruptPrompt).toBe(successor);
    await useTaskRuntimeStore.getState().pause('run-1');
    expect(mocks.pauseRun).toHaveBeenLastCalledWith('workspace-1', 'run-1', 'generation-2');
  });
});
