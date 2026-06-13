import { get, post, put, del } from './client';
import { isTauri, apiInvoke } from '../lib/tauri-bridge';
import type {
  SessionInfo,
  ToolInfo,
  SkillInfo,
  McpServerInfo,
  McpConfig,
  MemoryEntry,
  NamespacesResponse,
  SnapshotInfo,
  ConfigInfo,
  PermissionRule,
  AuditLog,
  WorkflowInfo,
  HistoryResponse,
  SandboxStatus,
  SandboxConfig,
  SandboxExecuteRequest,
  SandboxExecuteResult,
  CompressResponse,
  CompressionStats,
  ExtractResponse,
  ValidateSchemaResponse,
  ExtractExample,
  ConversationListItem,
  ConversationRecord,
  SavedMessage,
  ContextStats,
  FullConfigResponse,
  FullConfigUpdateRequest,
  TrajectoryEntry,
  TrajectoryStats,
  CuratorStatus,
  CuratorTransition,
  ProviderListResponse,
  TestConnectionResponse,
  SwitchModelResponse,
} from '../types/api';

export const sessionApi = {
  get: () => (isTauri() ? apiInvoke<SessionInfo>('get_session') : get<SessionInfo>('/session')),
  reset: () =>
    isTauri() ? apiInvoke<SessionInfo>('reset_session') : post<SessionInfo>('/session/reset'),
  getLatest: () =>
    isTauri()
      ? apiInvoke<{
          found: boolean;
          id?: string;
          title?: string;
          updated_at?: string;
          message_count?: number;
          error?: string;
        }>('get_latest_session')
      : get<{
          found: boolean;
          id?: string;
          title?: string;
          updated_at?: string;
          message_count?: number;
          error?: string;
        }>('/session/latest'),

  createCheckpoint: () =>
    isTauri()
      ? apiInvoke<{ success: boolean; snapshot_id?: string }>('create_checkpoint')
      : post<{ success: boolean; snapshot_id?: string }>('/session/checkpoint'),
  listCheckpoints: () =>
    isTauri()
      ? apiInvoke<SnapshotInfo[]>('list_checkpoints')
      : get<SnapshotInfo[]>('/session/checkpoints'),
  restoreCheckpoint: (id: string) =>
    isTauri()
      ? apiInvoke<{ success: boolean; restored_to?: string }>('restore_checkpoint', {
          snapshot_id: id,
        })
      : post<{ success: boolean; restored_to?: string }>(`/session/restore/${id}`),
};

export const historyApi = {
  getHistory: () =>
    isTauri() ? apiInvoke<HistoryResponse>('get_history') : get<HistoryResponse>('/history'),
  exportMarkdown: () =>
    isTauri()
      ? apiInvoke<{ format: string; content: string; message_count: number }>(
          'export_history_markdown'
        )
      : get<{ format: string; content: string; message_count: number }>(
          '/history/export?format=markdown'
        ),
  exportJson: () =>
    isTauri()
      ? apiInvoke<{ format: string; content: string; message_count: number }>('export_history_json')
      : get<{ format: string; content: string; message_count: number }>(
          '/history/export?format=json'
        ),
};

export const toolsApi = {
  list: () => (isTauri() ? apiInvoke<ToolInfo[]>('list_tools') : get<ToolInfo[]>('/tools')),
  get: (name: string) =>
    isTauri() ? apiInvoke<ToolInfo>('get_tool', { name }) : get<ToolInfo>(`/tools/${name}`),
  enable: (name: string) =>
    isTauri()
      ? apiInvoke<{ success: boolean }>('enable_tool', { name })
      : post<{ success: boolean }>(`/tools/${name}/enable`),
  disable: (name: string) =>
    isTauri()
      ? apiInvoke<{ success: boolean }>('disable_tool', { name })
      : post<{ success: boolean }>(`/tools/${name}/disable`),
};

export const skillsApi = {
  list: () => (isTauri() ? apiInvoke<SkillInfo[]>('list_skills') : get<SkillInfo[]>('/skills')),
  get: (name: string) =>
    isTauri() ? apiInvoke<SkillInfo>('get_skill', { name }) : get<SkillInfo>(`/skills/${name}`),
  load: (dir: string) =>
    isTauri()
      ? apiInvoke<{ success: boolean }>('load_skill', { name: dir })
      : post<{ success: boolean }>('/skills/load', { dir }),
  upload: (rootDir: string, files: { path: string; content: string }[]) =>
    isTauri()
      ? apiInvoke<{ message: string; loaded: string[]; skills: SkillInfo[] }>('upload_skill')
      : post<{ message: string; loaded: string[]; skills: SkillInfo[] }>('/skills/upload', {
          root_dir: rootDir,
          files,
        }),
};

