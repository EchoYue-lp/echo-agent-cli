import { useCallback, useEffect, useRef, useState } from 'react';
import { X, Plus, Minimize2 } from 'lucide-react';
import { useUiStore } from '../../stores/uiStore';
import { Terminal, type TerminalViewState } from './Terminal';
import type { TerminalTarget } from '../../api/endpoints';
import { isTauri } from '../../lib/tauri-bridge';

interface TerminalTab {
  id: string;
  label: string;
  target: TerminalTarget;
  closeRequest: number;
  state: TerminalViewState;
}
function targetLabel(target: TerminalTarget): string {
  return target.kind === 'local'
    ? '本地'
    : `SSH ${target.user ? `${target.user}@` : ''}${target.host}${target.port ? `:${target.port}` : ''}`;
}

function hasControlCharacters(value: string): boolean {
  return Array.from(value).some(
    (character) => character.charCodeAt(0) < 32 || character.charCodeAt(0) === 127
  );
}

export function TerminalDrawer() {
  const terminalOpen = useUiStore((store) => store.terminalOpen);
  const hideTerminal = useUiStore((store) => store.closeTerminal);
  const [tabs, setTabs] = useState<TerminalTab[]>([]);
  const tabRef = useRef(tabs);
  const sequence = useRef(0);
  const [activeId, setActiveId] = useState<string | null>(null);
  const [height, setHeight] = useState(300);
  const [dragging, setDragging] = useState(false);
  const [sshForm, setSshForm] = useState(false);
  const [host, setHost] = useState('');
  const [user, setUser] = useState('');
  const [port, setPort] = useState('');
  const [formError, setFormError] = useState('');
  const updateTabs = useCallback((update: (current: TerminalTab[]) => TerminalTab[]) => {
    const next = update(tabRef.current);
    tabRef.current = next;
    setTabs(next);
  }, []);
  const createTab = useCallback(
    (target: TerminalTarget) => {
      const id = `term-${crypto.randomUUID()}`;
      sequence.current += 1;
      updateTabs((current) => [
        ...current,
        {
          id,
          label: `Terminal ${sequence.current}`,
          target,
          closeRequest: 0,
          state: { phase: 'preparing' },
        },
      ]);
      setActiveId(id);
    },
    [updateTabs]
  );
  const createLocal = useCallback(() => createTab({ kind: 'local' }), [createTab]);
  useEffect(() => {
    if (terminalOpen && tabRef.current.length === 0) createLocal();
  }, [terminalOpen, createLocal]);
  const onState = useCallback(
    (id: string, state: TerminalViewState) => {
      updateTabs((current) => current.map((tab) => (tab.id === id ? { ...tab, state } : tab)));
    },
    [updateTabs]
  );
  const onClosed = useCallback(
    (id: string) => {
      const next = tabRef.current.filter((tab) => tab.id !== id);
      updateTabs(() => next);
      setActiveId((current) => (current === id ? (next.at(-1)?.id ?? null) : current));
      if (next.length === 0) hideTerminal();
    },
    [hideTerminal, updateTabs]
  );
  const closeTab = (id: string) =>
    updateTabs((current) =>
      current.map((tab) => (tab.id === id ? { ...tab, closeRequest: tab.closeRequest + 1 } : tab))
    );
  useEffect(() => {
    if (!dragging) return;
    const move = (event: MouseEvent) =>
      setHeight(
        Math.max(150, Math.min(window.innerHeight - event.clientY, window.innerHeight - 100))
      );
    const end = () => setDragging(false);
    document.addEventListener('mousemove', move);
    document.addEventListener('mouseup', end);
    return () => {
      document.removeEventListener('mousemove', move);
      document.removeEventListener('mouseup', end);
    };
  }, [dragging]);
  const startSsh = (event: React.FormEvent) => {
    event.preventDefault();
    const destination = host.trim();
    const account = user.trim();
    const number = port.trim() ? Number(port) : undefined;
    if (
      !destination ||
      destination.startsWith('-') ||
      /[\s@]/.test(destination) ||
      hasControlCharacters(destination) ||
      destination.includes('://')
    ) {
      setFormError('主机仅填 host 或现有 SSH alias；账号请单独填写。');
      return;
    }
    if (number !== undefined && (!Number.isInteger(number) || number < 1 || number > 65535)) {
      setFormError('端口必须在 1–65535 之间，或留空使用 SSH 配置。');
      return;
    }
    if (hasControlCharacters(account)) {
      setFormError('账号含无效控制字符。');
      return;
    }
    createTab({
      kind: 'ssh',
      host: destination,
      ...(account ? { user: account } : {}),
      ...(number !== undefined ? { port: number } : {}),
    });
    setFormError('');
    setSshForm(false);
  };

  return (
    <section
      aria-label="终端面板"
      hidden={!terminalOpen}
      className="flex flex-col border-t border-[var(--border-primary)] bg-[var(--bg-primary)]"
      style={{ height, display: terminalOpen ? undefined : 'none' }}
    >
      <button
        type="button"
        aria-label="调整终端高度"
        className="h-1 cursor-row-resize hover:bg-[var(--accent)]/30"
        onMouseDown={() => setDragging(true)}
        onKeyDown={(event) => {
          if (event.key === 'ArrowUp')
            setHeight((current) => Math.min(current + 20, window.innerHeight - 100));
          if (event.key === 'ArrowDown') setHeight((current) => Math.max(current - 20, 150));
        }}
      />
      <div className="flex items-center justify-between border-b border-[var(--border-primary)] bg-[var(--bg-sidebar)] px-2">
        <div
          role="tablist"
          aria-label="终端会话"
          className="flex min-w-0 items-center gap-0.5 overflow-x-auto"
        >
          {tabs.map((tab) => (
            <div
              key={tab.id}
              role="tab"
              tabIndex={tab.id === activeId ? 0 : -1}
              aria-selected={tab.id === activeId}
              aria-controls={`panel-${tab.id}`}
              className={`group flex cursor-pointer items-center gap-1.5 rounded-t-md px-3 py-1.5 text-xs ${tab.id === activeId ? 'border-b-2 border-b-[var(--accent)] bg-[var(--bg-primary)] text-[var(--text-primary)]' : 'text-[var(--text-secondary)] hover:bg-[var(--bg-sidebar-hover)]'}`}
              onClick={() => setActiveId(tab.id)}
              onKeyDown={(event) => {
                if (event.key === 'Enter' || event.key === ' ') {
                  event.preventDefault();
                  setActiveId(tab.id);
                }
              }}
            >
              <span title={targetLabel(tab.state.session?.target ?? tab.target)}>
                {tab.label} · {targetLabel(tab.state.session?.target ?? tab.target)}
              </span>
              <button
                aria-label={`${tab.state.phase === 'error' ? '重试关闭' : '关闭'} ${tab.label}`}
                disabled={tab.state.phase === 'closing'}
                onClick={(event) => {
                  event.stopPropagation();
                  closeTab(tab.id);
                }}
                className="rounded p-0.5 hover:bg-[var(--bg-hover)] disabled:opacity-40"
              >
                <X size={12} />
              </button>
            </div>
          ))}
          <button
            onClick={createLocal}
            title="New terminal"
            aria-label="新建本地终端"
            className="rounded-md p-1 text-[var(--text-tertiary)] hover:text-[var(--text-primary)]"
          >
            <Plus size={14} />
          </button>
          <button
            disabled={!isTauri()}
            onClick={() => setSshForm((open) => !open)}
            title={isTauri() ? '系统 OpenSSH 交互终端' : 'SSH 需要 EKO 桌面应用'}
            className="rounded px-2 py-1 text-xs text-[var(--text-secondary)] hover:bg-[var(--bg-sidebar-hover)] disabled:opacity-40"
          >
            SSH
          </button>
        </div>
        <div className="flex items-center gap-1">
          <button
            onClick={() => setHeight((current) => Math.max(150, current - 50))}
            title="Shrink"
            aria-label="缩小终端"
            className="rounded-md p-1 text-[var(--text-tertiary)]"
          >
            <Minimize2 size={12} />
          </button>
          <button
            onClick={hideTerminal}
            title="隐藏终端（保留会话）"
            aria-label="隐藏终端"
            className="rounded-md p-1 text-[var(--text-tertiary)]"
          >
            <X size={14} />
          </button>
        </div>
      </div>
      {sshForm && (
        <form
          onSubmit={startSsh}
          className="flex flex-wrap items-end gap-2 border-b border-[var(--border-primary)] p-2 text-xs"
        >
          <label className="flex flex-1 flex-col gap-1">
            主机 / SSH alias
            <input
              autoFocus
              value={host}
              onChange={(event) => setHost(event.target.value)}
              placeholder="主机或现有 SSH alias"
              className="rounded border border-[var(--border-primary)] bg-[var(--bg-primary)] px-2 py-1 text-[var(--text-primary)]"
            />
          </label>
          <label className="flex flex-col gap-1">
            账号（可选）
            <input
              value={user}
              onChange={(event) => setUser(event.target.value)}
              className="rounded border border-[var(--border-primary)] bg-[var(--bg-primary)] px-2 py-1 text-[var(--text-primary)]"
            />
          </label>
          <label className="flex w-24 flex-col gap-1">
            端口（可选）
            <input
              inputMode="numeric"
              value={port}
              onChange={(event) => setPort(event.target.value)}
              className="rounded border border-[var(--border-primary)] bg-[var(--bg-primary)] px-2 py-1 text-[var(--text-primary)]"
            />
          </label>
          <button
            type="submit"
            className="rounded bg-[var(--accent)] px-3 py-1 text-[var(--text-on-accent)]"
          >
            启动 SSH
          </button>
          <span className="basis-full text-[var(--text-tertiary)]">
            留空项沿用系统 SSH 配置；认证与 host-key 交互在终端完成，不保存凭据。
          </span>
          {formError && (
            <span role="alert" className="basis-full text-[var(--danger)]">
              {formError}
            </span>
          )}
        </form>
      )}
      <div className="min-h-0 flex-1 overflow-hidden">
        {tabs.map((tab) => (
          <div
            key={tab.id}
            id={`panel-${tab.id}`}
            role="tabpanel"
            aria-label={tab.label}
            className="h-full min-h-0 flex-col"
            style={{ display: tab.id === activeId ? 'flex' : 'none' }}
          >
            <Terminal
              sessionId={tab.id}
              target={tab.target}
              visible={terminalOpen && tab.id === activeId}
              closeRequest={tab.closeRequest}
              onStateChange={onState}
              onClosed={onClosed}
            />
          </div>
        ))}
      </div>
    </section>
  );
}
