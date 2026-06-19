import type { LoginResponse, HealthResponse } from '../types/api';
import { useToastStore } from '../stores/toastStore';
import { isTauri } from '../lib/tauri-bridge';

const BASE = '/api';

// 令牌管理
const TOKEN_KEY = 'jwt_token';
const TOKEN_TYPE_KEY = 'token_type';
const EXPIRES_AT_KEY = 'expires_at';

export function getToken(): string | null {
  // sessionStorage: cleared when the browser/tab closes, reducing the
  // window for XSS-based token theft compared to localStorage (P1-37).
  return sessionStorage.getItem(TOKEN_KEY);
}

export function getTokenType(): string | null {
  return sessionStorage.getItem(TOKEN_TYPE_KEY) || 'Bearer';
}

export function getExpiresAt(): number | null {
  const expiresAt = sessionStorage.getItem(EXPIRES_AT_KEY);
  return expiresAt ? parseInt(expiresAt, 10) : null;
}

export function setToken(token: string, tokenType: string = 'Bearer', expiresIn?: number): void {
  sessionStorage.setItem(TOKEN_KEY, token);
  sessionStorage.setItem(TOKEN_TYPE_KEY, tokenType);

  if (expiresIn) {
    const expiresAt = Date.now() + expiresIn * 1000;
    sessionStorage.setItem(EXPIRES_AT_KEY, expiresAt.toString());
  } else {
    sessionStorage.removeItem(EXPIRES_AT_KEY);
  }
}

export function clearToken(): void {
  sessionStorage.removeItem(TOKEN_KEY);
  sessionStorage.removeItem(TOKEN_TYPE_KEY);
  sessionStorage.removeItem(EXPIRES_AT_KEY);
}

export function isTokenExpired(): boolean {
  const expiresAt = getExpiresAt();
  if (!expiresAt) return false;
  return Date.now() >= expiresAt;
}

export function isAuthenticated(): boolean {
  const token = getToken();
  if (!token) return false;
  return !isTokenExpired();
}

// 带认证的请求函数
async function request<T>(path: string, opts?: RequestInit): Promise<T> {
  if (isTauri()) {
    throw new Error(`HTTP API fallback is disabled in Tauri mode: ${path}`);
  }

  const headers: Record<string, string> = {
    'Content-Type': 'application/json',
  };

  // 添加认证头（如果令牌存在且未过期）
  if (isAuthenticated()) {
    const token = getToken();
    const tokenType = getTokenType();
    if (token) {
      headers['Authorization'] = `${tokenType} ${token}`;
    }
  }

  const res = await fetch(`${BASE}${path}`, {
    headers,
    ...opts,
  });

  // 处理401未授权响应（令牌可能过期）
  if (res.status === 401) {
    clearToken();
    useToastStore.getState().addToast('error', '认证已过期，请重新登录');
    throw new Error('认证已过期，请重新登录');
  }

  if (!res.ok) {
    const text = await res.text();
    const message = `API ${res.status}: ${text}`;
    useToastStore.getState().addToast('error', message);
    throw new Error(message);
  }

  return res.json();
}

// 公开的HTTP方法
export function get<T>(path: string): Promise<T> {
  return request<T>(path);
}

export function post<T>(path: string, body?: unknown): Promise<T> {
  return request<T>(path, {
    method: 'POST',
    body: body ? JSON.stringify(body) : undefined,
  });
}

export function put<T>(path: string, body?: unknown, signal?: AbortSignal): Promise<T> {
  return request<T>(path, {
    method: 'PUT',
    body: body ? JSON.stringify(body) : undefined,
    signal,
  });
}

export function del<T>(path: string): Promise<T> {
  return request<T>(path, { method: 'DELETE' });
}

// 认证相关API
export async function login(username: string, password: string) {
  const response = await post<LoginResponse>('/auth/login', { username, password });
  setToken(response.token, response.token_type, response.expires_in);
  return response;
}

export async function logout() {
  clearToken();
}

export async function checkHealth() {
  return get<HealthResponse>('/health');
}
