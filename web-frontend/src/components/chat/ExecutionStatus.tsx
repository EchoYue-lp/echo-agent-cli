import { useEffect, useState } from 'react';
import { useChatStore } from '../../stores/chatStore';

/** 操作等待是运行观测，不作为助手新发现或最终回答。 */
export function ExecutionStatus() {
  const activity = useChatStore((state) => state.activity);
  const active = useChatStore((state) => state.isStreaming);
  const [now, setNow] = useState(Date.now);
  const running = active && activity !== null;
  useEffect(() => {
    if (!running) return;
    const timer = window.setInterval(() => setNow(Date.now()), 1000);
    return () => window.clearInterval(timer);
  }, [running]);
  if (!active || !activity) return null;
  const elapsed = Math.max(0, Math.floor((now - activity.startedAt) / 1000));
  return (
    <div
      className="flex flex-wrap items-center gap-x-3 gap-y-1 px-1 py-2 text-xs text-[var(--text-tertiary)]"
      aria-label="当前执行状态"
    >
      <span>{activity.label}</span>
      <span>已用时 {elapsed} 秒</span>
      <span>
        最后事件{' '}
        <time dateTime={new Date(activity.lastEventAt).toISOString()}>
          {new Date(activity.lastEventAt).toLocaleTimeString()}
        </time>
      </span>
    </div>
  );
}
