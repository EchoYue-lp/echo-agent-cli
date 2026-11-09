import type { ConversationUsageSnapshot } from '../../types/api';

function tokens(value: number): string {
  return new Intl.NumberFormat('zh-CN', { notation: 'compact', maximumFractionDigits: 1 }).format(
    value
  );
}

export function ConversationStatistics({
  streaming,
  value,
  error,
}: {
  streaming: boolean;
  value: ConversationUsageSnapshot | null;
  error: string | null;
}) {
  const available = value?.journal_identity != null ? value : null;
  const main = available?.main;
  const cache =
    main && main.input_tokens > 0 && main.conflicts === 0
      ? `${Math.round((main.cached_tokens * 1000) / main.input_tokens) / 10}%`
      : '--';
  const known =
    value?.prefix_complete && !main?.missing_usage && !main?.conflicts && !main?.pending_calls;
  return (
    <details
      className="group relative min-w-0 max-w-full sm:flex-initial flex-1"
      aria-label="主链运行统计明细"
    >
      <summary className="flex cursor-pointer list-none items-center justify-center gap-x-1 overflow-hidden whitespace-nowrap rounded focus-visible:outline-2 focus-visible:outline-[var(--accent)] sm:gap-x-3">
        <span className="shrink-0">
          {value?.turns_known ? value.turns : '--'} 轮 ·{' '}
          {main && !main.unidentified_steps ? main.steps : '--'} 步
        </span>
        <span className="shrink-0" title="客户端观测：最近一次完成且计数/时段可比较的主链尝试">
          {streaming ? '上次 ' : ''}
          {main?.latest_speed == null ? '--' : Math.round(main.latest_speed)} tok/s
        </span>
        <span
          className="min-w-0 truncate"
          aria-label={`主链 ${main ? tokens(main.input_tokens + main.output_tokens) : '--'} tok`}
          title="主链实际已报告的输入与输出累计，不含继承 Fork 前缀及 Subagent"
        >
          <span className="hidden sm:inline">主链 </span>
          {main ? tokens(main.input_tokens + main.output_tokens) : '--'} tok
        </span>
        <span className="shrink-0">缓存 {cache}</span>
        {(!known || error) && (
          <span className="shrink-0" title={error ? '统计暂不可读' : '覆盖不完整'}>
            <span className="sm:hidden" aria-hidden="true">
              !
            </span>
            <span className="sr-only sm:not-sr-only">{error ? '统计暂不可读' : '覆盖不完整'}</span>
          </span>
        )}
      </summary>
      <div className="absolute bottom-full left-0 z-50 mb-2 max-h-80 w-80 max-w-[calc(100vw-2rem)] overflow-y-auto break-words rounded-lg border border-[var(--border-secondary)] bg-[var(--bg-primary)] p-3 text-left leading-6 shadow-[var(--shadow-md)]">
        <p>
          主链累计：输入 {main?.input_tokens ?? '--'} / 输出 {main?.output_tokens ?? '--'}；命中{' '}
          {main?.cached_tokens ?? '--'} / 写缓存 {main?.cache_write_tokens ?? '--'}。
        </p>
        <p>
          逻辑调用 {main && !main.unidentified_steps ? main.steps : '--'}；实际尝试{' '}
          {main?.attempts ?? '--'}；未报告用量 {main?.missing_usage ?? '--'}；身份不全{' '}
          {main?.unidentified_steps ?? '--'}；冲突 {main?.conflicts ?? '--'}；尚未返回用量{' '}
          {main?.pending_calls ?? '--'}。
        </p>
        <p>
          会话观测速率：
          {main && main.output_duration_ms > 0
            ? Math.round((main.timed_output_tokens * 1000) / main.output_duration_ms)
            : '--'}{' '}
          tok/s（合格输出总数 / 合格时段总长）。
        </p>
        <p>
          Subagent：输入 {available?.subagent.input_tokens ?? '--'} / 输出{' '}
          {available?.subagent.output_tokens ?? '--'}；辅助调用（标题、压缩等）：未覆盖。
        </p>
        {value &&
          Object.entries(value.models).map(([model, usage]) => (
            <p key={model}>
              {model}：输入 {usage.input_tokens} / 输出 {usage.output_tokens}，{usage.attempts}{' '}
              次尝试。
            </p>
          ))}
        <p>
          原日志范围：{value?.first_observed_sequence ?? '--'}–{value?.through_sequence ?? '--'} /{' '}
          {value?.latest_sequence ?? '--'}；
          {value?.prefix_complete ? '前缀已覆盖' : '前缀未获完整证明'}。
        </p>
        <p>
          缓存率按已报告主链输入加权，写缓存不算命中。失败请求已报告的用量仍累计；编辑、回退不冲销费用。Fork
          继承前缀不计入目标账目，来源见原消息。
        </p>
        {error && <p role="status">{error}</p>}
      </div>
    </details>
  );
}
