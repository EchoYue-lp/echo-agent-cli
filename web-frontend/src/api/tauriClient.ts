// Tauri IPC 客户端 — 在 Tauri 环境中替代 HTTP REST API
// 当不在 Tauri 环境中时，回退到 HTTP API

import { invoke } from '@tauri-apps/api/core';
import { listen, type UnlistenFn } from '@tauri-apps/api/event';

/** 检测是否运行在 Tauri 环境中 */
export function isTauri(): boolean {
  return typeof window !== 'undefined' && '__TAURI_INTERNALS__' in window;
}

// ── Chat Streaming (Tauri Events) ──

export interface ChatEvent {
  conversation_id: string;
  type: 'token' | 'think_start' | 'think_end' | 'tool_call' | 'tool_result'
    | 'tool_error' | 'final_answer' | 'cancelled' | 'error' | 'plan'
    | 'step_start' | 'context_compressed' | 'unknown';
  data?: string;
  name?: string;
  args?: unknown;
  output?: string;
  error?: string;
  prompt_tokens?: number;
  completion_tokens?: number;
  steps?: string[];
  step_index?: number;
  description?: string;
  before_count?: number;
  after_count?: number;
  before_tokens?: number;
  after_tokens?: number;
}

export function listenChatEvents(callback: (event: ChatEvent) => void): Promise<UnlistenFn> {
  return listen<ChatEvent>('chat-event', (event) => {
    callback(event.payload);
  });
}

export async function tauriChatStream(message: string, conversationId?: string): Promise<void> {
  await invoke('chat_stream', { message, conversationId });
}

export async function tauriCancelChat(conversationId: string): Promise<void> {
  await invoke('cancel_chat', { conversationId });
}

// ── Config ──

export async function tauriGetConfig() {
  return invoke('get_config');
}

export async function tauriUpdateConfig(params: {
  model?: string;
  systemPrompt?: string;
  maxIterations?: number;
}) {
  return invoke('update_config', params);
}

// ── Context ──

export async function tauriGetContextStats() {
  return invoke('get_context_stats');
}

export async function tauriCompressContext(keepRecent: number) {
  return invoke('compress_context', { keepRecent });
}

// ── Conversations ──

export async function tauriListConversations() {
  return invoke('list_conversations');
}

export async function tauriGetConversation(id: string) {
  return invoke('get_conversation', { id });
}

export async function tauriDeleteConversation(id: string) {
  return invoke('delete_conversation', { id });
}

export async function tauriExportConversation(id: string) {
  return invoke('export_conversation', { id });
}

// ── MCP ──

export async function tauriListMcpServers() {
  return invoke('list_mcp_servers');
}

export async function tauriConnectMcpServer(configJson: string) {
  return invoke('connect_mcp_server', { configJson });
}

export async function tauriDisconnectMcpServer(name: string) {
  return invoke('disconnect_mcp_server', { name });
}

// ── Skills ──

export async function tauriListSkills() {
  return invoke('list_skills');
}

export async function tauriLoadSkills(dirPath: string) {
  return invoke('load_skills', { dirPath });
}

// ── Tools ──

export async function tauriListTools() {
  return invoke('list_tools');
}
