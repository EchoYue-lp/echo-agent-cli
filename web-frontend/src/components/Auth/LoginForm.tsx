import { useState } from 'react';
import { Sparkles, AlertCircle } from 'lucide-react';
import { useAuthStore } from '../../stores/authStore';

export function LoginForm() {
  const [username, setUsername] = useState('');
  const [password, setPassword] = useState('');
  const { login, isLoading, error, clearError } = useAuthStore();

  const handleSubmit = async (e: React.FormEvent) => {
    e.preventDefault();
    clearError();

    try {
      await login(username, password);
    } catch (err) {
      console.error('Login failed:', err);
    }
  };

  return (
    <div className="relative flex min-h-screen items-center justify-center overflow-hidden px-4">
      {/* Backdrop */}
      <div className="pointer-events-none fixed inset-0 bg-[var(--bg-chat)]/60" />

      {/* Card */}
      <div className="glass animate-fade-up relative z-10 w-full max-w-sm rounded-xl p-8 shadow-[var(--shadow-xl)]">
        {/* Brand */}
        <div className="mb-8 flex flex-col items-center">
          <div
            className="animate-glow-pulse mb-4 flex h-[64px] w-[64px] items-center justify-center rounded-xl"
            style={{
              background: 'linear-gradient(135deg, var(--accent) 0%, var(--color-purple) 100%)',
            }}
          >
            <Sparkles size={28} color="white" />
          </div>
          <h1 className="text-xl font-semibold tracking-tight text-[var(--text-primary)]">
            EKO
          </h1>
          <p className="mt-1 text-sm text-[var(--text-secondary)]">登录以继续使用</p>
        </div>

        {/* Form */}
        <form onSubmit={handleSubmit} className="space-y-4">
          <div className="space-y-3">
            <input
              id="username"
              name="username"
              type="text"
              autoComplete="username"
              required
              value={username}
              onChange={(e) => setUsername(e.target.value)}
              className="input w-full"
              placeholder="用户名"
              disabled={isLoading}
            />
            <input
              id="password"
              name="password"
              type="password"
              autoComplete="current-password"
              required
              value={password}
              onChange={(e) => setPassword(e.target.value)}
              className="input w-full"
              placeholder="密码"
              disabled={isLoading}
            />
          </div>

          {error && (
            <div
              className="animate-fade-in flex items-start gap-2 rounded-lg border p-3 text-sm"
              style={{
                borderColor: 'var(--border-primary)',
                background: 'color-mix(in srgb, var(--accent) 8%, transparent)',
                color: 'var(--text-primary)',
              }}
            >
              <AlertCircle
                size={14}
                className="mt-0.5 shrink-0"
                style={{ color: 'var(--accent)' }}
              />
              <span>{error}</span>
            </div>
          )}

          <button
            type="submit"
            disabled={isLoading}
            className="btn btn-primary w-full justify-center py-2.5"
          >
            {isLoading ? (
              <span className="flex items-center gap-2">
                <span
                  className="spinner h-3.5 w-3.5"
                  style={{ borderColor: 'rgba(255,255,255,0.3)', borderTopColor: 'white' }}
                />
                登录中...
              </span>
            ) : (
              '登录'
            )}
          </button>

        </form>

        <div className="mt-6 rounded-lg border border-[var(--border-primary)] bg-[var(--bg-hover)] p-3">
          <p className="text-xs text-[var(--text-tertiary)] leading-relaxed">
            认证默认禁用。如需启用，请设置环境变量{' '}
            <code
              className="rounded-md px-1 py-0.5 text-[11px]"
              style={{
                background: 'var(--bg-primary)',
                color: 'var(--accent)',
                border: '1px solid var(--border-primary)',
              }}
            >
              AUTH_ENABLED=true
            </code>
          </p>
        </div>
      </div>
    </div>
  );
}
