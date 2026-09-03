import { describe, expect, expectTypeOf, it } from 'vitest';
import type {
  BackgroundCellState,
  ConnectMcpRequest,
  ExtensionSkillEntry,
  RuntimeEventKind,
  RunContinuationState,
  SkillArtifactSyncReceipt,
  SkillInstallSettlementReceipt,
  SkillSyncReceipt,
  SkillUninstallSettlementReceipt,
  StreamingEvent,
  TaskRetryReceipt,
  TaskRunControlReceipt,
  TaskRunResumeReceipt,
  ToolControlReceipt,
  ToolInfo,
  WorkspaceTransitionReceipt,
} from '../generated';
import type { TauriMcpServerInfo, TauriSkillInfo } from './api';

const serializedToolInfo = {
  name: 'read_file',
  description: 'Read a file',
  parameters: { type: 'object', properties: { path: { type: 'string' } } },
  enabled: true,
  source: 'Builtin',
} satisfies ToolInfo;

const serializedToolControlReceipt = {
  success: true,
  name: 'read_file',
  policy_enabled: false,
  effective_enabled: false,
  changed: true,
  revision: 1,
} satisfies ToolControlReceipt;

const serializedMcpServer = {
  name: 'local-tools',
  status: 'disconnected',
  transport: 'stdio',
  tool_count: 0,
  tools: [],
  connected_at: null,
  error: null,
  enabled: true,
} satisfies TauriMcpServerInfo;

const serializedSkill = {
  name: 'research',
  description: 'Research workflow',
  path: '/skills/research',
  category: 'research',
  is_baseline: false,
  is_builtin: false,
  upstream_version: null,
  source: 'local',
  license: null,
  compatibility: null,
  tags: [],
  has_sandbox: false,
  depends_on: [],
  missing_dependencies: [],
  loaded: true,
  version: null,
  author: null,
} satisfies ExtensionSkillEntry satisfies TauriSkillInfo;

const settledSkillMutation = {
  operation_id: 'skill-op-1',
  idempotent: false,
  status: 'settled',
  target_receipts: [
    {
      target: 'workspace:project-a',
      workspace_generation: 'workspace-generation-2',
      status: 'settled',
      changed_entries: ['research'],
      error: null,
    },
  ],
} satisfies SkillSyncReceipt;

const committedSkillMutation = {
  ...settledSkillMutation,
  operation_id: 'skill-op-2',
  status: 'settled',
  target_receipts: [],
} satisfies SkillSyncReceipt;

const degradedSkillMutation = {
  ...settledSkillMutation,
  operation_id: 'skill-op-3',
  status: 'degraded',
  target_receipts: [
    {
      target: 'workspace:project-b',
      workspace_generation: 'workspace-generation-3',
      status: 'degraded',
      changed_entries: [],
      error: 'runtime fanout failed',
    },
  ],
} satisfies SkillSyncReceipt;

const skillReceiptContracts = {
  install: {
    name: 'research',
    installed_names: ['research'],
    path: '/skills/research',
    source: 'local',
    revision: null,
    settlement: settledSkillMutation,
  } satisfies SkillInstallSettlementReceipt,
  uninstall: {
    name: 'research',
    artifact_removed: false,
    artifact_error: 'artifact is still in use',
    settlement: degradedSkillMutation,
  } satisfies SkillUninstallSettlementReceipt,
  artifactSync: {
    results: [
      {
        name: 'research',
        success: true,
        updated: true,
        revision: 'revision-2',
        message: 'updated',
      },
    ],
    settlement: committedSkillMutation,
  } satisfies SkillArtifactSyncReceipt,
};

const connectMcpRequest = {
  name: 'local-tools',
  transport: 'stdio',
  command: 'mcp-local-tools',
  args: ['--stdio'],
  env: {},
} satisfies ConnectMcpRequest;

const streamingEvents = [
  { event: 'token', data: 'hello' },
  { event: 'tool_batch_start', tool_count: 2 },
  { event: 'cancelled' },
  { event: 'done' },
] satisfies StreamingEvent[];

const runtimeEventVariants = [
  'run_started',
  'run_steer_recorded',
  'task_completed',
  'subagent_assigned',
  'artifact_produced',
  'background_cell_started',
  'background_cell_finished',
  'run_continuation_configured',
  'run_turn_started',
  'run_turn_usage_accounted',
  'run_turn_compacted',
  'run_turn_finished',
  'run_pause_reason_changed',
  'run_cancelled',
] satisfies RuntimeEventKind[];

const serializedBackgroundCell = {
  cell_id: 'cell-1',
  name: 'test suite',
  command_hash: 'sha256:test',
  turn_id: 'turn-1',
  execution_id: null,
  call_id: 'call-1',
  phase: 'running',
  terminal_cause: null,
  terminal_message: null,
  exit_code: null,
  artifact_status: 'not_requested',
  artifact_message: null,
  total_output_bytes: 128,
  output_truncated: false,
  output_excerpt: 'tests are running',
  artifact_path: null,
  artifact_sha256: null,
  started_at: '2026-08-15T00:00:00Z',
  finished_at: null,
} satisfies BackgroundCellState;

