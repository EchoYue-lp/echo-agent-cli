import { useEffect, useRef, useState } from 'react';
import { listen } from '@tauri-apps/api/event';
import {
  terminalApi,
  type TerminalCreateOptions,
  type TerminalSession,
  type TerminalTarget,
  type ProductDataScope,
} from '../../api/endpoints';
import { errorMessage, isTauri } from '../../lib/tauri-bridge';
import { useToastStore } from '../../stores/toastStore';
import {
  boundedTerminalQueue,
  decodeTerminalBytes,
  encodeTerminalBytes,
  INPUT_QUEUE_BYTES,
  OUTPUT_QUEUE_BYTES,
  observeTerminal,
} from './terminal-io';

export type TerminalPhase = 'preparing' | 'starting' | 'running' | 'exited' | 'closing' | 'error';
export interface TerminalViewState {
  phase: TerminalPhase;
  message?: string;
  session?: TerminalSession;
}
interface TerminalProps {
  sessionId: string;
  target?: TerminalTarget;
  visible?: boolean;
  closeRequest?: number;
  onStateChange?: (id: string, state: TerminalViewState) => void;
  onClosed?: (id: string) => void;
  /** Original owner starts the process; this component only renders its PTY. */
  createSession?: (options: TerminalCreateOptions) => Promise<TerminalSession>;
  /** Project authentication has a separate owned close/handoff lifecycle. */
  retainOnUnmount?: boolean;
  projectScope?: ProductDataScope;
}
const LOCAL_TARGET: TerminalTarget = { kind: 'local' };
interface ExitPayload {
  id: string;
  reason: string;
  exit_code: number;
  signal?: string | null;
  error?: string | null;
}

