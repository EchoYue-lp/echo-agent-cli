import { useEffect, useRef, useState } from 'react';
import { Card } from '../common/Card';

import type { WeixinQrApi, WeixinQrPhase, WeixinQrStatus } from '../../api/endpoints';
export type { WeixinQrApi, WeixinQrPhase, WeixinQrStatus } from '../../api/endpoints';

const PHASE_LABELS: Record<WeixinQrPhase, string> = {
  idle: '尚未开始扫码',
  waiting_scan: '请用微信扫描二维码',
  waiting_confirmation: '已扫码，请在手机上确认',
  need_verify_code: '请填写手机上显示的数字校验码',
  expired: '二维码已过期，请重新获取',
  verification_blocked: '数字校验暂不可用，请重新扫码',
  authorized: '授权已保存',
  cancelled: '扫码已取消',
  failed: '授权未完成，请重试',
};

function isActive(phase: WeixinQrPhase): boolean {
  return (
    phase === 'waiting_scan' || phase === 'waiting_confirmation' || phase === 'need_verify_code'
  );
}

export function WeixinQrCard({
  api,
  expectedRevision,
  onChanged,
}: {
  api: WeixinQrApi;
  expectedRevision: string;
  onChanged: () => void | Promise<void>;
}) {
  const [status, setStatus] = useState<WeixinQrStatus | null>(null);
  const [code, setCode] = useState('');
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const current = useRef<WeixinQrStatus | null>(null);
  const generation = useRef(0);
  const mounted = useRef(false);
  const operationPending = useRef(false);
  const changed = useRef(onChanged);
  const authorizationObserved = useRef<string | null>(null);
  changed.current = onChanged;

  function publish(next: WeixinQrStatus) {
    const previous = current.current;
    current.current = next;
    setStatus(next);
    setError(null);
    if (next.phase !== 'need_verify_code') setCode('');
    if (next.phase === 'authorized' && previous !== null && previous.phase !== 'authorized') {
      const identity = JSON.stringify([next.configuration_revision, next.account_id]);
      if (authorizationObserved.current !== identity) {
        authorizationObserved.current = identity;
        const observedGeneration = generation.current;
        // A config refresh must not convert a saved authorization into a transport Ready claim.
        void Promise.resolve()
          .then(() => {
            if (mounted.current && observedGeneration === generation.current)
              return changed.current();
          })
          .catch(() => {
            if (mounted.current && observedGeneration === generation.current)
              setError('授权已保存，渠道状态刷新失败，请重新载入渠道页。');
          });
      }
    }
  }

  useEffect(() => {
    // 清理时推进同一 ref 的最新代次，不回写 effect 启动时的旧值。
    const invalidate = () => ++generation.current;
    mounted.current = true;
    invalidate();
    operationPending.current = false;
    current.current = null;
    setStatus(null);
    setCode('');
    setBusy(false);
    setError(null);
    let active = true;
    let timer: ReturnType<typeof setTimeout> | undefined;
    let pendingGeneration: number | undefined;
    const poll = async () => {
      if (!operationPending.current && pendingGeneration !== generation.current) {
        const requestGeneration = generation.current;
        pendingGeneration = requestGeneration;
        const challenge = current.current?.challenge_id;
        try {
          const next = await api.status(challenge);
          if (
            active &&
            requestGeneration === generation.current &&
            (!challenge || next.challenge_id === challenge)
          )
            publish(next);
        } catch {
          // IPC failures may contain request material. Only the dedicated DTO exposes diagnostics.
          if (active && requestGeneration === generation.current)
            setError('无法读取微信授权状态，请稍后重试。');
        } finally {
          if (pendingGeneration === requestGeneration) pendingGeneration = undefined;
        }
      }
    };
    const tick = () => {
      void poll();
      // An unresponsive old IPC must not suspend observation of its replacement challenge.
      if (active) timer = setTimeout(tick, 2000);
    };
    tick();
    return () => {
      active = false;
      mounted.current = false;
      invalidate();
      if (timer) clearTimeout(timer);
    };
  }, [api, expectedRevision]);

  async function perform(action: 'begin' | 'verify' | 'cancel' | 'forget') {
    if (operationPending.current) return;
    const previous = current.current;
    const challenge = previous?.challenge_id;
    if ((action === 'verify' || action === 'cancel') && !challenge) return;
    const submittedCode = code.trim();
    if (action === 'verify' && !/^[0-9]+$/.test(submittedCode)) {
      setError('请输入手机上显示的数字校验码。');
      return;
    }
    const requestGeneration = ++generation.current;
    operationPending.current = true;
    setBusy(true);
    setError(null);
    setCode('');
    if (action === 'begin' || action === 'forget') {
      // A superseded QR must disappear immediately, including while the replacement IPC is pending.
      current.current = null;
      setStatus(null);
    }
    const stillCurrent = () => mounted.current && requestGeneration === generation.current;
    try {
      const revision = previous?.configuration_revision ?? expectedRevision;
      let next: WeixinQrStatus;
      if (action === 'forget') {
        await api.forget(revision);
        if (!stillCurrent()) return;
        authorizationObserved.current = null;
        await changed.current();
        if (!stillCurrent()) return;
        next = await api.status();
      } else if (action === 'begin') {
        next = await api.begin(revision);
      } else if (action === 'verify' && challenge) {
        next = await api.verify(challenge, submittedCode);
      } else if (action === 'cancel' && challenge) {
        next = await api.cancel(challenge);
      } else {
        return;
      }
      if (!stillCurrent()) return;
      if (
        (action === 'verify' || action === 'cancel') &&
        next.challenge_id &&
        next.challenge_id !== challenge
      ) {
        setError('扫码状态已更新，请重新载入渠道页。');
        return;
      }
      publish(next);
    } catch {
      if (stillCurrent()) setError('微信授权操作未确认，请检查当前状态后重试。');
    } finally {
      if (stillCurrent()) {
        operationPending.current = false;
        setBusy(false);
      }
    }
  }

  const activeChallenge = status?.challenge_id && isActive(status.phase);
  const qrVisible = status?.qr_svg && activeChallenge;
  const buttonClass =
    'rounded border border-[var(--border-primary)] px-3 py-1.5 text-sm disabled:opacity-50';
  return (
    <Card as="section" className="space-y-3 p-4" aria-label="个人微信扫码授权">
      <h3 className="font-medium">个人微信</h3>
      <p className="text-xs text-[var(--text-secondary)]">
        用微信扫码连接，无需填写密码或应用密钥。
      </p>
      <div role="status" aria-live="polite" className="space-y-1 text-sm">
        <p>{status ? PHASE_LABELS[status.phase] : busy ? '正在处理授权' : '正在读取授权状态'}</p>
        {status && <p>{status.has_credential ? '本机已保存授权' : '本机未保存授权'}</p>}
        {status?.account_id && <p>账号：{status.account_id}</p>}
      </div>
      {qrVisible && (
        <img
          alt="微信登录二维码"
          className="h-56 w-56 rounded bg-white p-2"
          src={`data:image/svg+xml;charset=utf-8,${encodeURIComponent(status?.qr_svg ?? '')}`}
          draggable={false}
          referrerPolicy="no-referrer"
        />
      )}
      {status?.phase === 'need_verify_code' && (
        <form
          className="space-y-2"
          onSubmit={(event) => {
            event.preventDefault();
            void perform('verify');
          }}
        >
          <label className="block text-xs">
            手机数字校验码
            <input
              type="text"
              inputMode="numeric"
              autoComplete="off"
              spellCheck={false}
              aria-label="微信数字校验码"
              className="mt-1 w-full rounded border border-[var(--border-primary)] bg-transparent p-2 text-sm"
              value={code}
              disabled={busy}
              onChange={(event) => setCode(event.target.value)}
            />
          </label>
          <button type="submit" className={buttonClass} disabled={busy || !code.trim()}>
            提交数字校验码
          </button>
        </form>
      )}
      {(error || status?.error) && (
        <p role="alert" className="text-xs text-[var(--color-error-text)]">
          {error ?? status?.error}
        </p>
      )}
      <div className="flex flex-wrap gap-2">
        <button
          type="button"
          className={buttonClass}
          disabled={busy}
          onClick={() => void perform('begin')}
        >
          {activeChallenge ? '刷新微信二维码' : '获取微信二维码'}
        </button>
        {activeChallenge && (
          <button
            type="button"
            className={buttonClass}
            disabled={busy}
            onClick={() => void perform('cancel')}
          >
            取消微信扫码
          </button>
        )}
        {status?.has_credential && (
          <button
            type="button"
            className={buttonClass}
            disabled={busy}
            onClick={() => void perform('forget')}
          >
            清除本机微信授权
          </button>
        )}
      </div>
      <p className="text-xs text-[var(--text-secondary)]">
        扫码授权与连接状态分别确认；是否连接成功请查看渠道连接状态。
      </p>
    </Card>
  );
}
