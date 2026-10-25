import { useCallback, useEffect, useRef, useState } from 'react';
import { channelApi } from '../../api/endpoints';
import type {
  ManagedChannelId,
  ManagedChannelsConfiguration,
  ManagedChannelStatus,
  ChannelSecretUpdate,
} from '../../api/endpoints';

function secretUpdate(value: string, clear: boolean): ChannelSecretUpdate {
  return clear ? { action: 'clear' } : value ? { action: 'set', value } : { action: 'preserve' };
}
function initialDraft(config: ManagedChannelsConfiguration, channel: ManagedChannelId) {
  const current = channel === 'qqbot' ? config.qq : config.feishu;
  return {
    revision: config.revision,
    enabled: current.enabled,
    app_id: current.app_id,
    has_secret: current.has_secret,
    mode: config.feishu.mode,
    webhook_bind: config.feishu.webhook_bind,
    webhook_path: config.feishu.webhook_path,
    has_verification_token: config.feishu.has_verification_token,
  };
}
function connectionLabel(status: ManagedChannelStatus | undefined): string {
  if (!status) return '观察尚未载入';
  const { phase, evidence } = status.transport;
  if (phase === 'ready') {
    if (evidence === 'local_webhook_listener') return '本机 webhook 监听就绪（平台可达性未验证）';
    if (evidence === 'feishu_websocket_handshake') return '飞书 WebSocket 握手已确认';
    if (evidence === 'qq_session_ready' || evidence === 'qq_session_resumed')
      return 'QQ 会话就绪已确认';
    return '未知（尚无就绪证据）';
  }
  return {
    stopped: '传输已停止',
    connecting: '正在连接',
    disconnected: '连接中断',
    failed: '连接失败',
    unknown: '未知',
  }[phase];
}
function ChannelCard({
  channel,
  configuration,
  status,
  onControl,
  onRefresh,
}: {
  channel: ManagedChannelId;
  configuration: ManagedChannelsConfiguration;
  status: ManagedChannelStatus | undefined;
  onControl: (
    channel: ManagedChannelId,
    revision: string,
    instance: ManagedChannelStatus,
    action: 'start' | 'stop' | 'reconnect'
  ) => Promise<void>;
  onRefresh: () => Promise<void>;
}) {
  const name = channel === 'qqbot' ? 'QQ Bot' : '飞书';
  const [draft, setDraft] = useState(() => initialDraft(configuration, channel));
  const [secret, setSecret] = useState('');
  const [clearSecret, setClearSecret] = useState(false);
  const [token, setToken] = useState('');
  const [clearToken, setClearToken] = useState(false);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [saved, setSaved] = useState(false);
  const reset = (next: ManagedChannelsConfiguration) => {
    setDraft(initialDraft(next, channel));
    setSecret('');
    setClearSecret(false);
    setToken('');
    setClearToken(false);
  };
  const save = async () => {
    setBusy(true);
    setError(null);
    setSaved(false);
    try {
      const next = await channelApi.save({
        channel,
        expected_revision: draft.revision,
        enabled: draft.enabled,
        app_id: draft.app_id,
        secret: secretUpdate(secret, clearSecret),
        ...(channel === 'feishu'
          ? {
              mode: draft.mode,
              webhook_bind: draft.webhook_bind,
              webhook_path: draft.webhook_path,
              verification_token: secretUpdate(token, clearToken),
            }
          : {}),
      });
      reset(next);
      setSaved(true);
      await onRefresh();
    } catch (failure) {
      setError(failure instanceof Error ? failure.message : String(failure));
    } finally {
      setBusy(false);
    }
  };
  const reload = async () => {
    setBusy(true);
    setError(null);
    setSaved(false);
    try {
      reset(await channelApi.configuration());
    } catch (failure) {
      setError(failure instanceof Error ? failure.message : String(failure));
    } finally {
      setBusy(false);
    }
  };
  const control = async (action: 'start' | 'stop' | 'reconnect') => {
    if (!status) return;
    setBusy(true);
    setError(null);
    try {
      await onControl(channel, draft.revision, status, action);
    } catch (failure) {
      setError(failure instanceof Error ? failure.message : String(failure));
      await onRefresh();
    } finally {
      setBusy(false);
    }
  };
  const inputClass = 'w-full rounded border bg-transparent p-2 text-sm';
  return (
    <section
      className="space-y-3 rounded-lg border p-4"
      style={{ borderColor: 'var(--border-primary)' }}
    >
      <h3 className="font-medium">{name}</h3>
      <form
        className="space-y-2"
        onSubmit={(event) => {
          event.preventDefault();
          void save();
        }}
      >
        <label className="flex items-center gap-2">
          <input
            type="checkbox"
            checked={draft.enabled}
            onChange={(event) => setDraft({ ...draft, enabled: event.target.checked })}
          />
          启用{name}配置
        </label>
        <label className="block text-xs">
          App ID
          <input
            className={inputClass}
            aria-label={`${name} App ID`}
            value={draft.app_id}
            onChange={(event) => setDraft({ ...draft, app_id: event.target.value })}
          />
        </label>
        <label className="block text-xs">
          密钥
          <input
            className={inputClass}
            type="password"
            autoComplete="new-password"
            aria-label={`${name}密钥`}
            value={secret}
            disabled={clearSecret}
            placeholder={draft.has_secret ? '已配置，留空保留' : '尚未配置'}
            onChange={(event) => setSecret(event.target.value)}
          />
        </label>
        <label className="flex items-center gap-2 text-xs">
          <input
            type="checkbox"
            checked={clearSecret}
            onChange={(event) => setClearSecret(event.target.checked)}
          />
          明确清除{name}密钥
        </label>
        {channel === 'feishu' && (
          <>
            <label className="block text-xs">
              连接模式
              <select
                aria-label="飞书连接模式"
                value={draft.mode}
                onChange={(event) => setDraft({ ...draft, mode: event.target.value })}
              >
                <option value="long_poll">长连接</option>
                <option value="webhook">已有 webhook</option>
              </select>
            </label>
            {draft.mode === 'webhook' && (
              <>
                <label className="block text-xs">
                  监听地址
                  <input
                    className={inputClass}
                    aria-label="飞书监听地址"
                    value={draft.webhook_bind}
                    onChange={(event) => setDraft({ ...draft, webhook_bind: event.target.value })}
                  />
                </label>
                <label className="block text-xs">
                  路径
                  <input
                    className={inputClass}
                    aria-label="飞书webhook路径"
                    value={draft.webhook_path}
                    onChange={(event) => setDraft({ ...draft, webhook_path: event.target.value })}
                  />
                </label>
                <label className="block text-xs">
                  Verification token
                  <input
                    className={inputClass}
                    type="password"
                    autoComplete="new-password"
                    aria-label="飞书Verification token"
                    value={token}
                    disabled={clearToken}
                    placeholder={draft.has_verification_token ? '已配置，留空保留' : '尚未配置'}
                    onChange={(event) => setToken(event.target.value)}
                  />
                </label>
                <label className="text-xs">
                  <input
                    type="checkbox"
                    checked={clearToken}
                    onChange={(event) => setClearToken(event.target.checked)}
                  />
                  明确清除 Verification token
                </label>
                <p className="text-xs">
                  仅使用你已有的 webhook 可达入口；本机监听成功不代表平台可达。
                </p>
              </>
            )}
          </>
        )}
        <div className="flex gap-3 text-sm">
          <button type="submit" disabled={busy}>
            保存{name}配置
          </button>
          <button type="button" disabled={busy} onClick={() => void reload()}>
            重新载入{name}配置
          </button>
        </div>
      </form>
      {saved && <p className="text-xs">{name}配置已保存</p>}
      {error && (
        <p role="alert" className="text-xs text-red-500">
          {error}
        </p>
      )}
      <div
        className="space-y-1 border-t pt-2 text-xs"
        style={{ borderColor: 'var(--border-primary)' }}
      >
        <p>{connectionLabel(status)}</p>
        {status?.transport.error && <p>{status.transport.error}</p>}
        {status?.completed_cleanup_failures.map((failure, index) => (
          <p key={`${index}:${failure}`} role="status">
            上次清理已结算，保留失败记录：{failure}
          </p>
        ))}
        {status?.applied_configuration_revision &&
          status.applied_configuration_revision !== status.configuration_revision && (
            <p>应用配置版本已更新；现有连接保持原实例，可单独断开或应用配置。</p>
          )}
        {status && (
          <p>
            {status.shutdown_settled
              ? '原连接资源已结算'
              : status.handler_attached
                ? '原会话仍由运行时持有'
                : '原连接清理未确认'}{' '}
            · 活跃调用 {status.active_invocations}
          </p>
        )}
        {status?.transport.observed_at && (
          <p>观察于 {new Date(status.transport.observed_at).toLocaleString()}</p>
        )}
        <div className="flex gap-3 text-sm">
          <button
            disabled={busy || !status || !draft.enabled}
            onClick={() => void control('start')}
          >
            连接{name}
          </button>
          <button
            disabled={busy || !status || !draft.enabled}
            onClick={() => void control('reconnect')}
          >
            应用配置并重连{name}
          </button>
          <button
            disabled={busy || !status || !status.registered}
            onClick={() => void control('stop')}
          >
            断开{name}
          </button>
        </div>
        <p>连接状态与发送结果分别确认；实际发送结果请查看原 IM 会话记录。</p>
      </div>
    </section>
  );
}

