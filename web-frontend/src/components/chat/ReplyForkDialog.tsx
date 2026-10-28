import { useEffect, useRef, useState } from 'react';
import { FolderOpen, GitFork, GitBranch, X } from 'lucide-react';
import {
  conversationApi,
  type ConversationForkDestination,
  type ConversationForkPreparation,
  type ConversationForkRequest,
  type ConversationForkReceipt,
  type ProductDataScope,
  type ProjectSource,
} from '../../api/endpoints';
import { useConversationStore } from '../../stores/conversationStore';
import { useWorkspaceStore } from '../../stores/workspaceStore';
import { useUiStore } from '../../stores/uiStore';
import { useContextPaneStore } from '../../stores/contextPaneStore';
import { useToastStore } from '../../stores/toastStore';
import { productDataScope, sameProductDataScope } from '../../lib/productDataScope';
import { Modal } from '../common/Modal';
import { ProjectSourceFields, emptyProjectSource } from '../workspace/ProjectSourceFields';

export interface ReplyForkSource {
  scope: ProductDataScope;
  conversationId: string;
  conversationEpoch: number;
  navigationEpoch: number;
  userTurnIndex: number;
  requestId: string;
}

type DisplayReceipt = ConversationForkReceipt & {
  opened: boolean;
  open_error?: string;
};

function resourceNotice(receipt: DisplayReceipt): string {
  const resources = [
    receipt.worktree
      ? `${receipt.worktree.created || receipt.worktree.observed ? '已创建 Worktree' : 'Worktree 状态待核对'}：${receipt.worktree.path}`
      : '',
    receipt.workspace ? `已登记工作空间：${receipt.workspace.name}` : '',
    receipt.imported && receipt.id ? '会话历史已复制' : '',
    receipt.resources?.length
      ? `已保存 ${receipt.resources.length} 份历史资源${receipt.resources.some((resource) => !resource.reader_verified) ? `，其中 ${receipt.resources.filter((resource) => !resource.reader_verified).length} 份仍待核对` : ''}`
      : '',
    ...(receipt.warnings ?? []),
  ].filter(Boolean);
  return resources.join('；');
}

function recoveryNotice(receipt: DisplayReceipt): string {
  const listed =
    receipt.workspace ||
    receipt.conversation_created ||
    receipt.worktree?.created ||
    receipt.worktree?.observed;
  return listed
    ? '已创建的项目可从最近任务、工作空间或 Worktree 列表打开；其余步骤仍需核对。'
    : '当前没有可打开的新项目，请保留错误信息并核对后重试。';
}

