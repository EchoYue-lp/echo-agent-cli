import { useCallback, useEffect, useMemo, useRef, useState } from 'react';
import { FolderOpen } from 'lucide-react';
import {
  workspaceApi,
  type ProjectSource,
  type TerminalCreateOptions,
  type TerminalSession,
  type TerminalTarget,
} from '../../api/endpoints';
import { isTauri } from '../../lib/tauri-bridge';
import { Terminal } from '../terminal/Terminal';

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
}

export function ProjectSourceFields({
  draft,
  onChange,
  onBrowse,
  onRemoteSource,
  busy,
  fixedLocation = false,
}: Props) {
  const [connectionId, setConnectionId] = useState<string>();
  const [issue, setIssue] = useState('');
  const update = (patch: Partial<ProjectSourceDraft>) => {
    if (connectionId) return;
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
    setConnectionId(`project-draft-${crypto.randomUUID()}`);
  };
  return (
    <div className="space-y-3">
      <fieldset disabled={busy || fixedLocation || !!connectionId} className="space-y-3">
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
                  value={draft.host}
                  onChange={(event) => update({ host: event.target.value })}
                  className={inputClass}
                  autoComplete="off"
                />
              </label>
              <label className="space-y-1 text-xs text-[var(--text-secondary)]">
                <span>用户名（可选）</span>
                <input
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
                placeholder="远程绝对路径或 ~ 下的路径"
              />
            </label>
          </>
        )}
      </fieldset>
      {draft.kind === 'ssh' &&
        (connectionId ? (
          <ProjectAuthentication
            key={connectionId}
            connectionId={connectionId}
            target={target}
            path={draft.remotePath}
            onSource={onRemoteSource}
            onIssue={setIssue}
          />
        ) : (
          <button
            type="button"
            disabled={busy || !isTauri()}
            onClick={connect}
            className="rounded-lg border border-[var(--accent)] px-3 py-2 text-xs text-[var(--accent)] disabled:opacity-60"
          >
            连接并验证远程目录
          </button>
        ))}
      {connectionId && (
        <button
          type="button"
          disabled={busy}
          onClick={() => {
            setConnectionId(undefined);
            onRemoteSource(undefined);
          }}
          className="text-xs text-[var(--text-secondary)]"
        >
          断开并修改连接
        </button>
      )}
      {draft.kind === 'ssh' && !isTauri() && (
        <p className="text-xs text-[var(--text-tertiary)]">远程项目需要 EKO 桌面应用。</p>
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
}: {
  connectionId: string;
  target: TerminalTarget;
  path: string;
  onSource: (source: ProjectSource | undefined) => void;
  onIssue: (issue: string) => void;
}) {
  const creation = useRef<Promise<TerminalSession> | undefined>(undefined);
  const active = useRef(true);
  const [verifying, setVerifying] = useState(false);
  const [verified, setVerified] = useState(false);
  const start = useCallback(
    (options: TerminalCreateOptions) => {
      if (!active.current) return Promise.reject(new Error('连接视图已关闭'));
      const pending = workspaceApi.connectSsh(
        connectionId,
        target,
        options.rows ?? 24,
        options.cols ?? 80
      );
      creation.current = pending;
      return pending;
    },
    [connectionId, target]
  );
  useEffect(() => {
    active.current = true;
    return () => {
      active.current = false;
      // Observe the original create before closing. After successful owned
      // handoff this draft key is absent, so closing it cannot close the host.
      const pending = creation.current;
      if (pending)
        void pending
          .catch(() => undefined)
          .then(() => {
            if (!active.current) return workspaceApi.closeSsh(connectionId);
          })
          .catch((error) => {
            onIssue(`连接清理仍未确认：${error instanceof Error ? error.message : String(error)}`);
          });
    };
  }, [connectionId, onIssue]);
  const verify = async () => {
    setVerifying(true);
    setVerified(false);
    onIssue('');
    onSource(undefined);
    try {
      await creation.current;
      const result = await workspaceApi.verifySsh(connectionId, target, path);
      if (!active.current) return;
      if (target.kind !== 'ssh') throw new Error('SSH 目标无效');
      onSource({
        kind: 'ssh',
        host: target.host,
        user: target.user ?? null,
        port: target.port ?? null,
        path: result.path,
        connection_id: connectionId,
      });
      setVerified(true);
    } catch (error) {
      if (active.current) onIssue(error instanceof Error ? error.message : '验证失败');
    } finally {
      if (active.current) setVerifying(false);
    }
  };
  return (
    <div className="space-y-2">
      <p className="text-xs text-[var(--text-secondary)]">
        在终端中完成系统 SSH 认证，然后验证文件夹。连接启动不代表认证成功。
      </p>
      <div className="h-52">
        <Terminal sessionId={connectionId} target={target} createSession={start} retainOnUnmount />
      </div>
      <button
        type="button"
        disabled={verifying}
        onClick={verify}
        className="rounded-lg border border-[var(--accent)] px-3 py-2 text-xs text-[var(--accent)]"
      >
        {verifying ? '验证中…' : '验证认证与目录'}
      </button>
      {verified && (
        <p role="status" className="text-xs text-[var(--accent)]">
          远程目录已验证；创建后仍由后端确认并绑定项目。
        </p>
      )}
    </div>
  );
}