export const mcpApi = {
  list: () =>
    isTauri() ? apiInvoke<McpServerInfo[]>('list_mcp_servers') : get<McpServerInfo[]>('/mcp'),
  get: (name: string) =>
    isTauri()
      ? apiInvoke<McpServerInfo>('get_mcp_server', { name })
      : get<McpServerInfo>(`/mcp/${name}`),
  connect: (req: { name: string; transport: { transport: string; [key: string]: unknown } }) =>
    isTauri()
      ? apiInvoke<{ success: boolean; name?: string; error?: string }>('connect_mcp_server', {
          name: req.name,
          transport: req.transport,
        })
      : post<McpServerInfo>('/mcp/connect', req),
  disconnect: (name: string) =>
    isTauri()
      ? apiInvoke<{ success: boolean }>('disconnect_mcp_server', { name })
      : post<{ success: boolean }>(`/mcp/${name}/disconnect`),
  toggle: (name: string, enabled: boolean) =>
    isTauri()
      ? apiInvoke<{ success: boolean; enabled: boolean; message?: string }>('toggle_mcp_server', {
          name,
          enabled,
        })
      : post<{ success: boolean; enabled: boolean; message?: string }>(`/mcp/${name}/toggle`, {
          enabled,
        }),
  getConfig: () =>
    isTauri() ? apiInvoke<McpConfig>('get_mcp_config') : get<McpConfig>('/mcp/config'),
  updateConfig: (config: McpConfig) =>
    isTauri()
      ? apiInvoke<{ success: boolean; message?: string }>('update_mcp_config', { config })
      : put<{ success: boolean; message?: string; errors?: string[] }>('/mcp/config', config),
};

export const memoryApi = {
  list: (namespace?: string) =>
    isTauri()
      ? apiInvoke<MemoryEntry[]>('list_memory', { namespace })
      : get<MemoryEntry[]>(`/memory/list${namespace ? `?namespace=${namespace}` : ''}`),
  add: (entry: { namespace: string; key: string; value: any }) =>
    isTauri()
      ? apiInvoke<{ success: boolean; key: string; message: string }>('add_memory', entry)
      : post<{ success: boolean; key: string; message: string }>('/memory', entry),
  search: (query: string, namespace?: string) =>
    isTauri()
      ? apiInvoke<MemoryEntry[]>('search_memory', { query, namespace })
      : post<MemoryEntry[]>('/memory/search', { query, namespace }),
  delete: (entry: { namespace: string; key: string }) =>
    isTauri()
      ? apiInvoke<{ success: boolean; message: string }>('delete_memory', entry)
      : post<{ success: boolean; message: string }>('/memory/delete', entry),
  namespaces: () =>
    isTauri()
      ? apiInvoke<NamespacesResponse>('list_namespaces')
      : get<NamespacesResponse>('/memory/namespaces'),
};

export const configApi = {
  get: () => (isTauri() ? apiInvoke<ConfigInfo>('get_config') : get<ConfigInfo>('/config')),
  update: (cfg: Partial<ConfigInfo>) =>
    isTauri()
      ? apiInvoke<ConfigInfo>('update_config', { req: cfg })
      : put<ConfigInfo>('/config', cfg),
  getFull: () =>
    isTauri()
      ? apiInvoke<FullConfigResponse>('get_full_config')
      : get<FullConfigResponse>('/config/full'),
  updateFull: (cfg: Partial<FullConfigUpdateRequest>, _signal?: AbortSignal) =>
    isTauri()
      ? apiInvoke<FullConfigResponse>('update_full_config', { req: cfg })
      : put<FullConfigResponse>('/config/full', cfg, _signal),
};

