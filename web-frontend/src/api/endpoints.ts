import { get, post, put, del } from './client';
import type {
  SessionInfo, ToolInfo, SkillInfo, McpServerInfo, McpConfig,
  ConnectMcpRequest, MemoryEntry, NamespacesResponse, SnapshotInfo, ConfigInfo,
  PermissionRule, AuditLog, WorkflowInfo, HistoryResponse,
  SandboxStatus, SandboxConfig, SandboxExecuteRequest, SandboxExecuteResult,
  CompressResponse, CompressionStats,
  ExtractResponse, ValidateSchemaResponse, ExtractExample,
  ConversationListItem, ConversationRecord, SavedMessage,
  ContextStats, FullConfigResponse, FullConfigUpdateRequest,
  TrajectoryEntry, TrajectoryStats, CuratorStatus, CuratorTransition,
  ProviderListResponse, TestConnectionResponse, SwitchModelResponse,
} from '../types/api';

export const sessionApi = {
  get: () => get<SessionInfo>('/session'),
  reset: () => post<SessionInfo>('/session/reset'),
  getLatest: () => get<{ found: boolean; id?: string; title?: string; updated_at?: string; message_count?: number; error?: string }>('/session/latest'),

  createCheckpoint: () => post<{ success: boolean; snapshot_id?: string }>('/session/checkpoint'),
  listCheckpoints: () => get<SnapshotInfo[]>('/session/checkpoints'),
  restoreCheckpoint: (id: string) => post<{ success: boolean; restored_to?: string }>(`/session/restore/${id}`),
};

export const historyApi = {
  getHistory: () => get<HistoryResponse>('/history'),
  exportMarkdown: () => get<{ format: string; content: string; message_count: number }>('/history/export?format=markdown'),
  exportJson: () => get<{ format: string; content: string; message_count: number }>('/history/export?format=json'),
};

export const toolsApi = {
  list: () => get<ToolInfo[]>('/tools'),
  get: (name: string) => get<ToolInfo>(`/tools/${name}`),
  enable: (name: string) => post<{ success: boolean }>(`/tools/${name}/enable`),
  disable: (name: string) => post<{ success: boolean }>(`/tools/${name}/disable`),
};

export const skillsApi = {
  list: () => get<SkillInfo[]>('/skills'),
  get: (name: string) => get<SkillInfo>(`/skills/${name}`),
  load: (dir: string) => post<{ success: boolean }>('/skills/load', { dir }),
  upload: (rootDir: string, files: { path: string; content: string }[]) =>
    post<{ message: string; loaded: string[]; skills: SkillInfo[] }>('/skills/upload', { root_dir: rootDir, files }),
};

export const mcpApi = {
  list: () => get<McpServerInfo[]>('/mcp'),
  get: (name: string) => get<McpServerInfo>(`/mcp/${name}`),
  connect: (req: ConnectMcpRequest) => post<McpServerInfo>('/mcp/connect', req),
  disconnect: (name: string) => post<{ success: boolean }>(`/mcp/${name}/disconnect`),
  getConfig: () => get<McpConfig>('/mcp/config'),
  updateConfig: (config: McpConfig) => put<{ success: boolean; message?: string; errors?: string[] }>('/mcp/config', config),
};

export const memoryApi = {
  list: (namespace?: string) => get<MemoryEntry[]>(`/memory/list${namespace ? `?namespace=${namespace}` : ''}`),
  add: (entry: { namespace: string; key: string; value: any }) => post<{ success: boolean; key: string; message: string }>('/memory', entry),
  search: (query: string, namespace?: string) => post<MemoryEntry[]>('/memory/search', { query, namespace }),
  delete: (entry: { namespace: string; key: string }) => post<{ success: boolean; message: string }>('/memory/delete', entry),
  namespaces: () => get<NamespacesResponse>('/memory/namespaces'),
};

export const configApi = {
  get: () => get<ConfigInfo>('/config'),
  update: (cfg: Partial<ConfigInfo>) => put<ConfigInfo>('/config', cfg),
  getFull: () => get<FullConfigResponse>('/config/full'),
  updateFull: (cfg: Partial<FullConfigUpdateRequest>, signal?: AbortSignal) => put<FullConfigResponse>('/config/full', cfg, signal),
};