const serializedContinuation = {
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
  pause: {
    reason: 'repeated_blocker',
    detail: 'three turns without progress',
    changed_at: '2026-08-15T00:00:00Z',
  },
  blocker_audit: {
    fingerprint: 'no-task-progress',
    consecutive_turns: 3,
  },
  provider_retry: null,
  deferred: false,
  deferred_reason: null,
} satisfies RunContinuationState;

const serializedWorkspaceTransition = {
  status: 'degraded',
  previous_workspace_id: 'workspace-a',
  target_workspace_id: 'workspace-b',
  target_root: '/workspace-b',
  degraded_subsystems: [
    {
      subsystem: 'config_watcher',
      target_root: '/workspace-b',
      stale_roots: [],
      error: 'watch settled with degraded cleanup',
    },
  ],
} satisfies WorkspaceTransitionReceipt;

const taskRuntimeMutationContracts = {
  control: {
    success: false,
    run_id: 'already-terminal',
  } satisfies TaskRunControlReceipt,
  plannedResume: {
    kind: 'resumed',
    run_id: 'run-planned',
    turn_id: null,
  } satisfies TaskRunResumeReceipt,
  continuationResume: {
    kind: 'continuation_resumed',
    run_id: 'run-continuation',
    turn_id: 'turn-1',
  } satisfies TaskRunResumeReceipt,
  retry: {
    kind: 'recovery_retry_recorded',
    run_id: 'retry-run',
    task_id: 'task-1',
    next_attempt: null,
  } satisfies TaskRetryReceipt,
};

describe('Rust serialization contracts', () => {
  it('consumes the generated ToolInfo wire fields', () => {
    expect(serializedToolInfo.parameters).toHaveProperty('properties.path');
    expectTypeOf(serializedToolInfo).toMatchTypeOf<ToolInfo>();
    expect(serializedToolControlReceipt.revision).toBe(1);
    expectTypeOf(serializedToolControlReceipt).toMatchTypeOf<ToolControlReceipt>();
  });

  it('preserves explicit nulls from Tauri projections', () => {
    expect(serializedMcpServer.connected_at).toBeNull();
    expect(serializedMcpServer.error).toBeNull();
    expect(serializedSkill.version).toBeNull();
  });

  it('keeps skill settlement states distinct across generated receipts', () => {
    expect(settledSkillMutation.status).toBe('settled');
    expect(committedSkillMutation.status).toBe('settled');
    expect(degradedSkillMutation.status).toBe('degraded');
    expect(degradedSkillMutation.target_receipts[0]?.error).toBe('runtime fanout failed');
    expect(skillReceiptContracts.install.settlement.idempotent).toBe(false);
    expect(skillReceiptContracts.uninstall.artifact_removed).toBe(false);
    expect(skillReceiptContracts.artifactSync.results[0]?.updated).toBe(true);
  });

  it('uses the flattened HTTP MCP request contract', () => {
    expect(connectMcpRequest.transport).toBe('stdio');
    expect(connectMcpRequest.command).toBe('mcp-local-tools');
    expectTypeOf(connectMcpRequest).toMatchTypeOf<ConnectMcpRequest>();
  });

  it('keeps representative streaming and runtime event variants typed', () => {
    expect(streamingEvents.map((event) => event.event)).toEqual([
      'token',
      'tool_batch_start',
      'cancelled',
      'done',
    ]);
    expect(runtimeEventVariants).toContain('artifact_produced');
    expect(runtimeEventVariants).toContain('background_cell_finished');
    expect(runtimeEventVariants).toContain('run_turn_usage_accounted');
    expect(serializedBackgroundCell.finished_at).toBeNull();
    expect(serializedContinuation.pause?.reason).toBe('repeated_blocker');
    expect(serializedContinuation.time_budget_seconds).toBe(7_200);
    expect(serializedContinuation.compaction_count).toBe(2);
  });

  it('consumes the generated workspace transition receipt', () => {
    expect(serializedWorkspaceTransition.status).toBe('degraded');
    expect(serializedWorkspaceTransition.degraded_subsystems[0]?.stale_roots).toEqual([]);
    expectTypeOf(serializedWorkspaceTransition).toMatchTypeOf<WorkspaceTransitionReceipt>();
  });

  it('keeps TaskRuntime mutation receipts generated', () => {
    expect(taskRuntimeMutationContracts.control.success).toBe(false);
    expect(taskRuntimeMutationContracts.plannedResume.turn_id).toBeNull();
    expect(taskRuntimeMutationContracts.continuationResume.turn_id).toBe('turn-1');
    expect(taskRuntimeMutationContracts.retry.kind).toBe('recovery_retry_recorded');
  });
});