export const permissionsApi = {
  getMode: () =>
    isTauri()
      ? apiInvoke<{ mode: string }>('get_permissions_mode')
      : get<{ mode: string }>('/permissions/mode'),
  setMode: (mode: string) =>
    isTauri()
      ? apiInvoke<{ success: boolean }>('set_permissions_mode', { mode })
      : put<{ success: boolean }>('/permissions/mode', { mode }),
  listRules: () =>
    isTauri()
      ? apiInvoke<PermissionRule[]>('list_permission_rules')
      : get<PermissionRule[]>('/permissions/rules'),
  addRule: (rule: Omit<PermissionRule, 'priority'>) =>
    isTauri()
      ? apiInvoke<PermissionRule>('add_permission_rule', {
          matcher: rule.name,
          behavior: rule.effect,
          source: 'manual',
        })
      : post<PermissionRule>('/permissions/rules', rule),
  removeRule: (name: string) =>
    isTauri()
      ? apiInvoke<{ success: boolean }>('remove_permission_rule', { matcher: name })
      : del<{ success: boolean }>(`/permissions/rules/${name}`),
};

export const auditApi = {
  logs: (offset?: number, limit?: number) =>
    isTauri()
      ? apiInvoke<{ logs: AuditLog[]; total: number; offset: number; limit: number }>(
          'get_audit_logs',
          { offset, limit }
        )
      : get<{ logs: AuditLog[]; total: number; offset: number; limit: number }>(
          `/audit/logs${offset !== undefined ? `?offset=${offset}&limit=${limit ?? 100}` : ''}`
        ),
  stats: () =>
    isTauri()
      ? apiInvoke<{ total: number; allowed: number; denied: number; asked: number }>(
          'get_audit_stats'
        )
      : get<{ total: number; allowed: number; denied: number; asked: number }>('/audit/stats'),
  clear: () =>
    isTauri()
      ? apiInvoke<{ success: boolean }>('clear_audit_logs')
      : del<{ success: boolean }>('/audit/logs'),
};

export const workflowApi = {
  list: () =>
    isTauri() ? apiInvoke<WorkflowInfo[]>('list_workflows') : get<WorkflowInfo[]>('/workflow'),
  get: (id: string) =>
    isTauri()
      ? apiInvoke<WorkflowInfo>('get_workflow', { id })
      : get<WorkflowInfo>(`/workflow/${id}`),
  create: (definition: string, name?: string) =>
    isTauri()
      ? apiInvoke<WorkflowInfo>('create_workflow', { definition, name })
      : post<WorkflowInfo>('/workflow', { definition, name }),
  delete: (id: string) =>
    isTauri()
      ? apiInvoke<{ success: boolean }>('delete_workflow', { id })
      : del<{ success: boolean }>(`/workflow/${id}`),
  execute: (id: string, input?: unknown) =>
    isTauri()
      ? apiInvoke<{ success: boolean; result?: unknown }>('execute_workflow', { id, input })
      : post<{ success: boolean; result?: unknown }>(`/workflow/${id}/execute`, { input }),
};

export const sandboxApi = {
  status: () =>
    isTauri()
      ? apiInvoke<SandboxStatus>('get_sandbox_status')
      : get<SandboxStatus>('/sandbox/status'),
  config: () =>
    isTauri()
      ? apiInvoke<SandboxConfig>('get_sandbox_config')
      : get<SandboxConfig>('/sandbox/config'),
  updateConfig: (cfg: SandboxConfig) =>
    isTauri()
      ? apiInvoke<SandboxConfig>('update_sandbox_config', { config: cfg })
      : put<SandboxConfig>('/sandbox/config', cfg),
  execute: (req: SandboxExecuteRequest) =>
    isTauri()
      ? apiInvoke<SandboxExecuteResult>('execute_sandbox', {
          code: req.code,
          language: req.language,
        })
      : post<SandboxExecuteResult>('/sandbox/execute', req),
};

export const compressApi = {
  trigger: (options?: { keep_messages?: number }) =>
    isTauri()
      ? apiInvoke<CompressResponse>('compress_context')
      : post<CompressResponse>('/compress', options),
  getStats: () =>
    isTauri()
      ? apiInvoke<CompressionStats>('get_compression_stats')
      : get<CompressionStats>('/compress/stats'),
};