export const permissionsApi = {
  getMode: () => get<{ mode: string }>('/permissions/mode'),
  setMode: (mode: string) => put<{ success: boolean }>('/permissions/mode', { mode }),
  listRules: () => get<PermissionRule[]>('/permissions/rules'),
  addRule: (rule: Omit<PermissionRule, 'priority'>) => post<PermissionRule>('/permissions/rules', rule),
  removeRule: (name: string) => del<{ success: boolean }>(`/permissions/rules/${name}`),
};

export const auditApi = {
  logs: () => get<{ logs: AuditLog[]; total: number; offset: number; limit: number }>('/audit/logs'),
  stats: () => get<{ total: number; allowed: number; denied: number; asked: number }>('/audit/stats'),
  clear: () => del<{ success: boolean }>('/audit/logs'),
};

export const workflowApi = {
  list: () => get<WorkflowInfo[]>('/workflow'),
  get: (id: string) => get<WorkflowInfo>(`/workflow/${id}`),
  create: (definition: string, name?: string) => post<WorkflowInfo>('/workflow', { definition, name }),
  delete: (id: string) => del<{ success: boolean }>(`/workflow/${id}`),
  execute: (id: string, input?: unknown) => post<{ success: boolean; result?: unknown }>(`/workflow/${id}/execute`, { input }),
};

export const sandboxApi = {
  status: () => get<SandboxStatus>('/sandbox/status'),
  config: () => get<SandboxConfig>('/sandbox/config'),
  updateConfig: (cfg: SandboxConfig) => put<SandboxConfig>('/sandbox/config', cfg),
  execute: (req: SandboxExecuteRequest) => post<SandboxExecuteResult>('/sandbox/execute', req),
};

export const compressApi = {
  trigger: (options?: { keep_messages?: number }) => post<CompressResponse>('/compress', options),
  getStats: () => get<CompressionStats>('/compress/stats'),
};

export const extractApi = {
  extract: (input: string, schema: object, schema_name?: string) =>
    post<ExtractResponse>('/extract', { input, schema, schema_name }),
  validateSchema: (schema: object) =>
    post<ValidateSchemaResponse>('/extract/validate', { schema }),
  getExamples: () => get<ExtractExample[]>('/extract/examples'),
};

export const conversationApi = {
  list: () => get<ConversationListItem[]>('/conversations'),
  save: (data: { id: string; title: string; messages: SavedMessage[]; model?: string }) =>
    post<{ success: boolean; id: string }>('/conversations', data),
  get: (id: string) => get<ConversationRecord>(`/conversations/${id}`),
  update: (id: string, data: { title?: string; messages?: SavedMessage[] }) =>
    put<{ success: boolean }>(`/conversations/${id}`, data),
  delete: (id: string) => del<{ success: boolean }>(`/conversations/${id}`),
  export: (id: string) => get<{ format: string; content: string; id: string }>(`/conversations/${id}/export`),
  restore: (id: string) =>
    post<{ success: boolean; message_count: number; conversation_id: string }>(`/conversations/${id}/restore`),
};

export const contextApi = {
  get: () => get<ContextStats>('/context'),
};

export interface BackgroundTask {
  id: string;
  description: string;
  status: string;
  created_at: string;
  updated_at: string;
  result?: string;
  error?: string;
  kind?: string;
  progress?: number;
}

export interface SubmitTaskRequest {
  kind: string;
  description: string;
  params: Record<string, unknown>;
}

export const tasksApi = {
  list: () => get<BackgroundTask[]>('/tasks'),
  get: (id: string) => get<BackgroundTask>(`/tasks/${id}`),
  submit: (req: SubmitTaskRequest) => post<{ success: boolean; task_id: string }>('/tasks', req),
  cancel: (id: string) => post<{ success: boolean; task_id: string }>(`/tasks/${id}/cancel`),
};

// ── Trace Events API ─────────────────────────────────────────────────

export interface TraceEvent {
  timestamp: string;
  kind: TraceKind;
  duration_ms?: number;
  metadata?: Record<string, unknown>;
}

