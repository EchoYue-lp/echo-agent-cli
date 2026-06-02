import { Component, type ErrorInfo, type ReactNode } from 'react';
import { AlertTriangle, RefreshCw } from 'lucide-react';

interface Props {
  children: ReactNode;
}

interface State {
  hasError: boolean;
  error: Error | null;
}

export class ErrorBoundary extends Component<Props, State> {
  constructor(props: Props) {
    super(props);
    this.state = { hasError: false, error: null };
  }

  static getDerivedStateFromError(error: Error): State {
    return { hasError: true, error };
  }

  componentDidCatch(error: Error, errorInfo: ErrorInfo) {
    console.error('[ErrorBoundary] Uncaught error:', error, errorInfo);
  }

  handleReset = () => {
    this.setState({ hasError: false, error: null });
  };

  render() {
    if (this.state.hasError) {
      return (
        <div
          style={{
            display: 'flex',
            flexDirection: 'column',
            alignItems: 'center',
            justifyContent: 'center',
            height: '100vh',
            width: '100vw',
            background: 'var(--bg-chat, #f1f5f9)',
            color: 'var(--text-primary, #0f172a)',
            fontFamily: "-apple-system, BlinkMacSystemFont, 'SF Pro Text', sans-serif",
            padding: '2rem',
          }}
        >
          <div
            style={{
              display: 'flex',
              alignItems: 'center',
              justifyContent: 'center',
              width: 56,
              height: 56,
              borderRadius: 14,
              background: 'var(--color-error-bg, #fee2e2)',
              marginBottom: 20,
            }}
          >
            <AlertTriangle size={24} style={{ color: 'var(--color-error, #ef4444)' }} />
          </div>
          <h1 style={{ fontSize: 18, fontWeight: 600, marginBottom: 8 }}>出了一些问题</h1>
          <p
            style={{
              fontSize: 13,
              color: 'var(--text-secondary, #475569)',
              maxWidth: 400,
              textAlign: 'center',
              lineHeight: 1.6,
              marginBottom: 20,
            }}
          >
            应用遇到了意外错误。请尝试刷新页面，或清除浏览器缓存后重试。
          </p>
          {this.state.error && (
            <pre
              style={{
                fontSize: 11,
                fontFamily: "'SF Mono', Menlo, monospace",
                background: 'var(--bg-code, #0f172a)',
                color: 'var(--color-code-text, #e2e8f0)',
                padding: '12px 16px',
                borderRadius: 8,
                maxWidth: 500,
                overflow: 'auto',
                marginBottom: 20,
                lineHeight: 1.5,
              }}
            >
              {this.state.error.message}
            </pre>
          )}
          <div style={{ display: 'flex', gap: 10 }}>
            <button
              onClick={this.handleReset}
              style={{
                display: 'flex',
                alignItems: 'center',
                gap: 6,
                padding: '8px 18px',
                borderRadius: 8,
                border: 'none',
                background: 'var(--accent, #6366f1)',
                color: 'white',
                fontSize: 13,
                fontWeight: 500,
                cursor: 'pointer',
              }}
            >
              <RefreshCw size={14} /> 重试
            </button>
            <button
              onClick={() => window.location.reload()}
              style={{
                padding: '8px 18px',
                borderRadius: 8,
                border: '1px solid var(--border-primary, #e2e8f0)',
                background: 'var(--bg-primary, #fff)',
                color: 'var(--text-primary, #0f172a)',
                fontSize: 13,
                fontWeight: 500,
                cursor: 'pointer',
              }}
            >
              刷新页面
            </button>
          </div>
        </div>
      );
    }

    return this.props.children;
  }
}