export const extractApi = {
  extract: (input: string, schema: object, schema_name?: string) =>
    isTauri()
      ? apiInvoke<ExtractResponse>('extract_data', { input, schema, schema_name })
      : post<ExtractResponse>('/extract', { input, schema, schema_name }),
  validateSchema: (schema: object) =>
    isTauri()
      ? apiInvoke<ValidateSchemaResponse>('validate_schema', { schema })
      : post<ValidateSchemaResponse>('/extract/validate', { schema }),
  getExamples: () =>
    isTauri()
      ? apiInvoke<ExtractExample[]>('get_extract_examples')
      : get<ExtractExample[]>('/extract/examples'),
};

export const conversationApi = {
  list: () =>
    isTauri()
      ? apiInvoke<ConversationListItem[]>('list_conversations')
      : get<ConversationListItem[]>('/conversations'),
  save: (data: { id: string; title: string; messages: SavedMessage[]; model?: string }) =>
    isTauri()
      ? apiInvoke<{ success: boolean; id: string }>('save_conversation', {
          id: data.id,
          title: data.title,
          messages: data.messages,
        })
      : post<{ success: boolean; id: string }>('/conversations', data),
  get: (id: string) =>
    isTauri()
      ? apiInvoke<ConversationRecord>('get_conversation', { id })
      : get<ConversationRecord>(`/conversations/${id}`),
  update: (id: string, data: { title?: string; messages?: SavedMessage[] }) =>
    isTauri()
      ? apiInvoke<{ success: boolean }>('update_conversation', {
          id,
          title: data.title,
          messages: data.messages,
        })
      : put<{ success: boolean }>(`/conversations/${id}`, data),
  delete: (id: string) =>
    isTauri()
      ? apiInvoke<{ success: boolean }>('delete_conversation', { id })
      : del<{ success: boolean }>(`/conversations/${id}`),
  export: (id: string) =>
    isTauri()
      ? apiInvoke<{ format: string; content: string; id: string }>('export_conversation', { id })
      : get<{ format: string; content: string; id: string }>(`/conversations/${id}/export`),
  restore: (id: string) =>
    isTauri()
      ? apiInvoke<{ success: boolean; message_count: number; conversation_id: string }>(
          'restore_conversation',
          { id }
        )
      : post<{ success: boolean; message_count: number; conversation_id: string }>(
          `/conversations/${id}/restore`
        ),
  search: (query: string, limit?: number) =>
    isTauri()
      ? apiInvoke<ConversationListItem[]>('search_conversations', { query, limit })
      : get<ConversationListItem[]>(`/conversations/search?q=${encodeURIComponent(query)}`),
};

export const contextApi = {
  get: () =>
    isTauri() ? apiInvoke<ContextStats>('get_context_stats') : get<ContextStats>('/context'),
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
  list: () =>
    isTauri() ? apiInvoke<BackgroundTask[]>('list_tasks') : get<BackgroundTask[]>('/tasks'),
  get: (id: string) =>
    isTauri() ? apiInvoke<BackgroundTask>('get_task', { id }) : get<BackgroundTask>(`/tasks/${id}`),
  submit: (req: SubmitTaskRequest) =>
    isTauri()
      ? apiInvoke<{ success: boolean; task_id: string }>('submit_task', {
          kind: req.kind,
          description: req.description,
          params: req.params,
        })
      : post<{ success: boolean; task_id: string }>('/tasks', req),
  cancel: (id: string) =>
    isTauri()
      ? apiInvoke<{ success: boolean; task_id: string }>('cancel_task', { id })
      : post<{ success: boolean; task_id: string }>(`/tasks/${id}/cancel`),
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
  | {
      type: 'context_compression';
      before_messages: number;
      after_messages: number;
      before_tokens: number;
      after_tokens: number;
    };

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
  listSessions: () =>
    isTauri()
      ? apiInvoke<string[]>('list_trace_sessions')
      : get<string[]>('/trace-events/sessions'),
  getEvents: (sessionId: string) =>
    isTauri()
      ? apiInvoke<TraceEvent[]>('get_trace_events', { session_id: sessionId })
      : get<TraceEvent[]>(`/trace-events/${sessionId}`),
  getSummary: (sessionId: string) =>
    isTauri()
      ? apiInvoke<TraceSummary>('get_trace_summary', { session_id: sessionId })
      : get<TraceSummary>(`/trace-events/${sessionId}/summary`),
  clearSession: (sessionId: string) =>
    isTauri()
      ? apiInvoke<{ cleared: string }>('clear_trace_session', { session_id: sessionId })
      : del<{ cleared: string }>(`/trace-events/${sessionId}`),
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
    isTauri()
      ? apiInvoke<FileEntry[]>('list_files', { path })
      : get<FileEntry[]>(`/files/list${path ? `?path=${encodeURIComponent(path)}` : ''}`),
  read: (path: string) =>
    isTauri()
      ? apiInvoke<FileContent>('read_file', { path })
      : get<FileContent>(`/files/read?path=${encodeURIComponent(path)}`),
  diff: (path: string, gitRef = 'HEAD') =>
    isTauri()
      ? apiInvoke<DiffResult>('diff_file', { path, git_ref: gitRef })
      : get<DiffResult>(`/files/diff?path=${encodeURIComponent(path)}&git_ref=${gitRef}`),
  tree: (depth = 3) =>
    isTauri()
      ? apiInvoke<FileTreeNode[]>('file_tree', { depth })
      : get<FileTreeNode[]>(`/files/tree?depth=${depth}`),
};