export type TraceKind =
  | { type: 'llm_call'; model: string; input_tokens: number; output_tokens: number }
  | { type: 'tool_call'; tool: string; success: boolean; error?: string }
  | { type: 'agent_step'; step_number: number; thought_preview?: string }
  | { type: 'pipeline_stage'; pipeline: string; stage: string }
  | { type: 'memory_access'; operation: string; results_count?: number }
  | { type: 'mcp_call'; server: string; method: string }
  | { type: 'context_compression'; before_messages: number; after_messages: number; before_tokens: number; after_tokens: number };

export interface TraceSummary {
  session_id: string;
  total_duration_ms: number;
  llm_calls: number;
  total_input_tokens: number;
  total_output_tokens: number;
  tool_calls: number;
  tool_success_rate: number;
  agent_steps: number;
  events: TraceEvent[];
}

export const traceEventsApi = {
  listSessions: () => get<string[]>('/trace-events/sessions'),
  getEvents: (sessionId: string) => get<TraceEvent[]>(`/trace-events/${sessionId}`),
  getSummary: (sessionId: string) => get<TraceSummary>(`/trace-events/${sessionId}/summary`),
  clearSession: (sessionId: string) => del<{ cleared: string }>(`/trace-events/${sessionId}`),
};

// ── Files API ───────────────────────────────────────────────────────

export interface FileEntry {
  name: string;
  path: string;
  is_dir: boolean;
  size: number;
  modified?: string;
  extension?: string;
}

export interface FileTreeNode {
  name: string;
  path: string;
  is_dir: boolean;
  children?: FileTreeNode[];
}

export interface FileContent {
  path: string;
  content: string;
  size: number;
  language?: string;
}

export interface DiffLine {
  tag: 'equal' | 'insert' | 'delete';
  old_line?: number;
  new_line?: number;
  content: string;
}

export interface DiffHunk {
  old_start: number;
  old_count: number;
  new_start: number;
  new_count: number;
  lines: DiffLine[];
}

export interface DiffResult {
  path: string;
  old_content: string;
  new_content: string;
  hunks: DiffHunk[];
}

export const filesApi = {
  list: (path?: string) =>
    get<FileEntry[]>(`/files/list${path ? `?path=${encodeURIComponent(path)}` : ''}`),
  read: (path: string) => get<FileContent>(`/files/read?path=${encodeURIComponent(path)}`),
  diff: (path: string, gitRef = 'HEAD') =>
    get<DiffResult>(`/files/diff?path=${encodeURIComponent(path)}&git_ref=${gitRef}`),
  tree: (depth = 3) => get<FileTreeNode[]>(`/files/tree?depth=${depth}`),
};

// ── Terminal API ────────────────────────────────────────────────────

export interface TerminalSession {
  id: string;
  cwd: string;
  created_at: string;
}

export const terminalApi = {
  list: () => get<TerminalSession[]>('/terminal'),
  create: (cwd?: string) => post<TerminalSession>('/terminal', { cwd }),
  close: (id: string) => del<{ closed: string }>(`/terminal/${id}`),
  wsUrl: (id: string) => {
    const protocol = window.location.protocol === 'https:' ? 'wss:' : 'ws:';
    return `${protocol}//${window.location.host}/api/terminal/${id}/ws`;
  },
};

// ── Papers API ──────────────────────────────────────────────────────

export interface Paper {
  id: string;
  title: string;
  authors: string[];
  abstract_text?: string;
  doi?: string;
  arxiv_id?: string;
  year?: number;
  venue?: string;
  tags: string[];
  notes?: string;
  pdf_path?: string;
  added_at: string;
}

export interface CreatePaperRequest {
  title: string;
  authors?: string[];
  abstract_text?: string;
  doi?: string;
  arxiv_id?: string;
  year?: number;
  venue?: string;
  tags?: string[];
}

