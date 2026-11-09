import { useEffect, useRef, useState } from 'react';
import { weixinNotificationApi } from '../../api/endpoints';
import type {
  WeixinNotificationConfiguration as NotificationConfiguration,
  WeixinNotificationResult as NotificationResult,
} from '../../api/endpoints';

export function WeixinNotificationConfiguration({
  task,
}: {
  task: { id: string; definition_id: string };
}) {
  const [configuration, setConfiguration] = useState<NotificationConfiguration | null>(null);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [reload, setReload] = useState(0);
  const generation = useRef(0);
  const pending = useRef(false);

  useEffect(() => {
    // 清理时推进同一 ref 的最新代次，不回写 effect 启动时的旧值。
    const invalidate = () => ++generation.current;
    const request = invalidate();
    pending.current = true;
    setBusy(true);
    setConfiguration(null);
    setError(null);
    void weixinNotificationApi
      .configuration(task.definition_id)
      .then((value) => {
        if (request === generation.current) setConfiguration(value);
      })
      .catch(() => {
        if (request === generation.current) setError('无法读取微信通知配置，请重新载入。');
      })
      .finally(() => {
        if (request === generation.current) {
          pending.current = false;
          setBusy(false);
        }
      });
    return () => {
      invalidate();
    };
  }, [task.id, task.definition_id, reload]);

  const toggle = async () => {
    if (!configuration || pending.current) return;
    const request = ++generation.current;
    pending.current = true;
    setBusy(true);
    setError(null);
    try {
      const next = await weixinNotificationApi.configure(
        task.id,
        task.definition_id,
        configuration.revision,
        !configuration.enabled
      );
      if (request === generation.current) setConfiguration(next);
    } catch {
      if (request === generation.current) {
        setConfiguration(null);
        setError('微信通知更新未确认，请重新载入配置后再操作。');
      }
    } finally {
      if (request === generation.current) {
        pending.current = false;
        setBusy(false);
      }
    }
  };

  return (
    <div className="space-y-1 border-t border-[var(--border-primary)] pt-2 text-xs">
      <p>
        {configuration
          ? configuration.enabled
            ? '微信结果通知已启用'
            : '微信结果通知未启用'
          : busy
            ? '正在读取微信通知配置'
            : '微信通知配置未确认'}
      </p>
      {configuration?.enabled && configuration.account_id && (
        <p className="text-[var(--text-secondary)]">接收账号：{configuration.account_id}</p>
      )}
      <p className="text-[var(--text-secondary)]">
        发送给已连接微信的扫码本人；配置变更仅影响之后开始的任务。
      </p>
      <div className="flex gap-3">
        <button
          type="button"
          disabled={busy || !configuration}
          onClick={() => void toggle()}
          className="underline disabled:opacity-50"
        >
          {configuration?.enabled ? '停用微信结果通知' : '启用微信结果通知'}
        </button>
        <button
          type="button"
          disabled={busy}
          onClick={() => setReload((value) => value + 1)}
          className="underline disabled:opacity-50"
        >
          重新载入通知配置
        </button>
      </div>
      {error && (
        <p role="alert" className="text-[var(--color-error-text)]">
          {error}
        </p>
      )}
    </div>
  );
}

function canonical(value: string | null | undefined): string {
  return (value ?? '').replace(/([a-z])([A-Z])/g, '$1_$2').toLowerCase();
}

function receiptLabel(result: NotificationResult): string {
  if (!result.configured) return '本次运行未配置微信结果通知';
  switch (canonical(result.outcome)) {
    case 'completed':
      return '微信已接受结果通知';
    case 'failed':
      return '微信结果通知失败';
    case 'cancelled':
      return '微信结果通知已取消';
    case 'dropped':
      return '微信结果通知未发送';
    case 'outcome_unknown':
      return '微信发送结果未知，请勿直接重发';
  }
  if (result.outcome) return '微信投递结果尚未识别';
  switch (canonical(result.phase)) {
    case 'awaiting_result':
      return '等待本次任务结果后发送微信通知';
    case 'persisted':
      return '微信通知已准入，尚未确认发送';
    case 'claimed':
      return '微信通知等待发送';
    case 'effect_started':
      return '微信发送已开始，结果待确认';
    case 'turn_settled':
      return '微信通知已结算，结果未确认';
    default:
      return '微信通知处理中，平台结果待确认';
  }
}

/** Observe only the original run's journal projection; task completion is a separate fact. */
export function WeixinNotificationResult({ runId }: { runId: string }) {
  const [result, setResult] = useState<NotificationResult | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [reload, setReload] = useState(0);
  useEffect(() => {
    let active = true;
    let timer: ReturnType<typeof setTimeout> | undefined;
    setResult(null);
    setError(null);
    const read = async () => {
      try {
        const next = await weixinNotificationApi.result(runId);
        if (!active) return;
        if (next.run_id !== runId) {
          setError('微信通知记录与本次运行不匹配，请重新读取。');
          return;
        }
        setResult(next);
        if (next.configured && !next.outcome && canonical(next.phase) !== 'turn_settled')
          timer = setTimeout(() => void read(), 2000);
      } catch {
        if (active) setError('无法确认本次微信通知结果，请重新读取。');
      }
    };
    void read();
    return () => {
      active = false;
      if (timer) clearTimeout(timer);
    };
  }, [runId, reload]);

  const visibleResult = result?.run_id === runId ? result : null;
  return (
    <div className="space-y-1 text-xs" aria-label="本次运行微信通知">
      <p role="status">
        {visibleResult ? receiptLabel(visibleResult) : '正在读取本次微信通知结果'}
      </p>
      {visibleResult?.reason && (
        <p className="text-[var(--text-secondary)]">{visibleResult.reason}</p>
      )}
      {error && (
        <>
          <p role="alert" className="text-[var(--color-error-text)]">
            {error}
          </p>
          <button
            type="button"
            className="underline"
            onClick={() => setReload((value) => value + 1)}
          >
            重新读取微信通知结果
          </button>
        </>
      )}
    </div>
  );
}