// ── Terminal API ────────────────────────────────────────────────────

export interface TerminalSession {
  id: string;
  cwd: string;
  created_at: string;
}

export const terminalApi = {
  list: () =>
    isTauri()
      ? apiInvoke<TerminalSession[]>('list_terminal_sessions')
      : get<TerminalSession[]>('/terminal'),
  create: (cwd?: string) =>
    isTauri()
      ? apiInvoke<TerminalSession>('create_terminal', { id: `term-${Date.now()}`, cwd })
      : post<TerminalSession>('/terminal', { cwd }),
  close: (id: string) =>
    isTauri()
      ? apiInvoke<{ closed: string }>('close_terminal', { id })
      : del<{ closed: string }>(`/terminal/${id}`),
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
  list: (params?: { tag?: string; search?: string }) =>
    isTauri()
      ? apiInvoke<Paper[]>('list_papers')
      : (() => {
          const q = new URLSearchParams();
          if (params?.tag) q.set('tag', params.tag);
          if (params?.search) q.set('search', params.search);
          const qs = q.toString();
          return get<Paper[]>(`/papers${qs ? `?${qs}` : ''}`);
        })(),
  get: (id: string) =>
    isTauri() ? apiInvoke<Paper>('get_paper', { id }) : get<Paper>(`/papers/${id}`),
  create: (req: CreatePaperRequest) =>
    isTauri()
      ? apiInvoke<Paper>('create_paper', { title: req.title, authors: req.authors })
      : post<Paper>('/papers', req),
  delete: (id: string) =>
    isTauri()
      ? apiInvoke<{ deleted: string }>('delete_paper', { id })
      : del<{ deleted: string }>(`/papers/${id}`),
  updateNotes: (id: string, notes: string) =>
    isTauri()
      ? apiInvoke<Paper>('update_paper_notes', { id, notes })
      : put<Paper>(`/papers/${id}/notes`, { notes }),
  addTags: (id: string, tags: string[]) =>
    isTauri()
      ? apiInvoke<Paper>('add_paper_tags', { id, tags })
      : post<Paper>(`/papers/${id}/tags`, { tags }),
};

// ── Scratchpad API ──────────────────────────────────────────────────

export interface ScratchpadContent {
  content: string;
  modified_at: string;
}

