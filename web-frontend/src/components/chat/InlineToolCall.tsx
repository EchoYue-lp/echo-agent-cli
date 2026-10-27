import { memo, useCallback, useEffect, useState, useRef } from 'react';
import {
  AlertTriangle,
  Check,
  ChevronDown,
  ChevronRight,
  CircleStop,
  Copy,
  LoaderCircle,
  Wrench,
  X,
} from 'lucide-react';
import { toolExecutionApi } from '../../api/endpoints';
import { errorMessage } from '../../lib/tauri-bridge';
import { useToolExecutionStore } from '../../stores/toolExecutionStore';
import {
  transcriptToolExecution,
  type TranscriptToolRecord,
  type ToolExecutionDetailChunk,
  type ToolExecutionDetailManifest,
} from '../../types/api';

interface InlineToolCallProps {
  toolId: string;
  index?: number;
  transcript?: TranscriptToolRecord;
}

const LIVE_DETAIL_AUTOLOAD_CHARS = 256 * 1024;

export function toolSummaryText(name: string, argsPreview: string): string {
  return `${name}${argsPreview ? ` · ${argsPreview}` : ''}`;
}

function formatArgs(args: unknown): string {
  try {
    return JSON.stringify(args, null, 2) ?? String(args ?? '');
  } catch {
    return String(args ?? '');
  }
}

function formatBytes(bytes: number): string {
  if (bytes < 1024) return `${bytes} B`;
  if (bytes < 1024 * 1024) return `${Math.round(bytes / 1024)} KiB`;
  return `${(bytes / (1024 * 1024)).toFixed(1)} MiB`;
}

interface BoundDetailState {
  binding: string | null;
  manifest: ToolExecutionDetailManifest | null;
  chunks: ToolExecutionDetailChunk[];
  cursor: string | null;
  complete: boolean;
  loading: boolean;
  loadError: string | null;
}

function emptyDetail(binding: string | null): BoundDetailState {
  return {
    binding,
    manifest: null,
    chunks: [],
    cursor: null,
    complete: false,
    loading: false,
    loadError: null,
  };
}

