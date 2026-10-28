import { useEffect, useMemo, useRef, useState } from 'react';
import { FolderOpen } from 'lucide-react';
import { listen } from '@tauri-apps/api/event';
import {
  terminalApi,
  workspaceApi,
  type ProjectSource,
  type TerminalSession,
  type TerminalTarget,
} from '../../api/endpoints';
import { isTauri } from '../../lib/tauri-bridge';
import { decodeTerminalBytes, encodeTerminalBytes, observeTerminal } from '../terminal/terminal-io';
import { useToastStore } from '../../stores/toastStore';

export interface ProjectSourceDraft {
  kind: 'existing_local' | 'new_local' | 'ssh';
  localPath: string;
  directoryName: string;
  host: string;
  user: string;
  port: string;
  remotePath: string;
}

export const emptyProjectSource = (): ProjectSourceDraft => ({
  kind: 'existing_local',
  localPath: '',
  directoryName: '',
  host: '',
  user: '',
  port: '',
  remotePath: '',
});

export function localProjectSource(draft: ProjectSourceDraft): ProjectSource {
  if (!draft.localPath.trim()) throw new Error('请选择本地文件夹');
  if (draft.kind === 'existing_local')
    return { kind: 'existing_local', path: draft.localPath.trim() };
  if (draft.kind !== 'new_local') throw new Error('请先连接并验证远程目录');
  const name = draft.directoryName.trim();
  if (!name || name === '.' || name === '..' || name.includes('/') || name.includes('\\')) {
    throw new Error('请输入一个新的文件夹名称，不要包含路径分隔符');
  }
  return { kind: 'new_local', parent: draft.localPath.trim(), directory_name: name };
}

const inputClass =
  'min-w-0 w-full rounded-lg border border-[var(--border-primary)] bg-[var(--bg-input)] px-3 py-2 text-sm text-[var(--text-primary)] outline-none focus:border-[var(--accent)] disabled:opacity-60';

interface Props {
  draft: ProjectSourceDraft;
  onChange: (draft: ProjectSourceDraft) => void;
  onBrowse: () => void;
  onRemoteSource: (source: ProjectSource | undefined) => void;
  busy: boolean;
  fixedLocation?: boolean;
  draftConnectionId?: string;
  retainConnection?: boolean;
}