export const scratchpadApi = {
  get: () =>
    isTauri()
      ? apiInvoke<ScratchpadContent>('get_scratchpad')
      : get<ScratchpadContent>('/scratchpad'),
  update: (content: string) =>
    isTauri()
      ? apiInvoke<ScratchpadContent>('update_scratchpad', { content })
      : put<ScratchpadContent>('/scratchpad', { content }),
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
    isTauri()
      ? apiInvoke<Decision[]>('list_decisions')
      : get<Decision[]>(`/decisions${limit ? `?limit=${limit}` : ''}`),
  create: (req: CreateDecisionRequest) =>
    isTauri()
      ? apiInvoke<Decision>('create_decision', { title: req.decision, rationale: req.rationale })
      : post<Decision>('/decisions', req),
  clear: () =>
    isTauri()
      ? apiInvoke<{ cleared: boolean }>('clear_decisions')
      : del<{ cleared: boolean }>('/decisions'),
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
  list: () =>
    isTauri()
      ? apiInvoke<WorkspaceListResponse>('list_workspaces')
      : get<WorkspaceListResponse>('/workspaces'),
  create: (name: string, kind?: string, root?: string) =>
    isTauri()
      ? apiInvoke<{ success: boolean; workspace: Workspace }>('create_workspace', {
          name,
          kind,
          root,
        })
      : post<{ success: boolean; workspace: Workspace }>('/workspaces', { name, kind, root }),
  current: () =>
    isTauri()
      ? apiInvoke<{ workspace: Workspace | null; active: boolean }>('get_current_workspace')
      : get<{ workspace: Workspace | null; active: boolean }>('/workspaces/current'),
  get: (id: string) =>
    isTauri()
      ? apiInvoke<{ workspace: Workspace }>('get_workspace', { id }).then((r) => r.workspace)
      : get<Workspace>(`/workspaces/${id}`),
  switch: (id: string) =>
    isTauri()
      ? apiInvoke<{ success: boolean; workspace: Workspace }>('switch_workspace', { id })
      : post<{ success: boolean; workspace: Workspace }>(`/workspaces/${id}/switch`, {}),
  delete: (id: string) =>
    isTauri()
      ? apiInvoke<{ success: boolean }>('delete_workspace', { id })
      : del<{ success: boolean }>(`/workspaces/${id}`),
  linkProject: (id: string, path: string) =>
    isTauri()
      ? apiInvoke<{ success: boolean; workspace: Workspace }>('link_project', { id, path })
      : post<{ success: boolean; workspace: Workspace }>(`/workspaces/${id}/link`, { path }),
  defaultRoot: (name: string) =>
    isTauri()
      ? apiInvoke<{ default_root: string }>('get_default_root', { name })
      : get<{ default_root: string }>(`/workspaces/default-root/${encodeURIComponent(name)}`),
};

// ── Evolution API (自进化) ─────────────────────────────────────────

