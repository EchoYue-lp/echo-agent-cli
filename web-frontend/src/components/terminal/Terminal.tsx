import { useRef, useEffect, useState } from 'react';
import { isTauri, apiInvoke } from '../../lib/tauri-bridge';

interface TerminalProps {
  sessionId: string;
}

// Base64 encode/decode helpers (works in both browser and node)
function b64Encode(str: string): string {
  // Use TextEncoder for proper UTF-8 handling
  const bytes = new TextEncoder().encode(str);
  let binary = '';
  for (const b of bytes) binary += String.fromCharCode(b);
  return btoa(binary);
}

function b64Decode(b64: string): string {
  const binary = atob(b64);
  const bytes = new Uint8Array(binary.length);
  for (let i = 0; i < binary.length; i++) bytes[i] = binary.charCodeAt(i);
  return new TextDecoder().decode(bytes);
}

export function Terminal({ sessionId }: TerminalProps) {
  const containerRef = useRef<HTMLDivElement>(null);
  const termRef = useRef<import('@xterm/xterm').Terminal | null>(null);
  const fitAddonRef = useRef<import('@xterm/addon-fit').FitAddon | null>(null);
  const [connected, setConnected] = useState(false);
  const isTauriMode = isTauri();

  // ── Tauri mode: PTY-backed terminal via IPC + xterm.js ──

  useEffect(() => {
    if (!isTauriMode) return;
    if (!containerRef.current) return;

    let disposed = false;
    let unlistenOutput: (() => void) | undefined;
    let unlistenExit: (() => void) | undefined;
    let observer: ResizeObserver | undefined;
    let handleClick: (() => void) | undefined;
    const cleanupResources = () => {
      unlistenOutput?.();
      unlistenOutput = undefined;
      unlistenExit?.();
      unlistenExit = undefined;
      observer?.disconnect();
      observer = undefined;
      if (containerRef.current && handleClick) {
        containerRef.current.removeEventListener('click', handleClick);
      }
      handleClick = undefined;
      if (termRef.current) {
        termRef.current.dispose();
        termRef.current = null;
      }
      fitAddonRef.current = null;
    };

    // Dynamic import xterm.js (avoids SSR issues)
    (async () => {
      const [{ Terminal: XTerm }, { FitAddon }, { listen }] = await Promise.all([
        import('@xterm/xterm'),
        import('@xterm/addon-fit'),
        import('@tauri-apps/api/event'),
      ]);

      if (disposed || !containerRef.current) return;

      // Import CSS
      await import('@xterm/xterm/css/xterm.css');

      const fitAddon = new FitAddon();
      const term = new XTerm({
        cursorBlink: true,
        fontSize: 13,
        fontFamily: '"JetBrains Mono", "Fira Code", "Cascadia Code", Menlo, Monaco, monospace',
        theme: {
          background: '#0a0a0a',
          foreground: '#e5e5e5',
          cursor: '#e5e5e5',
          selectionBackground: '#264f78',
          black: '#1a1a1a',
          red: '#f87171',
          green: '#4ade80',
          yellow: '#facc15',
          blue: '#60a5fa',
          magenta: '#c084fc',
          cyan: '#22d3ee',
          white: '#e5e5e5',
          brightBlack: '#525252',
          brightRed: '#fca5a5',
          brightGreen: '#86efac',
          brightYellow: '#fde047',
          brightBlue: '#93c5fd',
          brightMagenta: '#d8b4fe',
          brightCyan: '#67e8f9',
          brightWhite: '#fafafa',
        },
        allowProposedApi: true,
      });

      term.loadAddon(fitAddon);
      term.open(containerRef.current);
      fitAddon.fit();

      termRef.current = term;
      fitAddonRef.current = fitAddon;
      setConnected(true);

      // ── Listen for PTY output from Rust backend ──
      unlistenOutput = await listen<{ id: string; data: string }>('terminal-output', (event) => {
        if (event.payload.id === sessionId) {
          const text = b64Decode(event.payload.data);
          term.write(text);
        }
      });

      // ── Listen for process exit ──
      unlistenExit = await listen<{ id: string }>('terminal-exit', (event) => {
        if (event.payload.id === sessionId) {
          term.write('\r\n\x1b[90m[Process exited]\x1b[0m\r\n');
          setConnected(false);
        }
      });

      // ── Send user input to PTY ──
      term.onData((data: string) => {
        apiInvoke('write_terminal', {
          id: sessionId,
          data: b64Encode(data),
        }).catch((e: unknown) => {
          console.error('[Terminal] write_terminal failed:', e);
          term.write(`\r\n\x1b[31m[Write error: ${e}]\x1b[0m\r\n`);
        });
      });

      // ── Handle resize ──
      term.onResize(({ rows, cols }: { rows: number; cols: number }) => {
        apiInvoke('resize_terminal', {
          id: sessionId,
          rows,
          cols,
        }).catch((e: unknown) => {
          console.warn('Failed to resize terminal:', e);
        });
      });

      // Trigger initial resize to set correct dimensions
      const { rows, cols } = fitAddon.proposeDimensions() ?? { rows: 24, cols: 80 };
      term.resize(cols, rows);

      // ── ResizeObserver for container size changes ──
      observer = new ResizeObserver(() => {
        if (fitAddonRef.current) {
          try {
            fitAddonRef.current.fit();
          } catch {
            // ignore fit errors during rapid resize
          }
        }
      });
      observer.observe(containerRef.current);

      // Focus terminal on click
      handleClick = () => term.focus();
      containerRef.current.addEventListener('click', handleClick);
      term.focus();

      if (disposed) {
        cleanupResources();
      }
    })().catch((e: unknown) => {
      console.error('Failed to initialize terminal:', e);
    });

    return () => {
      disposed = true;
      cleanupResources();
    };
  }, [sessionId, isTauriMode]);

  // ── Web mode: WebSocket-backed terminal ──

  useEffect(() => {
    if (isTauriMode) return;
    if (!containerRef.current) return;

    let disposed = false;
    let ws: WebSocket | null = null;

    (async () => {
      const [{ Terminal: XTerm }, { FitAddon }] = await Promise.all([
        import('@xterm/xterm'),
        import('@xterm/addon-fit'),
      ]);

      if (disposed || !containerRef.current) return;
      await import('@xterm/xterm/css/xterm.css');

      const fitAddon = new FitAddon();
      const term = new XTerm({
        cursorBlink: true,
        fontSize: 13,
        fontFamily: '"JetBrains Mono", "Fira Code", Menlo, Monaco, monospace',
        theme: {
          background: '#0a0a0a',
          foreground: '#e5e5e5',
          cursor: '#e5e5e5',
        },
      });

      term.loadAddon(fitAddon);
      term.open(containerRef.current);
      fitAddon.fit();
      termRef.current = term;
      fitAddonRef.current = fitAddon;

      // Connect WebSocket
      const protocol = window.location.protocol === 'https:' ? 'wss:' : 'ws:';
      const wsUrl = `${protocol}//${window.location.host}/api/terminal/${sessionId}/ws`;
      ws = new WebSocket(wsUrl);

      ws.onopen = () => setConnected(true);
      ws.onclose = () => setConnected(false);
      ws.onerror = () => setConnected(false);
      ws.onmessage = (event) => {
        try {
          const msg = JSON.parse(event.data);
          if (msg.type === 'output') term.write(msg.data);
        } catch {
          /* ignore */
        }
      };

      term.onData((data: string) => {
        if (ws?.readyState === WebSocket.OPEN) {
          ws.send(JSON.stringify({ type: 'input', data }));
        }
      });

      term.focus();
    })();

    return () => {
      disposed = true;
      ws?.close();
      if (termRef.current) {
        termRef.current.dispose();
        termRef.current = null;
      }
    };
  }, [sessionId, isTauriMode]);

  return (
    <div className="flex flex-col h-full bg-[#0a0a0a] rounded-lg overflow-hidden">
      {/* Status bar */}
      <div className="flex items-center justify-between px-3 py-1 border-b border-gray-800">
        <span className="text-[10px] text-gray-500 font-mono">EKO Terminal</span>
        <span className={`text-[10px] ${connected ? 'text-green-500' : 'text-red-500'}`}>
          {connected ? '● Connected' : '○ Disconnected'}
        </span>
      </div>
      {/* xterm.js container — tabIndex required for keyboard focus in Tauri WebView */}
      <div
        ref={containerRef}
        role="textbox"
        aria-label="终端"
        aria-multiline="true"
        className="flex-1 min-h-0 p-1"
        tabIndex={0}
        style={{ outline: 'none' }}
      />
    </div>
  );
}