export function ReplyForkDialog({
  source,
  onClose,
}: {
  source: ReplyForkSource;
  onClose: () => void;
}) {
  const [preparation, setPreparation] = useState<ConversationForkPreparation | null>(null);
  const [destination, setDestination] = useState<ConversationForkDestination>('current_workspace');
  const [remoteSource, setRemoteSource] = useState<ProjectSource>();
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState('');
  const [receipt, setReceipt] = useState<DisplayReceipt | null>(null);
  const mounted = useRef(false);
  const open = useRef(true);
  const pending = useRef(false);
  const submitted = useRef<ConversationForkRequest | null>(null);
  const preparing = useRef<Promise<ConversationForkPreparation> | null>(null);
  const [connectionRetained, setConnectionRetained] = useState(false);
  const current = useWorkspaceStore((state) => state.current);
  const workspaceLoading = useWorkspaceStore((state) => state.isLoading);
  const conversationId = useConversationStore((state) => state.activeId);
  const conversationEpoch = useConversationStore((state) => state.newConversationEpoch);
  const conversationLoading = useConversationStore((state) => state.isLoading);
  const navigationEpoch = useUiStore((state) => state.navigationEpoch);
  const ownsSource = () => {
    const conversation = useConversationStore.getState();
    const workspace = useWorkspaceStore.getState();
    return (
      open.current &&
      mounted.current &&
      !workspace.isLoading &&
      !conversation.isLoading &&
      sameProductDataScope(source.scope, productDataScope(workspace.current)) &&
      conversation.activeId === source.conversationId &&
      conversation.newConversationEpoch === source.conversationEpoch &&
      useUiStore.getState().navigationEpoch === source.navigationEpoch
    );
  };
  const close = () => {
    open.current = false;
    onClose();
  };
  useEffect(() => {
    mounted.current = true;
    open.current = true;
    if (!preparing.current)
      preparing.current = conversationApi.prepareFork(
        source.scope,
        source.conversationId,
        source.userTurnIndex,
        source.requestId
      );
    void preparing.current.then(
      (value) => {
        if (!mounted.current || !open.current) return;
        if (
          value.request_id !== source.requestId ||
          value.source_id !== source.conversationId ||
          value.workspace_id !== source.scope.workspaceId ||
          value.workspace_generation !== source.scope.workspaceGeneration ||
          value.user_turn_index !== source.userTurnIndex
        ) {
          setError('分叉来源回执不匹配，请关闭后重新打开');
          return;
        }
        setPreparation(value);
      },
      (cause) => {
        if (mounted.current && open.current)
          setError(cause instanceof Error ? cause.message : '无法确认这轮回复，请稍后重试');
      }
    );
    return () => {
      mounted.current = false;
      open.current = false;
    };
  }, [source]);
  const sourceChanged =
    workspaceLoading ||
    conversationLoading ||
    !sameProductDataScope(source.scope, productDataScope(current)) ||
    conversationId !== source.conversationId ||
    conversationEpoch !== source.conversationEpoch ||
    navigationEpoch !== source.navigationEpoch;
  const ssh = preparation?.worktree?.ssh_target;
  const draft = ssh
    ? {
        ...emptyProjectSource(),
        kind: 'ssh' as const,
        host: ssh.host,
        user: ssh.user ?? '',
        port: ssh.port?.toString() ?? '',
        remotePath: preparation?.worktree?.path ?? '',
      }
    : null;
  const submit = async () => {
    if (pending.current || !preparation || !ownsSource()) return;
    let request = submitted.current;
    if (!request) {
      if (destination === 'new_worktree' && !preparation.worktree?.available) return;
      if (
        destination === 'new_worktree' &&
        ssh &&
        (remoteSource?.kind !== 'ssh' ||
          remoteSource.connection_id !== preparation.worktree?.draft_connection_id)
      ) {
        setError('请先用下方表单连接同一远程设备');
        return;
      }
      request = {
        preparation,
        destination,
        ...(destination === 'new_worktree' && remoteSource?.kind === 'ssh'
          ? { ssh_connection_id: remoteSource.connection_id }
          : {}),
      };
      submitted.current = request;
    }
    pending.current = true;
    setConnectionRetained(!!request.ssh_connection_id);
    setBusy(true);
    setError('');
    try {
      const result = await useConversationStore
        .getState()
        .forkCurrent(
          request,
          () =>
            open.current &&
            mounted.current &&
            useUiStore.getState().navigationEpoch === source.navigationEpoch
        );
      const facts = resourceNotice(result);
      if (!mounted.current || !open.current) {
        useToastStore
          .getState()
          .addToast(
            result.success ? 'info' : 'warning',
            `${facts || '分叉操作已返回'}。${recoveryNotice(result)}`,
            10000
          );
        return;
      }
      setReceipt(result);
      if (result.opened) {
        if (result.warnings?.length)
          useToastStore.getState().addToast('warning', result.warnings.join('；'), 10000);
        useContextPaneStore.getState().reset();
        close();
      } else if (result.success && !result.open_error) {
        useToastStore.getState().addToast('info', `${facts}。已保留，可从最近任务打开。`, 8000);
        close();
      } else
        setError(result.open_error ?? result.error ?? '尚未完成分叉，已创建资源保留在原列表中');
    } catch (cause) {
      const message = cause instanceof Error ? cause.message : '分叉结果尚未确认';
      if (mounted.current && open.current)
        setError(`${message}。重试会核对同一请求；已创建资源不会删除。`);
      else
        useToastStore
          .getState()
          .addToast('warning', `${message}。请从工作空间或 Worktree 列表核对资源。`, 10000);
    } finally {
      pending.current = false;
      if (mounted.current) {
        setBusy(false);
        setConnectionRetained(false);
      }
    }
  };
  return (
    <Modal
      onClose={close}
      ariaLabel="分叉会话"
      className="w-[min(440px,calc(100vw-24px))] max-h-[calc(100dvh-32px)] overflow-y-auto rounded-2xl border border-[var(--border-primary)] bg-[var(--bg-primary)] p-5 shadow-xl"
    >
      <div className="flex items-center justify-between gap-3">
        <h2 className="flex items-center gap-2 text-base font-semibold text-[var(--text-primary)]">
          <GitFork size={17} />
          分叉会话
        </h2>
        <button
          type="button"
          onClick={close}
          aria-label="关闭分叉"
          className="rounded-md p-1 text-[var(--text-tertiary)] hover:bg-[var(--bg-hover)]"
        >
          <X size={17} />
        </button>
      </div>
      <p className="mb-4 mt-2 text-xs leading-relaxed text-[var(--text-secondary)]">
        复制到这轮回复为止的完整对话，新会话等待你继续。
      </p>
      <fieldset disabled={busy || !!submitted.current || sourceChanged} className="space-y-2">
        <legend className="sr-only">分叉目的地</legend>
        {(
          [
            ['current_workspace', '当前工作空间', '共用当前文件夹，修改会在原处可见。', FolderOpen],
            ['new_worktree', '新 Worktree', '从上次提交创建，未提交改动留原处。', GitBranch],
          ] as const
        ).map(([value, label, description, Icon]) => (
          <label
            key={value}
            className={`flex cursor-pointer gap-3 rounded-xl border p-3 ${destination === value ? 'border-[var(--accent)] bg-[var(--bg-sidebar-active)]' : 'border-[var(--border-primary)]'}`}
          >
            <input
              type="radio"
              name="fork-destination"
              value={value}
              checked={destination === value}
              onChange={() => setDestination(value)}
              disabled={value === 'new_worktree' && !preparation?.worktree?.available}
              className="mt-1 accent-[var(--accent)]"
            />
            <span className="min-w-0">
              <span className="flex items-center gap-2 text-sm font-medium text-[var(--text-primary)]">
                <Icon size={15} />
                {label}
              </span>
              <span className="mt-1 block text-xs leading-relaxed text-[var(--text-secondary)]">
                {description}
              </span>
              {value === 'new_worktree' && preparation && !preparation.worktree?.available && (
                <span className="mt-1 block text-xs text-[var(--text-tertiary)]">
                  {preparation.worktree?.reason ?? '当前工作空间没有可用的 Git 提交。'}
                </span>
              )}
            </span>
          </label>
        ))}
      </fieldset>
      {destination === 'new_worktree' && draft && (
        <div className="mt-4">
          <p className="mb-2 text-xs text-[var(--text-secondary)]">
            连接同一远程设备，为新工作空间建立独立连接。
          </p>
          <ProjectSourceFields
            draft={draft}
            onChange={() => undefined}
            onBrowse={() => undefined}
            onRemoteSource={setRemoteSource}
            busy={busy}
            fixedLocation
            draftConnectionId={preparation?.worktree?.draft_connection_id ?? undefined}
            retainConnection={connectionRetained}
          />
        </div>
      )}
      {!preparation && !error && (
        <p role="status" className="mt-3 text-xs text-[var(--text-secondary)]">
          正在确认这轮回复和工作空间…
        </p>
      )}
      {sourceChanged && !busy && (
        <p role="status" className="mt-3 text-xs text-[var(--text-secondary)]">
          来源已切换，请从需要的回复重新打开分叉。
        </p>
      )}
      {receipt && resourceNotice(receipt) && (
        <p role="status" className="mt-3 break-words text-xs text-[var(--text-secondary)]">
          资源状态：{resourceNotice(receipt)}。{recoveryNotice(receipt)}
        </p>
      )}
      {error && (
        <p role="alert" className="mt-3 break-words text-xs text-[var(--color-error-text)]">
          {error}
        </p>
      )}
      <div className="mt-5 flex justify-end gap-2">
        <button
          type="button"
          onClick={close}
          className="rounded-lg border border-[var(--border-primary)] px-3 py-2 text-xs text-[var(--text-secondary)]"
        >
          {busy ? '关闭' : '取消'}
        </button>
        <button
          type="button"
          onClick={() => void submit()}
          disabled={
            busy ||
            !preparation ||
            sourceChanged ||
            (destination === 'new_worktree' && !!ssh && !remoteSource)
          }
          className="rounded-lg bg-[var(--accent)] px-4 py-2 text-xs font-medium text-white disabled:opacity-50"
        >
          {busy ? '正在创建…' : submitted.current ? '核对并重试' : '创建分叉'}
        </button>
      </div>
    </Modal>
  );
}