export const papersApi = {
  list: (params?: { tag?: string; search?: string }) => {
    const q = new URLSearchParams();
    if (params?.tag) q.set('tag', params.tag);
    if (params?.search) q.set('search', params.search);
    const qs = q.toString();
    return get<Paper[]>(`/papers${qs ? `?${qs}` : ''}`);
  },
  get: (id: string) => get<Paper>(`/papers/${id}`),
  create: (req: CreatePaperRequest) => post<Paper>('/papers', req),
  delete: (id: string) => del<{ deleted: string }>(`/papers/${id}`),
  updateNotes: (id: string, notes: string) =>
    put<Paper>(`/papers/${id}/notes`, { notes }),
  addTags: (id: string, tags: string[]) =>
    post<Paper>(`/papers/${id}/tags`, { tags }),
};

// ── Scratchpad API ──────────────────────────────────────────────────

export interface ScratchpadContent {
  content: string;
  modified_at: string;
}

export const scratchpadApi = {
  get: () => get<ScratchpadContent>('/scratchpad'),
  update: (content: string) => put<ScratchpadContent>('/scratchpad', { content }),
};

// ── Decisions API ───────────────────────────────────────────────────

export interface Decision {
  id: string;
  decision: string;
  rationale: string;
  alternatives: string[];
  context?: string;
  timestamp: string;
}

export interface CreateDecisionRequest {
  decision: string;
  rationale: string;
  alternatives?: string[];
  context?: string;
}

export const decisionsApi = {
  list: (limit?: number) =>
    get<Decision[]>(`/decisions${limit ? `?limit=${limit}` : ''}`),
  create: (req: CreateDecisionRequest) => post<Decision>('/decisions', req),
  clear: () => del<{ cleared: boolean }>('/decisions'),
};

// ── Workspace API ──

export interface Workspace {
  id: string;
  name: string;
  root: string;
  project_root?: string;
  kind: { type: string } & Record<string, unknown>;
  metadata: {
    description?: string;
    tags: string[];
  };
  created_at: string;
  last_active: string;
}

export interface WorkspaceListResponse {
  workspaces: Workspace[];
  count: number;
}

export const workspaceApi = {
  list: () => get<WorkspaceListResponse>('/workspaces'),
  create: (name: string, kind?: string, root?: string) =>
    post<{ success: boolean; workspace: Workspace }>('/workspaces', { name, kind, root }),
  current: () => get<{ workspace: Workspace | null; active: boolean }>('/workspaces/current'),
  get: (id: string) => get<Workspace>(`/workspaces/${id}`),
  switch: (id: string) => post<{ success: boolean; workspace: Workspace }>(`/workspaces/${id}/switch`, {}),
  delete: (id: string) => del<{ success: boolean }>(`/workspaces/${id}`),
  linkProject: (id: string, path: string) =>
    post<{ success: boolean; workspace: Workspace }>(`/workspaces/${id}/link`, { path }),
  defaultRoot: (name: string) =>
    get<{ default_root: string }>(`/workspaces/default-root/${encodeURIComponent(name)}`),
};

// ── Evolution API (自进化) ─────────────────────────────────────────

export const evolutionApi = {
  trajectories: (date?: string) =>
    get<{ trajectories: TrajectoryEntry[]; count: number }>(
      `/evolution/trajectories${date ? `?date=${date}` : ''}`
    ),
  trajectoryStats: () =>
    get<{ stats: TrajectoryStats }>('/evolution/trajectories/stats'),
  review: (runId?: string) =>
    post<{ success: boolean; run_id: string; actions: string[]; nothing_to_save: boolean; error?: string | null }>(
      '/evolution/review',
      { run_id: runId }
    ),
  curator: (action: string, skillName?: string) =>
    post<{
      success: boolean;
      status?: CuratorStatus;
      transitions?: CuratorTransition[];
      count?: number;
      pinned?: string;
      unpinned?: string;
      error?: string;
    }>('/evolution/curator', { action, skill_name: skillName }),
};

// ── Provider API ──────────────────────────────────────────────────────────

export const providerApi = {
  list: () => get<ProviderListResponse>('/providers'),
  test: (req: { provider: string; model: string; api_key?: string; base_url?: string }) =>
    post<TestConnectionResponse>('/providers/test', req),
  switch: (req: { model: string; api_key?: string; base_url?: string; provider?: string; temperature?: number; max_tokens?: number }) =>
    post<SwitchModelResponse>('/providers/switch', req),
};

