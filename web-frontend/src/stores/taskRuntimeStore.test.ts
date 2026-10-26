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
  const promise = new Promise<T>((next) => {
    resolve = next;
  });
  return { promise, resolve };
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
  it('pins a selected current run when the same conversation advances to another chat run', async () => {
    const first = { ...run('paused'), run_id: 'run-live' };
    mocks.latestRunForConversation.mockResolvedValueOnce(first);
    await useTaskRuntimeStore.getState().loadByConversation('workspace-1', 'conversation-1');
    mocks.getRun.mockResolvedValueOnce(first);
    await useTaskRuntimeStore.getState().loadByRun(scope, first.run_id);
    mocks.latestRunForConversation.mockResolvedValueOnce({ ...first, run_id: 'run-successor' });
    await useTaskRuntimeStore.getState().loadByConversation('workspace-1', 'conversation-1');
    expect(useTaskRuntimeStore.getState().activeRun?.run_id).toBe('run-successor');
    expect(taskRuntimePresentation(useTaskRuntimeStore.getState()).activeRun?.run_id).toBe(
      'run-live'
    );
  });

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
