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
} from '../types/api';

export const sessionApi = {
  get: () => get<SessionInfo>('/session'),
  reset: () => post<SessionInfo>('/session/reset'),
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
  updateFull: (cfg: Partial<FullConfigUpdateRequest>) => put<FullConfigResponse>('/config/full', cfg),
};

export const permissionsApi = {
  getMode: () => get<{ mode: string }>('/permissions/mode'),
  setMode: (mode: string) => put<{ success: boolean }>('/permissions/mode', { mode }),
  listRules: () => get<PermissionRule[]>('/permissions/rules'),
  addRule: (rule: Omit<PermissionRule, 'priority'>) => post<PermissionRule>('/permissions/rules', rule),
  removeRule: (name: string) => del<{ success: boolean }>(`/permissions/rules/${name}`),
};

export const auditApi = {
  logs: () => get<AuditLog[]>('/audit/logs'),
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