// ── Plugin API ──────────────────────────────────────────────────────────

export interface PluginInfo {
  name: string;
  display_name: string;
  version: string;
  description: string;
  author: string | null;
  license: string | null;
  scope: string;
  enabled: boolean;
  path: string;
  capabilities: string[];
  keywords: string[];
  dependencies: { name: string; version: string | null }[];
  config_keys: string[];
}

export const pluginApi = {
  list: () => get<PluginInfo[]>('/plugins'),
  get: (name: string) => get<{ info: PluginInfo; resolved?: Record<string, any> }>(`/plugins/${name}`),
  install: (req: { source: string; scope?: string }) =>
    post<{ success: boolean; plugin_id?: string; info?: PluginInfo; error?: string }>('/plugins/install', req),
  uninstall: (req: { name: string; keep_data?: boolean }) =>
    post<{ success: boolean; message?: string; error?: string }>('/plugins/uninstall', req),
  enable: (name: string) =>
    post<{ success: boolean; message?: string; error?: string }>(`/plugins/${name}/enable`),
  disable: (name: string) =>
    post<{ success: boolean; message?: string; error?: string }>(`/plugins/${name}/disable`),
  reload: () =>
    post<{ success: boolean; total?: number; enabled?: number; message?: string; error?: string }>('/plugins/reload'),
};

// ── Scheduler API (定时任务) ──────────────────────────────────────

export interface SchedulerTask {
  id: string;
  name: string;
  cron_expr: string;
  prompt: string;
  status: string;
  last_run_at: string | null;
  last_result: string | null;
  created_at: string;
  next_run: string | null;
}

export const schedulerApi = {
  list: () => get<SchedulerTask[]>('/scheduler/tasks'),
  create: (data: { name: string; cron_expr: string; prompt: string }) =>
    post<{ success: boolean }>('/scheduler/tasks', data),
  updateStatus: (id: string, enabled: boolean) =>
    put<{ success: boolean }>(`/scheduler/tasks/${id}/status`, { status: enabled ? 'enabled' : 'disabled' }),
  run: (id: string) =>
    post<{ success: boolean; result?: string; error?: string }>(`/scheduler/tasks/${id}/run`),
  delete: (id: string) => del<{ success: boolean }>(`/scheduler/tasks/${id}`),
};

// ── Auto Memory API (自动记忆) ───────────────────────────────────

export interface AutoMemoryStatus {
  enabled: boolean;
  observations_count: number;
}

export interface AutoMemoryObservation {
  category: string;
  text: string;
  confidence: number;
}

export const autoMemoryApi = {
  status: () => get<AutoMemoryStatus>('/auto-memory/status'),
  toggle: (enabled: boolean) => post<{ enabled: boolean }>('/auto-memory/toggle', { enabled }),
  extract: () => post<{ success: boolean; observations: AutoMemoryObservation[] }>('/auto-memory/extract'),
  observations: () => get<AutoMemoryObservation[]>('/auto-memory/observations'),
};

// ── Human Gate API (人工审批) ────────────────────────────────────

export interface HumanGateCheckpoint {
  task_id: string;
  prompt: string;
  context?: Record<string, unknown>;
  options?: string[];
  status: string;
  created_at: string;
}

export const humanGateApi = {
  list: () => get<HumanGateCheckpoint[]>('/tasks/checkpoints'),
  respond: (taskId: string, selection: string, instructions?: string) =>
    post<{ success: boolean }>(`/tasks/${taskId}/respond`, { selection, instructions }),
};

// ── Worktree API (Git 工作树) ─────────────────────────────────────

export interface WorktreeInfo {
  path: string;
  branch: string;
  managed: boolean;
  head: string;
}

export const worktreeApi = {
  list: () => get<WorktreeInfo[]>('/worktrees'),
  create: (req: { branch: string; base?: string }) => post<WorktreeInfo>('/worktrees', req),
  remove: (branch: string) => del<{ success: boolean }>(`/worktrees?branch=${encodeURIComponent(branch)}`),
};

