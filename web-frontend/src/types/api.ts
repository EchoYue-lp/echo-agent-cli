// API 类型定义

export interface ChatRequest {
  message: string;
}

export interface ChatResponse {
  answer: string;
  tool_calls: ToolCallInfo[];
  iterations: number;
  context_stats: ContextStats;
}

export type ChatRunStatus =
  | 'idle'
  | 'running'
  | 'thinking'
  | 'using_tool'
  | 'waiting_approval'
  | 'waiting_input'
  | 'completed'
  | 'failed'
  | 'cancelled';

export interface ToolCallInfo {
  name: string;
  args: unknown;
  result: string;
  success: boolean;
}

export interface ContextStats {
  message_count: number;
  estimated_tokens: number;
}

export interface SessionInfo {
  session_id: string | null;
  message_count: number;
  tool_count: number;
  skill_count: number;
  mcp_server_count: number;
}

export interface ToolInfo {
  name: string;
  description: string;
  source: string;
  input_schema?: Record<string, unknown>;
  enabled: boolean;
}

export interface SkillInfo {
  name: string;
  description: string;
  file: string;
}

export interface McpServerInfo {
  name: string;
  status: 'connected' | 'disconnected' | 'error' | 'disabled';
  transport: string;
  tool_count?: number;
  tools?: McpToolInfo[];
  connected_at?: string | null;
  error?: string | null;
  enabled?: boolean;
}

export interface McpToolInfo {
  name: string;
  description: string;
  input_schema?: Record<string, unknown>;
}

export interface ConnectMcpRequest {
  name: string;
  transport: McpTransportConfig;
}

export type McpTransportConfig =
  | { stdio: { command: string; args?: string[]; env?: Record<string, string> } }
  | { http: { url: string; headers?: Record<string, string> } }
  | { sse: { url: string; headers?: Record<string, string> } };

export interface MemoryEntry {
  namespace: string;
  key: string;
  value: any; // serde_json::Value can be any JSON value
  created_at: number;
  updated_at: number;
  score?: number; // optional relevance score for search results
}

export interface NamespacesResponse {
  namespaces: string[][]; // Vec<Vec<String>> in Rust
}

export interface SnapshotInfo {
  id: string;
  iteration: number;
  created_at: number;
}

export interface PermissionRule {
  name: string;
  tool_pattern: string;
  effect: 'allow' | 'deny' | 'ask';
  priority: number;
}

export interface AuditLog {
  id: string;
  timestamp: string;
  tool_name: string;
  args: unknown;
  decision: string;
  reason?: string;
}

export interface WorkflowInfo {
  id: string;
  name: string;
  definition: string;
  status: string;
  created_at: string;
}

export interface ConfigInfo {
  model: string;
  system_prompt: string;
  max_iterations: number;
  max_tokens: number;
  enable_tools: boolean;
  enable_memory: boolean;
  enable_human_in_loop: boolean;
}

// ── 附件类型 ──

export interface Attachment {
  name: string;
  mime_type: string;
  data: string; // base64 encoded
  size: number;
}

// WebSocket 消息类型
export type ClientMessage =
  | { type: 'message'; id?: string; data: string; attachments?: Attachment[] }
  | {
      type: 'approval_response';
      id?: string;
      request_id: string;
      approved: boolean;
      reason?: string;
    }
  | { type: 'input_response'; id?: string; request_id: string; text: string }
  | {
      type: 'selection_response';
      id?: string;
      request_id: string;
      selection: string;
      instructions?: string;
    }
  | { type: 'cancel'; id?: string }
  | { type: 'ping' };

export type ServerMessage =
  | { type: 'token'; id?: string; data: string }
  | { type: 'tool_start'; id?: string; name: string; args: unknown }
  | { type: 'tool_result'; id?: string; name: string; result: string; success: boolean }
  | { type: 'tool_batch_start'; id?: string; tool_count: number }
  | { type: 'tool_batch_end'; id?: string }
  | { type: 'final_answer'; id?: string; data: string }
  | {
      type: 'approval_request';
      id?: string;
      request_id: string;
      tool_name: string;
      args: unknown;
      prompt?: string;
    }
  | { type: 'input_request'; id?: string; request_id: string; prompt?: string }
  | {
      type: 'selection_request';
      id?: string;
      request_id: string;
      prompt: string;
      options: string[];
      task_id?: string | null;
      context?: unknown;
      phase?: string | null;
    }
  | { type: 'chart'; id?: string; spec: unknown }
  | { type: 'error'; id?: string; message: string }
  | { type: 'cancelled'; id?: string }
  | { type: 'done'; id?: string }
  | { type: 'run_status'; id?: string; status: ChatRunStatus }
  | { type: 'plan_ready'; id?: string; run_id: string }
  | { type: 'thinking_start'; id?: string }
  | { type: 'thinking_end'; id?: string; prompt_tokens: number; completion_tokens: number }
  | { type: 'pong' };