export function ProjectSourceFields({
  draft,
  onChange,
  onBrowse,
  onRemoteSource,
  busy,
  fixedLocation = false,
  draftConnectionId,
  retainConnection = false,
}: Props) {
  const [connectionId, setConnectionId] = useState<string>();
  const [issue, setIssue] = useState('');
  const [password, setPassword] = useState('');
  const [testing, setTesting] = useState(false);
  const [credentialLocked, setCredentialLocked] = useState(false);
  const update = (patch: Partial<ProjectSourceDraft>) => {
    if (connectionId && Object.keys(patch).some((key) => key !== 'remotePath')) return;
    if ('host' in patch || 'user' in patch || 'port' in patch || 'kind' in patch) setPassword('');
    onRemoteSource(undefined);
    onChange({ ...draft, ...patch });
  };
  const target = useMemo<TerminalTarget>(
    () => ({
      kind: 'ssh',
      host: draft.host.trim(),
      user: draft.user.trim() || null,
      port: draft.port.trim() ? Number(draft.port) : null,
    }),
    [draft.host, draft.user, draft.port]
  );
  const connect = () => {
    const port = target.kind === 'ssh' ? target.port : null;
    if (
      !draft.host.trim() ||
      (port != null && (!Number.isInteger(port) || port < 1 || port > 65535))
    ) {
      setIssue('请输入主机和有效的 SSH 端口');
      return;
    }
    setIssue('');
    onRemoteSource(undefined);
    setConnectionId(draftConnectionId ?? `project-draft-${crypto.randomUUID()}`);
  };
  return (
    <div className="space-y-3">
      <fieldset disabled={busy || fixedLocation} className="space-y-3">
        <legend className="mb-2 text-xs font-medium text-[var(--text-secondary)]">
          工作空间位置
        </legend>
        <div className="grid grid-cols-3 gap-2">
          {(
            [
              ['existing_local', '已有本地文件夹'],
              ['new_local', '新建本地文件夹'],
              ['ssh', '远程 SSH 设备'],
            ] as const
          ).map(([kind, label]) => (
            <button
              type="button"
              disabled={!!connectionId}
              key={kind}
              aria-pressed={draft.kind === kind}
              onClick={() => update({ kind })}
              className="rounded-lg border px-2 py-2 text-xs text-[var(--text-primary)] disabled:opacity-60"
              style={{
                borderColor: draft.kind === kind ? 'var(--accent)' : 'var(--border-primary)',
                background: draft.kind === kind ? 'var(--bg-sidebar-active)' : undefined,
              }}
            >
              {label}
            </button>
          ))}
        </div>
        {draft.kind !== 'ssh' ? (
          <>
            <label className="block space-y-1 text-xs text-[var(--text-secondary)]">
              <span>{draft.kind === 'new_local' ? '父文件夹' : '项目文件夹'}</span>
              <span className="flex gap-2">
                <input
                  aria-label={draft.kind === 'new_local' ? '父文件夹' : '项目文件夹'}
                  value={draft.localPath}
                  onChange={(event) => update({ localPath: event.target.value })}
                  className={inputClass}
                  placeholder="选择或输入绝对路径"
                />
                <button
                  type="button"
                  onClick={onBrowse}
                  className="flex shrink-0 items-center gap-1 rounded-lg border border-[var(--border-primary)] px-3"
                >
                  <FolderOpen size={14} />
                  浏览
                </button>
              </span>
            </label>
            {draft.kind === 'new_local' && (
              <label className="block space-y-1 text-xs text-[var(--text-secondary)]">
                <span>新文件夹名称</span>
                <input
                  value={draft.directoryName}
                  onChange={(event) => update({ directoryName: event.target.value })}
                  className={inputClass}
                />
              </label>
            )}
          </>
        ) : (
          <>
            <div className="grid grid-cols-2 gap-2">
              <label className="space-y-1 text-xs text-[var(--text-secondary)]">
                <span>主机或 SSH 别名</span>
                <input
                  disabled={!!connectionId}
                  value={draft.host}
                  onChange={(event) => update({ host: event.target.value })}
                  className={inputClass}
                  autoComplete="off"
                />
              </label>
              <label className="space-y-1 text-xs text-[var(--text-secondary)]">
                <span>用户名（可选）</span>
                <input
                  disabled={!!connectionId}
                  value={draft.user}
                  onChange={(event) => update({ user: event.target.value })}
                  className={inputClass}
                  autoComplete="off"
                />
              </label>
            </div>
            <label className="block space-y-1 text-xs text-[var(--text-secondary)]">
              <span>端口（默认使用 SSH 配置）</span>
              <input
                disabled={!!connectionId}
                value={draft.port}
                onChange={(event) => update({ port: event.target.value })}
                className={inputClass}
                inputMode="numeric"
              />
            </label>
            <label className="block space-y-1 text-xs text-[var(--text-secondary)]">
              <span>远程项目文件夹</span>
              <input
                value={draft.remotePath}
                onChange={(event) => update({ remotePath: event.target.value })}
                className={inputClass}
                placeholder="如 ssh-test、~/ssh-test 或 /home/ubuntu/ssh-test"
              />
            </label>
          </>
        )}
      </fieldset>
      {draft.kind === 'ssh' && (
        <label className="block space-y-1 text-xs text-[var(--text-secondary)]">
          <span>SSH 密码</span>
          <input
            type="password"
            aria-label="SSH 密码"
            value={password}
            disabled={busy || testing || credentialLocked}
            onChange={(event) => setPassword(event.target.value)}
            autoComplete="off"
            spellCheck={false}
            className={inputClass}
            placeholder="仅用于本次连接；密钥认证可留空"
          />
          <span className="block text-[11px] text-[var(--text-tertiary)]">
            {credentialLocked
              ? '已登录；如需更换密码，请先断开并重新测试连接。'
              : '密码以掩码显示，不保存。测试只验证登录；创建时再校验目录，相对目录从远程 home 解析。'}
          </span>
        </label>
      )}
      {draft.kind === 'ssh' &&
        (connectionId ? (
          <ProjectAuthentication
            key={connectionId}
            connectionId={connectionId}
            target={target}
            path={draft.remotePath}
            onSource={onRemoteSource}
            onIssue={setIssue}
            password={password}
            onPasswordConsumed={(submitted) =>
              setPassword((value) => (value === submitted ? '' : value))
            }
            onTestingChange={setTesting}
            onAuthenticatedChange={setCredentialLocked}
            busy={busy}
            retainConnection={retainConnection}
          />
        ) : (
          <button
            type="button"
            disabled={busy || !isTauri()}
            onClick={connect}
            className="rounded-lg border border-[var(--accent)] px-3 py-2 text-xs text-[var(--accent)] disabled:opacity-60"
          >
            测试连接
          </button>
        ))}
      {connectionId && (
        <button
          type="button"
          disabled={busy}
          onClick={() => {
            setConnectionId(undefined);
            setPassword('');
            setTesting(false);
            setCredentialLocked(false);
            setIssue('');
            onRemoteSource(undefined);
          }}
          className="text-xs text-[var(--text-secondary)]"
        >
          断开并修改连接
        </button>
      )}
      {draft.kind === 'ssh' && !isTauri() && (
        <p className="text-xs text-[var(--text-tertiary)]">远程项目需要 Eclip 桌面应用。</p>
      )}
      {issue && (
        <p role="alert" className="text-xs text-[var(--color-error-text)]">
          {issue}
        </p>
      )}
      <p className="text-xs text-[var(--text-tertiary)]">
        会话和任务数据保存在本机。已有项目文件保留原位，移除项目不会删除代码。
      </p>
    </div>
  );
}

