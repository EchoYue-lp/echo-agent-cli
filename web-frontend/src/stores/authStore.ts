import { create } from 'zustand';
import type { AuthState } from '../types/api';
import * as api from '../api/client';

interface AuthStore extends AuthState {
  // Actions
  login: (username: string, password: string) => Promise<void>;
  logout: () => void;
  checkAuth: () => boolean;
  initFromStorage: () => void;
  clearError: () => void;
}

const getInitialState = (): AuthState => {
  if (typeof window === 'undefined') {
    return {
      isAuthenticated: false,
      token: null,
      tokenType: null,
      expiresAt: null,
      isLoading: false,
      error: null,
    };
  }

  const token = api.getToken();
  const tokenType = api.getTokenType();
  const expiresAt = api.getExpiresAt();
  const isAuthenticated = api.isAuthenticated();

  return {
    isAuthenticated,
    token,
    tokenType,
    expiresAt,
    isLoading: false,
    error: null,
  };
};

export const useAuthStore = create<AuthStore>((set, get) => ({
  // Initial state
  ...getInitialState(),

  // Initialize from storage on store creation
  initFromStorage: () => {
    set({ ...getInitialState() });
  },

  // Login action
  login: async (username: string, password: string) => {
    set({ isLoading: true, error: null });

    try {
      const response = await api.login(username, password);

      set({
        isAuthenticated: true,
        token: response.token,
        tokenType: response.token_type,
        expiresAt: Date.now() + response.expires_in * 1000,
        isLoading: false,
        error: null,
      });
    } catch (error) {
      set({
        isAuthenticated: false,
        token: null,
        tokenType: null,
        expiresAt: null,
        isLoading: false,
        error: error instanceof Error ? error.message : '登录失败',
      });
      throw error;
    }
  },

  // Logout action
  logout: () => {
    api.logout();
    set({
      isAuthenticated: false,
      token: null,
      tokenType: null,
      expiresAt: null,
      error: null,
    });
  },

  // Check authentication status
  checkAuth: () => {
    const isAuthenticated = api.isAuthenticated();
    if (!isAuthenticated && get().isAuthenticated) {
      // Token expired while app was running
      get().logout();
    }
    return isAuthenticated;
  },

  // Clear error
  clearError: () => {
    set({ error: null });
  },
}));

// Initialize store from localStorage on module load
if (typeof window !== 'undefined') {
  useAuthStore.getState().initFromStorage();

  // Set up periodic auth check (every 5 minutes)
  setInterval(() => {
    useAuthStore.getState().checkAuth();
  }, 5 * 60 * 1000);

  // Also check auth when window regains focus
  window.addEventListener('focus', () => {
    useAuthStore.getState().checkAuth();
  });
}