// Execution round: one ReAct loop iteration (think → tools)
export interface ExecutionRound {
  /** Thinking that precedes this round's tools */
  thinking?: { content: string };
  /** Tools executed in this round (parallel if >1) */
  tools: ToolCallInfo[];
}

// Chat store types
export interface ChatMessage {
  id: string;
  role: 'user' | 'assistant';
  content: string;
  thinkingContent?: string; // deprecated, kept for history display
  thinkingSegments?: { content: string }[];
  attachments?: { name: string; mime_type: string; url: string; size: number }[];
  toolCalls?: ToolCallInfo[];
  chartSpecs?: unknown[];
  isStreaming?: boolean;
  timestamp: number;
  /** @deprecated Use executionRounds instead. Flat execution order tracking for backward compat. */
  executionSteps?: { type: 'thinking' | 'tool'; index: number }[];
  /** Execution rounds: each round = one ReAct iteration (thinking + tool batch) */
  executionRounds?: ExecutionRound[];
}

export interface ApprovalRequest {
  requestId: string;
  toolName: string;
  args: unknown;
  prompt?: string;
}

// History types (matching backend MessageItem)
export interface HistoryMessage {
  role: string;
  content: string | null;
  tool_calls?: HistoryToolCall[];
}

export interface HistoryToolCall {
  id: string;
  name: string;
  arguments: string;
}

export interface HistoryResponse {
  messages: HistoryMessage[];
  total: number;
}

// Snapshot metadata for localStorage cache
export interface SnapshotMeta {
  title: string;
  preview: string;
  messageCount: number;
  createdAt: number;
}

// ── Compression types ──

export interface CompressionStats {
  current_tokens: number;
  token_limit: number;
  message_count: number;
  compression_ratio: number;
  needs_compression: boolean;
}

export interface CompressResponse {
  success: boolean;
  messages_before: number;
  messages_after: number;
  tokens_saved: number;
  message: string;
}

// ── Extract types ──

export interface ExtractRequest {
  input: string;
  schema: object;
  schema_name?: string;
}

export interface ExtractResponse {
  success: boolean;
  data: unknown;
  schema_name: string;
}

export interface ValidateSchemaResponse {
  valid: boolean;
  errors: string[];
}

export interface ExtractExample {
  name: string;
  description: string;
  schema: object;
  example_input: string;
}

// ── Conversation persistence types ──

export interface SavedMessage {
  role: string;
  content: string | null;
  tool_calls?: { id: string; name: string; arguments: string }[];
  thinking_segments?: string[];
  execution_steps?: { type: string; index: number }[];
  execution_rounds?: {
    thinking?: { content: string };
    tools: { name: string; args: unknown; result: string; success: boolean }[];
  }[];
  tool_result?: string | null;
}

export interface ConversationRecord {
  id: string;
  conversation_id: string;
  title: string;
  messages: SavedMessage[];
  created_at: string;
  updated_at: string;
}

export interface ConversationListItem {
  id: number;
  conversation_id: string;
  title: string | null;
  message_count: number;
  created_at: string;
  updated_at: string;
}

// ── Sandbox types ──

export interface SandboxStatus {
  local_available: boolean;
  docker_available: boolean;
  k8s_available: boolean;
  current_backend: string;
}

export interface SandboxConfig {
  security_level: 'low' | 'medium' | 'high';
  max_memory_mb: number | null;
  max_cpu_seconds: number | null;
  network_enabled: boolean;
}

export interface SandboxExecuteRequest {
  language: string;
  code: string;
}

export interface SandboxExecuteResult {
  success: boolean;
  stdout: string;
  stderr: string;
  exit_code: number | null;
  duration_ms: number;
}