export const InlineToolCall = memo(function InlineToolCall({
  toolId,
  transcript,
}: InlineToolCallProps) {
  const tool = useToolExecutionStore((state) =>
    transcript ? transcriptToolExecution(transcript, state.tools) : state.tools[toolId]
  );
  const bindingKey = tool
    ? JSON.stringify([
        tool.workspace_id,
        tool.conversation_id ?? null,
        tool.run_id ?? null,
        tool.owner.kind === 'chat'
          ? ['chat', tool.owner.message_id]
          : ['subagent', tool.owner.subagent_run_id],
        tool.call_id,
        tool.name,
        tool.id,
        tool.detail_ref,
      ])
    : null;
  const [expanded, setExpanded] = useState(false);
  const [detail, setDetail] = useState<BoundDetailState>(() => emptyDetail(bindingKey));
  const detailRef = useRef(detail);
  const bindingGeneration = useRef(0);
  const activeRequest = useRef<{ binding: string; generation: number } | null>(null);
  const [copied, setCopied] = useState<string | null>(null);
  const [now, setNow] = useState(Date.now());
  const publishDetail = useCallback((next: BoundDetailState) => {
    detailRef.current = next;
    setDetail(next);
  }, []);

  useEffect(() => {
    bindingGeneration.current += 1;
    activeRequest.current = null;
    publishDetail(emptyDetail(bindingKey));
    setCopied(null);
    return () => {
      bindingGeneration.current += 1;
      activeRequest.current = null;
    };
  }, [bindingKey, publishDetail]);

  // A newly rendered binding must never expose the previous binding's data,
  // even before the effect above has reset its local request state.
  const scopedDetail = detail.binding === bindingKey ? detail : emptyDetail(bindingKey);
  const { manifest, chunks, complete, loading, loadError } = scopedDetail;

  useEffect(() => {
    if (tool?.status !== 'running') return;
    const timer = window.setInterval(() => setNow(Date.now()), 250);
    return () => window.clearInterval(timer);
  }, [tool?.status]);

  const loadPage = useCallback(
    async (initial: boolean) => {
      const current = detailRef.current;
      if (
        !tool ||
        !tool.detail_ref ||
        !bindingKey ||
        current.binding !== bindingKey ||
        activeRequest.current
      )
        return;
      if (current.complete) return;
      const request = { binding: bindingKey, generation: bindingGeneration.current };
      activeRequest.current = request;
      publishDetail({ ...current, loading: true, loadError: null });
      const isCurrent = () =>
        activeRequest.current === request &&
        bindingGeneration.current === request.generation &&
        detailRef.current.binding === request.binding;
      try {
        const [nextManifest, page] = await Promise.all([
          initial || !current.manifest || current.manifest.status !== tool.status
            ? toolExecutionApi.detail(tool.workspace_id, tool.detail_ref)
            : Promise.resolve(current.manifest),
          toolExecutionApi.readOutput(
            tool.workspace_id,
            tool.detail_ref,
            initial ? null : current.cursor
          ),
        ]);
        if (!isCurrent()) return;
        if (nextManifest.id !== tool.id) throw new Error('工具详情与当前执行身份不一致');
        publishDetail({
          ...detailRef.current,
          manifest: nextManifest,
          chunks: initial ? page.chunks : [...detailRef.current.chunks, ...page.chunks],
          cursor: page.next_cursor ?? null,
          complete: page.complete,
        });
      } catch (error) {
        if (!isCurrent()) return;
        publishDetail({ ...detailRef.current, loadError: errorMessage(error) });
      } finally {
        if (isCurrent()) {
          activeRequest.current = null;
          publishDetail({ ...detailRef.current, loading: false });
        }
      }
    },
    [bindingKey, publishDetail, tool]
  );

  useEffect(() => {
    if (!expanded || manifest || loadError || !tool?.detail_ref) return;
    void loadPage(true);
  }, [expanded, loadError, loadPage, manifest, tool]);

  useEffect(() => {
    const loadedCharacters = chunks.reduce((total, chunk) => total + chunk.text.length, 0);
    if (
      !expanded ||
      !tool?.detail_ref ||
      tool.status !== 'running' ||
      loadError ||
      loadedCharacters >= LIVE_DETAIL_AUTOLOAD_CHARS
    ) {
      return;
    }
    const timer = window.setInterval(() => void loadPage(false), 500);
    return () => window.clearInterval(timer);
  }, [chunks, expanded, loadError, loadPage, tool]);

  useEffect(() => {
    if (
      !expanded ||
      !tool?.detail_ref ||
      tool.status === 'running' ||
      manifest?.status !== 'running'
    ) {
      return;
    }
    void loadPage(false);
  }, [expanded, loadPage, manifest?.status, tool]);

  if (!tool && !transcript) return null;

  const status = tool?.status ?? 'unknown';
  const name = tool?.name ?? transcript?.name ?? '工具记录';
  const duration = tool
    ? (tool.duration_ms ?? Math.max(0, (tool.finished_at ?? now) - tool.started_at)) / 1000
    : null;
  const statusIcon =
    status === 'running' ? (
      <LoaderCircle size={12} className="animate-spin text-[var(--accent)]" />
    ) : status === 'succeeded' ? (
      <Check size={12} className="text-[var(--color-success)]" />
    ) : status === 'failed' || status === 'timed_out' ? (
      <X size={12} className="text-[var(--color-error)]" />
    ) : !tool ? (
      <AlertTriangle size={12} className="text-[var(--color-warning)]" />
    ) : (
      <CircleStop size={12} className="text-[var(--text-tertiary)]" />
    );
  const currentManifest = tool && manifest?.id === tool.id ? manifest : null;
  const argsText = currentManifest ? formatArgs(currentManifest.invocation.args) : '';
  const outputText = chunks.map((chunk) => `[${chunk.channel}]\n${chunk.text}`).join('\n');
  const summary = toolSummaryText(name, tool?.args_preview ?? '');
  const loadedCharacters = chunks.reduce((total, chunk) => total + chunk.text.length, 0);
  const liveLoadingPaused = status === 'running' && loadedCharacters >= LIVE_DETAIL_AUTOLOAD_CHARS;
  const statusText = !tool
    ? '状态未恢复'
    : status === 'running'
      ? 'running'
      : status === 'succeeded'
        ? 'success'
        : status === 'failed'
          ? 'failed'
          : status === 'cancelled'
            ? 'cancelled'
            : status === 'timed_out'
              ? 'timed out'
              : status === 'interrupted'
                ? 'interrupted'
                : 'unknown';

  const copyText = async (text: string, label: string) => {
    const generation = bindingGeneration.current;
    await navigator.clipboard.writeText(text);
    if (bindingGeneration.current !== generation || detailRef.current.binding !== bindingKey)
      return;
    setCopied(label);
    window.setTimeout(() => {
      if (bindingGeneration.current === generation) setCopied(null);
    }, 1500);
  };

  return (
    <div className="my-1 min-w-0 pl-2 font-mono text-[11px]">
      <button
        type="button"
        onClick={() => setExpanded((value) => !value)}
        className="flex min-h-6 w-full min-w-0 items-center gap-1.5 py-0.5 text-left"
        title={summary}
      >
        {expanded ? <ChevronDown size={11} /> : <ChevronRight size={11} />}
        <span className="shrink-0">{statusIcon}</span>
        <Wrench size={12} className="shrink-0 text-[var(--text-tertiary)]" />
        <span className="min-w-0 flex-1 truncate leading-5 text-[var(--text-primary)]">
          <span>{name}</span>
          {(tool?.args_preview ?? '') && (
            <span className="ml-1.5 text-[var(--text-tertiary)]">· {tool?.args_preview ?? ''}</span>
          )}
        </span>
        <span className="shrink-0 text-[10px] text-[var(--text-tertiary)]">{statusText}</span>
        <span className="shrink-0 tabular-nums text-[var(--text-tertiary)]">
          {duration != null ? `${duration.toFixed(1)}s` : null}
        </span>
      </button>

      {expanded && (
        <div className="ml-5 mt-1 min-w-0 border-l border-[var(--border-primary)] pl-3">
          {!tool?.detail_ref && !transcript && (
            <div className="text-[10px] text-[var(--text-tertiary)]">仅保留工具执行摘要</div>
          )}
          {transcript && (
            <section className="mb-3 space-y-2 text-[10px] text-[var(--text-secondary)]">
              {!tool && <p>状态未恢复；以下仅为原始请求与会话记录。</p>}
              {transcript.requestedArguments != null && (
                <section>
                  <div className="mb-1 text-[var(--text-tertiary)]">原始请求参数</div>
                  <pre className="max-h-64 overflow-auto whitespace-pre-wrap break-words">
                    {transcript.requestedArguments}
                  </pre>
                </section>
              )}
              {transcript.recordedOutput != null && (
                <section>
                  <div className="mb-1 text-[var(--text-tertiary)]">原始记录正文</div>
                  <pre className="max-h-96 overflow-auto whitespace-pre-wrap break-words">
                    {transcript.recordedOutput}
                  </pre>
                </section>
              )}
            </section>
          )}
          {loadError && (
            <div className="mb-2 flex items-center gap-1.5 text-[var(--color-error)]">
              <AlertTriangle size={11} />
              <span>{loadError}</span>
              <button type="button" disabled={loading} onClick={() => void loadPage(true)}>
                重试加载
              </button>
            </div>
          )}
          {currentManifest && (
            <>
              <section className="mb-3">
                <div className="mb-1 flex items-center justify-between text-[9px] uppercase text-[var(--text-tertiary)]">
                  <span>参数</span>
                  <button type="button" onClick={() => copyText(argsText, 'args')}>
                    {copied === 'args' ? '已复制' : '复制'}
                  </button>
                </div>
                <pre className="max-h-64 overflow-auto whitespace-pre-wrap break-words bg-[var(--bg-code)] p-2 text-[10px] leading-relaxed text-[var(--color-code-text)]">
                  {argsText}
                </pre>
              </section>

              <section>
                <div className="mb-1 flex items-center justify-between text-[9px] uppercase text-[var(--text-tertiary)]">
                  <span>
                    输出 · {formatBytes(currentManifest.output_bytes)}
                    {currentManifest.result?.truncated ? ' · Agent 上下文已截断' : ''}
                  </span>
                  {outputText && (
                    <button type="button" onClick={() => copyText(outputText, 'output')}>
                      {copied === 'output' ? '已复制' : <Copy size={10} />}
                    </button>
                  )}
                </div>
                {currentManifest.result?.failure && (
                  <div className="mb-2 border-l-2 border-[var(--color-error)] pl-2 text-[10px] text-[var(--text-secondary)]">
                    {currentManifest.result.failure.category} ·{' '}
                    {currentManifest.result.failure.recovery}
                  </div>
                )}
                {currentManifest.result &&
                  Object.keys(currentManifest.result.metadata).length > 0 && (
                    <details className="mb-2 text-[10px] text-[var(--text-secondary)]">
                      <summary className="cursor-pointer text-[var(--text-tertiary)]">
                        metadata
                      </summary>
                      <pre className="mt-1 overflow-auto whitespace-pre-wrap break-words bg-[var(--bg-code)] p-2 text-[10px] leading-relaxed text-[var(--color-code-text)]">
                        {formatArgs(currentManifest.result.metadata)}
                      </pre>
                    </details>
                  )}
                <div className="max-h-96 space-y-2 overflow-auto bg-[var(--bg-code)] p-2">
                  {chunks.map((chunk, index) => (
                    <section key={`${chunk.channel}-${index}`}>
                      <div className="mb-0.5 text-[9px] text-[var(--text-tertiary)]">
                        {chunk.channel}
                      </div>
                      <pre className="whitespace-pre-wrap break-words text-[10px] leading-relaxed text-[var(--color-code-text)]">
                        {chunk.text}
                      </pre>
                    </section>
                  ))}
                  {chunks.length === 0 && !loading && (
                    <div className="text-[10px] text-[var(--text-tertiary)]">暂无输出</div>
                  )}
                </div>
                {liveLoadingPaused && (
                  <div className="mt-2 text-[10px] text-[var(--text-tertiary)]">
                    实时加载已暂停，避免长日志占满页面内存。
                  </div>
                )}
                {!complete && (status !== 'running' || liveLoadingPaused) && (
                  <button
                    type="button"
                    onClick={() => void loadPage(false)}
                    disabled={loading}
                    className="mt-2 text-[10px] text-[var(--accent)]"
                  >
                    {loading ? '加载中...' : '加载更多'}
                  </button>
                )}
              </section>
            </>
          )}
          {loading && !manifest && (
            <div className="text-[10px] text-[var(--text-tertiary)]">正在加载完整信息...</div>
          )}
        </div>
      )}
    </div>
  );
});