export const evolutionApi = {
  trajectories: (date?: string) =>
    isTauri()
      ? apiInvoke<{ trajectories: TrajectoryEntry[]; count: number }>('get_trajectories', { date })
      : get<{ trajectories: TrajectoryEntry[]; count: number }>(
          `/evolution/trajectories${date ? `?date=${date}` : ''}`
        ),
  trajectoryStats: () =>
    isTauri()
      ? apiInvoke<{ stats: TrajectoryStats }>('get_trajectory_stats')
      : get<{ stats: TrajectoryStats }>('/evolution/trajectories/stats'),
  review: (runId?: string) =>
    isTauri()
      ? apiInvoke<{
          success: boolean;
          run_id: string;
          actions: string[];
          nothing_to_save: boolean;
          error?: string | null;
        }>('review_trajectory', { trajectory_id: runId })
      : post<{
          success: boolean;
          run_id: string;
          actions: string[];
          nothing_to_save: boolean;
          error?: string | null;
        }>('/evolution/review', { run_id: runId }),
  curator: (action: string, skillName?: string) =>
    isTauri()
      ? apiInvoke<{
          success: boolean;
          status?: CuratorStatus;
          transitions?: CuratorTransition[];
          count?: number;
          pinned?: string;
          unpinned?: string;
          error?: string;
        }>('curator_action', { action, skill_name: skillName })
      : post<{
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
  list: () =>
    isTauri()
      ? apiInvoke<ProviderListResponse>('list_providers')
      : get<ProviderListResponse>('/providers'),
  test: (req: { provider: string; model: string; api_key?: string; base_url?: string }) =>
    isTauri()
      ? apiInvoke<TestConnectionResponse>('test_connection', req)
      : post<TestConnectionResponse>('/providers/test', req),
  switch: (req: {
    model: string;
    api_key?: string;
    base_url?: string;
    provider?: string;
    temperature?: number;
    max_tokens?: number;
  }) =>
    isTauri()
      ? apiInvoke<SwitchModelResponse>('switch_model', req)
      : post<SwitchModelResponse>('/providers/switch', req),
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
  list: () => (isTauri() ? apiInvoke<PluginInfo[]>('list_plugins') : get<PluginInfo[]>('/plugins')),
  get: (name: string) =>
    isTauri()
      ? apiInvoke<{ info: PluginInfo; resolved?: Record<string, any> }>('get_plugin', { name })
      : get<{ info: PluginInfo; resolved?: Record<string, any> }>(`/plugins/${name}`),
  install: (req: { source: string; scope?: string }) =>
    isTauri()
      ? apiInvoke<{ success: boolean; plugin_id?: string; info?: PluginInfo; error?: string }>(
          'install_plugin',
          { source: req.source, scope: req.scope }
        )
      : post<{ success: boolean; plugin_id?: string; info?: PluginInfo; error?: string }>(
          '/plugins/install',
          req
        ),
  uninstall: (req: { name: string; keep_data?: boolean }) =>
    isTauri()
      ? apiInvoke<{ success: boolean; message?: string; error?: string }>('uninstall_plugin', {
          name: req.name,
          keep_data: req.keep_data,
        })
      : post<{ success: boolean; message?: string; error?: string }>('/plugins/uninstall', req),
  enable: (name: string) =>
    isTauri()
      ? apiInvoke<{ success: boolean; message?: string; error?: string }>('enable_plugin', { name })
      : post<{ success: boolean; message?: string; error?: string }>(`/plugins/${name}/enable`),
  disable: (name: string) =>
    isTauri()
      ? apiInvoke<{ success: boolean; message?: string; error?: string }>('disable_plugin', {
          name,
        })
      : post<{ success: boolean; message?: string; error?: string }>(`/plugins/${name}/disable`),
  reload: () =>
    isTauri()
      ? apiInvoke<{
          success: boolean;
          total?: number;
          enabled?: number;
          message?: string;
          error?: string;
        }>('reload_plugins')
      : post<{
          success: boolean;
          total?: number;
          enabled?: number;
          message?: string;
          error?: string;
        }>('/plugins/reload'),
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
  list: () =>
    isTauri()
      ? apiInvoke<SchedulerTask[]>('list_scheduler_tasks')
      : get<SchedulerTask[]>('/scheduler/tasks'),
  create: (data: { name: string; cron_expr: string; prompt: string }) =>
    isTauri()
      ? apiInvoke<{ success: boolean }>('add_scheduler_task', {
          name: data.name,
          cron_expr: data.cron_expr,
          prompt: data.prompt,
        })
      : post<{ success: boolean }>('/scheduler/tasks', data),
  updateStatus: (id: string, enabled: boolean) =>
    isTauri()
      ? apiInvoke<{ success: boolean }>('set_scheduler_task_status', {
          id,
          status: enabled ? 'enabled' : 'disabled',
        })
      : put<{ success: boolean }>(`/scheduler/tasks/${id}/status`, {
          status: enabled ? 'enabled' : 'disabled',
        }),
  run: (id: string) =>
    isTauri()
      ? apiInvoke<{ success: boolean; result?: string; error?: string }>('run_scheduler_task', {
          id,
        })
      : post<{ success: boolean; result?: string; error?: string }>(`/scheduler/tasks/${id}/run`),
  delete: (id: string) =>
    isTauri()
      ? apiInvoke<{ success: boolean }>('remove_scheduler_task', { id })
      : del<{ success: boolean }>(`/scheduler/tasks/${id}`),
};

// ── Auto Memory API (自动记忆) ───────────────────────────────────

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
  list: () =>
    isTauri()
      ? apiInvoke<HumanGateCheckpoint[]>('list_human_gates')
      : get<HumanGateCheckpoint[]>('/tasks/checkpoints'),
  respond: (taskId: string, selection: string, instructions?: string) =>
    isTauri()
      ? apiInvoke<{ success: boolean }>('respond_human_gate', {
          gate_id: taskId,
          response: selection,
          instructions,
        })
      : post<{ success: boolean }>(`/tasks/${taskId}/respond`, { selection, instructions }),
};

// ── Worktree API (Git 工作树) ─────────────────────────────────────

export interface WorktreeInfo {
  path: string;
  branch: string;
  managed: boolean;
  head: string;
}

export const worktreeApi = {
  list: () =>
    isTauri() ? apiInvoke<WorktreeInfo[]>('list_worktrees') : get<WorktreeInfo[]>('/worktrees'),
  create: (req: { branch: string; base?: string }) =>
    isTauri()
      ? apiInvoke<WorktreeInfo>('create_worktree', { branch: req.branch, path: req.base })
      : post<WorktreeInfo>('/worktrees', req),
  remove: (branch: string) =>
    isTauri()
      ? apiInvoke<{ success: boolean }>('remove_worktree', { path: branch })
      : del<{ success: boolean }>(`/worktrees?branch=${encodeURIComponent(branch)}`),
};