// ── Context info ── (reuses ContextStats)

// ── Full Config types ──

export interface FullConfigResponse {
  model: {
    provider: string;
    name: string;
    has_auth_token: boolean;
    base_url: string | null;
    max_tokens: number | null;
    temperature: number | null;
  };
  agent: {
    model: string;
    system_prompt: string;
    max_iterations: number;
    token_limit: number;
    enable_tools: boolean;
    enable_memory: boolean;
    enable_human_loop: boolean;
    session_id: string | null;
    available_models: string[];
  };
  mcp: { config_path: string | null };
  channels: {
    qq: { enabled: boolean; app_id: string };
    feishu: { enabled: boolean; app_id: string; mode: string };
    session: {
      timeout_minutes: number;
      reset_keywords: string[];
      reset_commands: string[];
    };
  };
  server: { host: string; port: number };
  logging: { level: string };
}

export interface FullConfigUpdateRequest {
  model?: {
    max_tokens?: number;
    temperature?: number;
  };
  agent?: {
    name?: string;
    system_prompt?: string;
    max_iterations?: number;
    enable_tools?: boolean;
    enable_memory?: boolean;
    enable_human_in_loop?: boolean;
    memory_path?: string;
  };
  mcp?: { config_path?: string };
  channels?: {
    qq?: { enabled?: boolean; app_id?: string; client_secret?: string };
    feishu?: { enabled?: boolean; app_id?: string; app_secret?: string; mode?: string };
    session?: {
      timeout_minutes?: number;
      reset_keywords?: string[];
      reset_commands?: string[];
    };
  };
  server?: { host?: string; port?: number };
  logging?: { level?: string };
}

// ── MCP 配置类型 ──

export interface McpConfig {
  mcpServers: Record<string, McpServerEntry>;
}

export interface McpServerEntry {
  command?: string;
  args?: string[];
  env?: Record<string, string>;
  url?: string;
  headers?: Record<string, string>;
  transport?: string;
  disabled?: boolean;
}

// ── 认证相关类型 ──

export interface LoginRequest {
  username: string;
  password: string;
}

export interface LoginResponse {
  token: string;
  token_type: string;
  expires_in: number;
}

export interface AuthState {
  isAuthenticated: boolean;
  token: string | null;
  tokenType: string | null;
  expiresAt: number | null;
  isLoading: boolean;
  error: string | null;
}

export interface HealthResponse {
  status: string;
  timestamp: string;
}

// ── Evolution / 自进化 types ──

export interface ShareGPTMessage {
  from: string;
  value: string;
}

export interface TrajectoryEntry {
  id: string;
  session_id: string;
  conversations: ShareGPTMessage[];
  model: string;
  completed: boolean;
  timestamp: string;
  token_usage: number;
  tool_call_count: number;
  duration_ms: number;
}

export interface TrajectoryStats {
  total: number;
  completed: number;
  failed: number;
  total_tokens: number;
  total_tool_calls: number;
  avg_duration_ms: number;
}

export interface ReviewOutcome {
  run_id: string;
  actions: string[];
  nothing_to_save: boolean;
  error?: string | null;
}

export interface CuratorStatus {
  total: number;
  active: number;
  stale: number;
  archived: number;
  pinned: number;
  last_run_at: string | null;
}

export interface CuratorTransition {
  skill: string;
  from: string;
  to: string;
}

// ── Provider types ──────────────────────────────────────────────────────────

export interface ProviderTemplate {
  id: string;
  name: string;
  base_url: string;
  api_key_env: string;
  default_models: string[];
  requires_api_key: boolean;
}

export interface ConfiguredModel {
  id: string;
  display_name: string;
  provider: string;
  model: string;
  enabled: boolean;
  is_default: boolean;
  has_auth_token: boolean;
  auth_source: 'config' | 'env' | 'none' | string;
  base_url: string | null;
  temperature: number | null;
  max_tokens: number | null;
  context_window: number | null;
}

export interface ConfiguredModelListResponse {
  models: ConfiguredModel[];
  default_model_id: string | null;
}

export interface TestConnectionResponse {
  success: boolean;
  response?: string;
  error?: string;
  model?: string;
  auth_source?: 'input' | 'config' | 'env' | 'none' | string;
  has_auth_token?: boolean;
}