export function Terminal({
  sessionId,
  target = LOCAL_TARGET,
  visible = true,
  closeRequest = 0,
  onStateChange,
  onClosed,
  createSession,
  retainOnUnmount = false,
  projectScope,
}: TerminalProps) {
  const container = useRef<HTMLDivElement>(null);
  const visibleRef = useRef(visible);
  visibleRef.current = visible;
  const fitVisible = useRef<(() => void) | undefined>(undefined);
  const requestClose = useRef<(() => Promise<void>) | undefined>(undefined);
  const [state, setState] = useState<TerminalViewState>({ phase: 'preparing' });
  const [issue, setIssue] = useState('');
  const desktop = isTauri();
  const retainRef = useRef(retainOnUnmount);
  retainRef.current = retainOnUnmount;

  useEffect(() => {
    const element = container.current;
    if (!element) return;
    let disposed = false;
    let resourcesDisposed = false;
    let closeWanted = false;
    let exit: ExitPayload | undefined;
    let creation: Promise<TerminalSession> | undefined;
    let backendId = sessionId;
    let nativeClose: ReturnType<typeof terminalApi.close> | undefined;
    let closeAttempt: Promise<void> | undefined;
    let term: import('@xterm/xterm').Terminal | undefined;
    let observer: ResizeObserver | undefined;
    let socket: WebSocket | undefined;
    let input: ReturnType<typeof boundedTerminalQueue> | undefined;
    let output: ReturnType<typeof boundedTerminalQueue> | undefined;
    const unlisten: Array<() => void> = [];
    const disposables: Array<{ dispose(): void }> = [];
    const report = (next: TerminalViewState) => {
      if (!disposed) setState(next);
      onStateChange?.(sessionId, next);
    };
    const reportIssue = (message: string) => {
      if (!disposed) setIssue(message);
    };
    const cleanup = () => {
      if (resourcesDisposed) return;
      resourcesDisposed = true;
      for (const release of unlisten.splice(0)) release();
      observer?.disconnect();
      for (const disposable of disposables.splice(0)) disposable.dispose();
      input?.dispose();
      output?.dispose();
      socket?.close();
      term?.dispose();
      if (fitVisible.current === fit) fitVisible.current = undefined;
    };
    const fit = () => {
      if (!visibleRef.current || disposed || resourcesDisposed) return;
      try {
        fitAddon?.fit();
        term?.focus();
      } catch {
        reportIssue('终端尺寸暂不可用；会话仍保留。');
      }
    };
    let fitAddon: import('@xterm/addon-fit').FitAddon | undefined;
    const close = (): Promise<void> => {
      closeWanted = true;
      input?.dispose();
      if (closeAttempt) return closeAttempt;
      closeAttempt = (async () => {
        if (!creation) {
          cleanup();
          onClosed?.(sessionId);
          return;
        }
        report({
          phase: 'closing',
          message: '等待原本地进程与输出读者结算；不证明远端副作用停止。',
        });
        try {
          // Observe the original native Promise. AbortSignal cannot cancel IPC.
          // A rejected create may retain native cleanup. A pending create must
          // not be interpreted as absence while it can still spawn.
          await observeTerminal(
            creation.then(
              () => undefined,
              () => undefined
            )
          );
          if (!exit) {
            if (!nativeClose) {
              const original = terminalApi.close(backendId);
              nativeClose = original;
              void original.then(
                () => {
                  nativeClose = undefined;
                },
                () => {
                  nativeClose = undefined;
                }
              );
            }
            const closed = await observeTerminal(nativeClose);
            if (!closed.success) throw new Error('终端关闭未确认；原会话和目标保留。');
            // closed=false is explicit current-service absence after create settled,
            // not a claim that a remote process group has been terminated.
          }
          cleanup();
          onClosed?.(sessionId);
        } catch (error) {
          const message = `关闭尚未确认，可重试：${errorMessage(error)}`;
          report({ phase: 'error', message });
          if (disposed) useToastStore.getState().addToast('error', `终端 ${sessionId} ${message}`);
        } finally {
          closeAttempt = undefined;
        }
      })();
      return closeAttempt;
    };
    requestClose.current = close;
    fitVisible.current = fit;

    void (async () => {
      const [{ Terminal: XTerm }, { FitAddon }] = await Promise.all([
        import('@xterm/xterm'),
        import('@xterm/addon-fit'),
      ]);
      if (disposed || closeWanted) return;
      await import('@xterm/xterm/css/xterm.css');
      if (disposed || closeWanted) return;
      fitAddon = new FitAddon();
      term = new XTerm({
        cursorBlink: true,
        fontSize: 13,
        scrollback: 2000,
        fontFamily: '"JetBrains Mono", "Fira Code", Menlo, monospace',
        theme: { background: '#0a0a0a', foreground: '#e5e5e5', cursor: '#e5e5e5' },
      });
      term.loadAddon(fitAddon);
      term.open(element);
      fit();
      const currentTerm = term;
      output = boundedTerminalQueue(
        (bytes) => new Promise<void>((resolve) => currentTerm.write(bytes, resolve)),
        reportIssue,
        OUTPUT_QUEUE_BYTES,
        '输出队列超限，已停止渲染后续输出；显示存在缺口，会话仍保留。'
      );
      const legacyInputDecoder = new TextDecoder();
      input = boundedTerminalQueue(
        async (bytes) => {
          if (!creation) throw new Error('process not started');
          await creation;
          if (closeWanted) throw new Error('terminal is closing');
          if (desktop) await terminalApi.write(sessionId, encodeTerminalBytes(bytes));
          else {
            if (socket?.readyState !== WebSocket.OPEN)
              throw new Error('terminal socket is not open');
            socket.send(
              JSON.stringify({
                type: 'input',
                data: legacyInputDecoder.decode(bytes, { stream: true }),
              })
            );
          }
        },
        reportIssue,
        INPUT_QUEUE_BYTES,
        '输入队列超限，已停止后续输入并丢弃未发送队列；在途输入可能已处理，不自动重放。'
      );
      const onExit = (payload: ExitPayload) => {
        if (payload.id !== sessionId) return;
        exit = payload;
        input?.dispose();
        report({
          phase: 'exited',
          message: `本地进程退出 ${payload.exit_code}${payload.signal ? ` (${payload.signal})` : ''}${payload.error ? `：${payload.error}` : ''}；远端副作用状态未被证明。`,
        });
      };
      if (desktop) {
        if (disposed || closeWanted) return;
        const subscribe = async <T,>(name: string, receive: (payload: T) => void) => {
          const release = await listen<T>(name, (event) => {
            if (!resourcesDisposed) receive(event.payload);
          });
          if (disposed || resourcesDisposed || closeWanted) release();
          else unlisten.push(release);
        };
        await Promise.all([
          subscribe<{ id: string; data: string }>('terminal-output', (payload) => {
            if (payload.id === sessionId) output?.push(decodeTerminalBytes(payload.data));
          }),
          subscribe<ExitPayload>('terminal-exit', onExit),
          subscribe<{ id: string; message: string }>('terminal-issue', (payload) => {
            if (payload.id === sessionId) {
              reportIssue(payload.message);
              report({ phase: 'error', message: payload.message });
            }
          }),
          subscribe<{ skipped_events: number }>('terminal-gap', (payload) =>
            reportIssue(
              `终端传输遗漏 ${payload.skipped_events} 条聚合事件；本屏幕可能不完整，不支持历史重放。`
            )
          ),
        ]);
        if (disposed || closeWanted || resourcesDisposed) return;
      } else if (target.kind === 'ssh') {
        throw new Error('SSH 终端仅支持 Eclip 桌面应用');
      }
      let pendingSize: { rows: number; cols: number } | undefined;
      let resizing = false;
      const flushResize = async () => {
        if (resizing || !creation || !desktop) return;
        resizing = true;
        try {
          await creation;
          while (pendingSize && !closeWanted && !exit && !disposed) {
            const size = pendingSize;
            pendingSize = undefined;
            await terminalApi.resize(sessionId, size.rows, size.cols);
          }
        } catch {
          reportIssue('终端尺寸更新失败；原会话保留。');
        } finally {
          resizing = false;
        }
      };
      disposables.push(
        currentTerm.onData((data) => {
          if (creation && !closeWanted && !exit) input?.push(new TextEncoder().encode(data));
        })
      );
      disposables.push(
        currentTerm.onResize((size) => {
          if (visibleRef.current && size.rows > 0 && size.cols > 0) {
            pendingSize = size;
            void flushResize();
          }
        })
      );
      observer = new ResizeObserver(fit);
      observer.observe(element);
      report({
        phase: 'starting',
        message: '正在启动本地进程；SSH 认证由终端中的系统 OpenSSH 处理。',
      });
      const size = fitAddon.proposeDimensions() ?? { rows: 24, cols: 80 };
      creation = (createSession ?? terminalApi.create)({
        id: sessionId,
        target: projectScope ? undefined : target,
        projectWorkspaceId: projectScope?.workspaceId,
        projectWorkspaceGeneration: projectScope?.workspaceGeneration,
        rows: Math.max(1, size.rows),
        cols: Math.max(1, size.cols),
      });
      const info = await creation;
      if (!desktop) backendId = info.id;
      if (!exit && !closeWanted && !disposed)
        report({
          phase: 'running',
          session: info,
          message:
            info.target.kind === 'ssh'
              ? 'SSH 本地进程已启动；不代表远端认证成功。'
              : '本地终端进程已启动。',
        });
      if (disposed || closeWanted) {
        if (closeWanted || !retainRef.current) void close();
        return;
      }
      if (!desktop) {
        socket = new WebSocket(terminalApi.wsUrl(info.id));
        socket.onmessage = (event) => {
          try {
            const payload = JSON.parse(event.data) as { type: string; data?: string };
            if (payload.type === 'output' && typeof payload.data === 'string')
              output?.push(new TextEncoder().encode(payload.data));
          } catch {
            reportIssue('终端传输格式无效；输出可能存在缺口。');
          }
        };
        socket.onclose = () =>
          report({ phase: 'error', message: '终端传输已断开；不证明本地进程退出。' });
        socket.onerror = () => reportIssue('终端传输失败；不自动重连或重放。');
      }
      void flushResize();
    })().catch((error) => {
      if (!disposed && !closeWanted) report({ phase: 'error', message: errorMessage(error) });
      cleanup();
    });
    return () => {
      disposed = true;
      cleanup();
      if (requestClose.current === close) requestClose.current = undefined;
      // Keep observation of a dispatched create even after React teardown.
      if (creation && !exit && !retainRef.current) void close();
    };
  }, [sessionId, target, desktop, onStateChange, onClosed, createSession, projectScope]);

  useEffect(() => {
    if (visible) fitVisible.current?.();
  }, [visible]);
  useEffect(() => {
    if (closeRequest > 0) void requestClose.current?.();
  }, [closeRequest]);

  return (
    <div className="flex h-full flex-col overflow-hidden rounded-lg bg-[#0a0a0a]">
      <div className="flex items-center justify-between border-b border-gray-800 px-3 py-1 text-[10px] text-gray-400">
        <span className="font-mono">Eclip Terminal</span>
        <span>{state.message ?? state.phase}</span>
      </div>
      {issue && (
        <div role="alert" className="border-b border-amber-700 px-3 py-1 text-xs text-amber-300">
          {issue}
        </div>
      )}
      {state.phase === 'error' && (
        <div role="alert" className="px-3 py-1 text-xs text-red-300">
          {state.message}
        </div>
      )}
      <div
        ref={container}
        role="textbox"
        aria-label="终端"
        aria-multiline="true"
        className="min-h-0 flex-1 p-1"
        tabIndex={0}
        onFocus={(event) => {
          // Tab 到外层可访问文本框时，把实际键盘焦点交给 xterm 的输入区。
          if (event.target === event.currentTarget) fitVisible.current?.();
        }}
        style={{ outline: 'none' }}
      />
    </div>
  );
}