export function ChannelsPanel() {
  const [configuration, setConfiguration] = useState<ManagedChannelsConfiguration | null>(null);
  const [statuses, setStatuses] = useState<ManagedChannelStatus[]>([]);
  const [error, setError] = useState<string | null>(null);
  const request = useRef(0);
  const mounted = useRef(true);
  const publish = useCallback(
    (channels: ManagedChannelStatus[]) =>
      setStatuses((previous) =>
        channels.map((incoming) => {
          const old = previous.find((entry) => entry.id === incoming.id);
          if (
            old?.mutation_epoch &&
            incoming.mutation_epoch &&
            /^\d+$/.test(old.mutation_epoch) &&
            /^\d+$/.test(incoming.mutation_epoch) &&
            BigInt(incoming.mutation_epoch) < BigInt(old.mutation_epoch)
          )
            return old;
          return incoming;
        })
      ),
    []
  );
  const refresh = useCallback(async () => {
    const generation = ++request.current;
    try {
      const value = await channelApi.status();
      if (mounted.current && generation === request.current) {
        publish(value.channels);
        setError(null);
      }
    } catch (failure) {
      if (mounted.current && generation === request.current)
        setError(failure instanceof Error ? failure.message : String(failure));
    }
  }, [publish]);
  useEffect(() => {
    mounted.current = true;
    let active = true;
    let timer: ReturnType<typeof setTimeout> | undefined;
    void channelApi
      .configuration()
      .then((value) => {
        if (active) setConfiguration(value);
      })
      .catch((failure) => {
        if (active) setError(String(failure));
      });
    const poll = async () => {
      await refresh();
      if (active) timer = setTimeout(() => void poll(), 2000);
    };
    void poll();
    return () => {
      active = false;
      mounted.current = false;
      if (timer) clearTimeout(timer);
    };
  }, [refresh]);
  const control = async (
    channel: ManagedChannelId,
    revision: string,
    instance: ManagedChannelStatus,
    action: 'start' | 'stop' | 'reconnect'
  ) => {
    const generation = ++request.current;
    const value = await channelApi.control(channel, revision, instance, action);
    if (mounted.current && generation === request.current) publish(value.channels);
    await refresh();
  };
  return (
    <div className="space-y-4 p-4" style={{ color: 'var(--text-primary)' }}>
      <h2 className="text-base font-semibold">IM 渠道</h2>
      <p className="text-xs">管理已有 QQ Bot 与飞书。保存配置、连接事实和消息投递分别呈现。</p>
      {error && <p role="alert">{error}</p>}
      {!configuration ? (
        <p>正在读取配置</p>
      ) : (
        <>
          <ChannelCard
            channel="qqbot"
            configuration={configuration}
            status={statuses.find((status) => status.id === 'qqbot')}
            onControl={control}
            onRefresh={refresh}
          />
          <ChannelCard
            channel="feishu"
            configuration={configuration}
            status={statuses.find((status) => status.id === 'feishu')}
            onControl={control}
            onRefresh={refresh}
          />
        </>
      )}
    </div>
  );
}
