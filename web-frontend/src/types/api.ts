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
  status: 'connected' | 'disconnected' | 'error';
  transport: string;
  tool_count: number;
  tools: McpToolInfo[];
  connected_at: string | null;
  error: string | null;
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
  | { type: 'message'; data: string; attachments?: Attachment[] }
  | { type: 'approval_response'; request_id: string; approved: boolean; reason?: string }
  | { type: 'input_response'; request_id: string; text: string }
  | { type: 'cancel' };

export type ServerMessage =
  | { type: 'token'; data: string }
  | { type: 'tool_start'; name: string; args: unknown }
  | { type: 'tool_result'; name: string; result: string; success: boolean }
  | { type: 'final_answer'; data: string }
  | { type: 'approval_request'; request_id: string; tool_name: string; args: unknown; prompt?: string }
  | { type: 'input_request'; request_id: string; prompt?: string }
  | { type: 'chart'; spec: unknown }
  | { type: 'error'; message: string }
  | { type: 'cancelled' }
  | { type: 'thinking_start' }
  | { type: 'thinking_end'; prompt_tokens: number; completion_tokens: number };

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

// ── Checkpoint types ──

export interface SnapshotInfo {
  id: string;
  iteration: number;
  created_at: number;
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
  id: string;
  conversation_id: string;
  title: string;
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
  security_level: "low" | "medium" | "high";
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
  model: { name: string; max_tokens: number | null; temperature: number | null };
  agent: {
    model: string;
    system_prompt: string;
    max_iterations: number;
    token_limit: number;
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
  model?: { name?: string; max_tokens?: number; temperature?: number };
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