function ProjectAuthentication({
  connectionId,
  target,
  path,
  onSource,
  onIssue,
  password,
  onPasswordConsumed,
  onTestingChange,
  onAuthenticatedChange,
  busy,
  retainConnection,
}: {
  connectionId: string;
  target: TerminalTarget;
  path: string;
  onSource: (source: ProjectSource | undefined) => void;
  onIssue: (issue: string) => void;
  password: string;
  onPasswordConsumed: (submitted: string) => void;
  onTestingChange: (testing: boolean) => void;
  onAuthenticatedChange: (authenticated: boolean) => void;
  busy: boolean;
  retainConnection: boolean;
}) {
  const latest = useRef({
    retainConnection,
    password,
    path,
    onSource,
    onIssue,
    onPasswordConsumed,
    onTestingChange,
    onAuthenticatedChange,
  });
  latest.current = {
    retainConnection,
    password,
    path,
    onSource,
    onIssue,
    onPasswordConsumed,
    onTestingChange,
    onAuthenticatedChange,
  };
  const active = useRef(false);
  const creation = useRef<Promise<TerminalSession> | undefined>(undefined);
  const operation = useRef<symbol | null>(null);
  const retry = useRef<(() => Promise<void>) | undefined>(undefined);
  const trust = useRef<(() => Promise<void>) | undefined>(undefined);
  const [testing, setTesting] = useState(true);
  const [authenticated, setAuthenticated] = useState(false);
  const [status, setStatus] = useState('正在连接 SSH…');
  const [hostPrompt, setHostPrompt] = useState('');

  useEffect(() => {
    active.current = true;
    let live = true;
    let exited = false;
    let passwordPrompt = 0;
    let hostKey = '';
    let tail = '';
    const decoder = new TextDecoder();
    const releases: Array<() => void> = [];
    const current = (token: symbol) => live && operation.current === token;
    const write = async (value: string) => {
      const result = await observeTerminal(
        terminalApi.write(connectionId, encodeTerminalBytes(new TextEncoder().encode(value)))
      );
      if (!result.success) throw new Error('SSH 输入未确认，请断开后重新测试；不会自动重发密码');
    };
    const test = async () => {
      if (!live || operation.current) return;
      const token = Symbol('ssh-login-test');
      operation.current = token;
      setTesting(true);
      latest.current.onTestingChange(true);
      latest.current.onIssue('');
      latest.current.onSource(undefined);
      setAuthenticated(false);
      latest.current.onAuthenticatedChange(false);
      setStatus('正在测试 SSH 登录…');
      const deadline = performance.now() + 30_000;
      let sentPrompt = 0;
      try {
        const original = creation.current;
        if (!original) throw new Error('SSH 连接尚未开始');
        await observeTerminal(original);
        while (current(token)) {
          if (exited) throw new Error('SSH 连接已退出，请断开后重新测试连接');
          const result = await observeTerminal(workspaceApi.testSsh(connectionId, target));
          if (!current(token)) return;
          if (result.authenticated) {
            latest.current.onPasswordConsumed(latest.current.password);
            latest.current.onAuthenticatedChange(true);
            setAuthenticated(true);
            setStatus('SSH 登录已验证；创建时再验证远程目录。');
            return;
          }
          if (hostKey) {
            setStatus('请核对主机指纹后确认连接');
            return;
          }
          if (passwordPrompt > sentPrompt) {
            if (sentPrompt) throw new Error('SSH 未接受本次密码，请重新输入后测试连接');
            const secret = latest.current.password;
            if (!secret) {
              setStatus('SSH 正在等待密码，请填写上方密码后再次测试连接');
              return;
            }
            if (
              Array.from(secret).some(
                (char) => char === '\r' || char === '\n' || char.charCodeAt(0) === 0
              )
            )
              throw new Error('SSH 密码不能包含换行或空字符');
            sentPrompt = passwordPrompt;
            // 只在原 OpenSSH 已请求密码后写入；其原 PTY 负责关闭回显。
            // 发送前清除本次密码，不因迟到失败重放，也不写入配置/日志/模型。
            latest.current.onPasswordConsumed(secret);
            await write(secret + '\r');
            if (!current(token)) return;
            setStatus('密码已提交，正在确认原 SSH 连接…');
          }
          if (performance.now() >= deadline)
            throw new Error('SSH 登录测试超时，请检查主机、用户名和认证方式；可断开后重新测试');
          await new Promise<void>((resolve) => setTimeout(resolve, 250));
        }
      } catch (error) {
        if (current(token)) {
          setStatus('SSH 登录尚未通过');
          latest.current.onIssue(error instanceof Error ? error.message : 'SSH 登录测试失败');
        }
      } finally {
        if (current(token)) {
          operation.current = null;
          setTesting(false);
          latest.current.onTestingChange(false);
        }
      }
    };
    retry.current = test;
    trust.current = async () => {
      if (!live || operation.current || !hostKey) return;
      try {
        await write('yes\r');
        if (!live) return;
        hostKey = '';
        tail = '';
        setHostPrompt('');
        await test();
      } catch {
        if (live) latest.current.onIssue('主机指纹确认未送达，请断开后重试');
      }
    };
    creation.current = (async () => {
      const observe = async <T,>(name: string, receive: (payload: T) => void) => {
        const release = await listen<T>(name, ({ payload }) => {
          if (live) receive(payload);
        });
        if (live) releases.push(release);
        else release();
      };
      await Promise.all([
        observe<{ id: string; data: string }>('terminal-output', (payload) => {
          if (payload.id !== connectionId) return;
          tail = (tail + decoder.decode(decodeTerminalBytes(payload.data), { stream: true })).slice(
            -4096
          );
          if (/Are you sure you want to continue connecting[^?]*\?\s*$/is.test(tail)) {
            hostKey = tail;
            setHostPrompt(tail);
            tail = '';
          } else if (/(?:password|passphrase)[^:\r\n]*:\s*$/i.test(tail)) {
            passwordPrompt += 1;
            tail = '';
          }
        }),
        observe<{ id: string }>('terminal-exit', (payload) => {
          if (payload.id !== connectionId) return;
          exited = true;
          setAuthenticated(false);
          latest.current.onAuthenticatedChange(false);
          latest.current.onSource(undefined);
          setStatus('SSH 连接已退出，请断开后重新测试');
        }),
        observe<{ id: string; message: string }>('terminal-issue', (payload) => {
          if (payload.id === connectionId)
            latest.current.onIssue('SSH 连接异常：' + payload.message);
        }),
      ]);
      if (!live) throw new Error('SSH 连接视图已关闭');
      return workspaceApi.connectSsh(connectionId, target, 24, 80);
    })();
    void test();
    return () => {
      live = false;
      active.current = false;
      operation.current = null;
      releases.forEach((release) => release());
      const pending = creation.current;
      if (pending)
        void pending
          .catch(() => undefined)
          .then(() => {
            // Once submitted, the existing fork command owns this independent draft.
            // Closing an observer must not close an in-flight Git/import connection.
            if (!active.current && !latest.current.retainConnection)
              return workspaceApi.closeSsh(connectionId);
          })
          .catch(() => {
            useToastStore
              .getState()
              .addToast(
                'warning',
                '原 SSH 认证连接清理尚未确认；资源仍由应用持有，可通过退出应用重试结算。'
              );
          });
    };
  }, [connectionId, target]);

  useEffect(() => {
    if (authenticated && target.kind === 'ssh')
      onSource({
        kind: 'ssh',
        host: target.host,
        user: target.user ?? null,
        port: target.port ?? null,
        path: path.trim(),
        connection_id: connectionId,
      });
  }, [authenticated, target, path, connectionId, onSource]);

  return (
    <div className="space-y-2" aria-busy={testing}>
      <p role="status" className="text-xs text-[var(--text-secondary)]">
        {status}
      </p>
      {hostPrompt && (
        <div className="space-y-2 rounded border border-[var(--border-primary)] p-3">
          <p className="whitespace-pre-wrap break-words text-xs text-[var(--text-secondary)]">
            {hostPrompt}
          </p>
          <button
            type="button"
            disabled={busy || testing}
            onClick={() => void trust.current?.()}
            className="text-xs text-[var(--accent)]"
          >
            核对指纹并信任此主机
          </button>
        </div>
      )}
      <button
        type="button"
        disabled={busy || testing}
        onClick={() => void retry.current?.()}
        className="rounded-lg border border-[var(--accent)] px-3 py-2 text-xs text-[var(--accent)]"
      >
        {testing ? '正在测试连接…' : '测试连接'}
      </button>
    </div>
  );
}
