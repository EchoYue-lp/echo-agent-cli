import { memo, useCallback, useEffect, useState } from 'react';
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
import type { ToolExecutionDetailChunk, ToolExecutionDetailManifest } from '../../types/api';

interface InlineToolCallProps {
  toolId: string;
  index?: number;
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

export const InlineToolCall = memo(function InlineToolCall({ toolId }: InlineToolCallProps) {
  const tool = useToolExecutionStore((state) => state.tools[toolId]);
  const [expanded, setExpanded] = useState(false);
  const [manifest, setManifest] = useState<ToolExecutionDetailManifest | null>(null);
  const [chunks, setChunks] = useState<ToolExecutionDetailChunk[]>([]);
  const [cursor, setCursor] = useState<string | null>(null);
  const [complete, setComplete] = useState(false);
  const [loading, setLoading] = useState(false);
  const [loadError, setLoadError] = useState<string | null>(null);
  const [copied, setCopied] = useState<string | null>(null);
  const [now, setNow] = useState(Date.now());

  useEffect(() => {
    if (tool?.status !== 'running') return;
    const timer = window.setInterval(() => setNow(Date.now()), 250);
    return () => window.clearInterval(timer);
  }, [tool?.status]);

  const loadPage = useCallback(
    async (initial: boolean) => {
      if (!tool || !tool.detail_ref || loading || complete) return;
      setLoading(true);
      setLoadError(null);
      try {
        const [nextManifest, page] = await Promise.all([
          initial || !manifest || manifest.status !== tool.status
            ? toolExecutionApi.detail(tool.detail_ref)
            : Promise.resolve(manifest),
          toolExecutionApi.readOutput(tool.detail_ref, initial ? null : cursor),
        ]);
        setManifest(nextManifest);
        setChunks((current) => (initial ? page.chunks : [...current, ...page.chunks]));
        setCursor(page.next_cursor ?? null);
        setComplete(page.complete);
      } catch (error) {
        setLoadError(errorMessage(error));
      } finally {
        setLoading(false);
      }
    },
    [complete, cursor, loading, manifest, tool]
  );

  useEffect(() => {
    if (!expanded || manifest || !tool?.detail_ref) return;
    void loadPage(true);
  }, [expanded, loadPage, manifest, tool]);

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

  if (!tool) return null;

  const duration =
    (tool.duration_ms ?? Math.max(0, (tool.finished_at ?? now) - tool.started_at)) / 1000;
  const statusIcon =
    tool.status === 'running' ? (
      <LoaderCircle size={12} className="animate-spin text-[var(--accent)]" />
    ) : tool.status === 'succeeded' ? (
      <Check size={12} className="text-[var(--color-success)]" />
    ) : tool.status === 'failed' || tool.status === 'timed_out' ? (
      <X size={12} className="text-[var(--color-error)]" />
    ) : (
      <CircleStop size={12} className="text-[var(--text-tertiary)]" />
    );
  const argsText = manifest ? formatArgs(manifest.invocation.args) : '';
  const outputText = chunks.map((chunk) => `[${chunk.channel}]\n${chunk.text}`).join('\n');
  const summary = toolSummaryText(tool.name, tool.args_preview);
  const loadedCharacters = chunks.reduce((total, chunk) => total + chunk.text.length, 0);
  const liveLoadingPaused =
    tool.status === 'running' && loadedCharacters >= LIVE_DETAIL_AUTOLOAD_CHARS;
  const statusText =
    tool.status === 'running'
      ? 'running'
      : tool.status === 'succeeded'
        ? 'success'
        : tool.status === 'failed'
          ? 'failed'
          : tool.status === 'cancelled'
            ? 'cancelled'
            : tool.status === 'timed_out'
              ? 'timed out'
              : tool.status === 'interrupted'
                ? 'interrupted'
                : 'unknown';

  const copyText = async (text: string, label: string) => {
    await navigator.clipboard.writeText(text);
    setCopied(label);
    window.setTimeout(() => setCopied(null), 1500);
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
          <span>{tool.name}</span>
          {tool.args_preview && (
            <span className="ml-1.5 text-[var(--text-tertiary)]">· {tool.args_preview}</span>
          )}
        </span>
        <span className="shrink-0 text-[10px] text-[var(--text-tertiary)]">{statusText}</span>
        <span className="shrink-0 tabular-nums text-[var(--text-tertiary)]">
          {duration.toFixed(1)}s
        </span>
      </button>

      {expanded && (
        <div className="ml-5 mt-1 min-w-0 border-l border-[var(--border-primary)] pl-3">
          {!tool.detail_ref && (
            <div className="text-[10px] text-[var(--text-tertiary)]">仅保留工具执行摘要</div>
          )}
          {loadError && (
            <div className="mb-2 flex items-center gap-1.5 text-[var(--color-error)]">
              <AlertTriangle size={11} />
              <span>{loadError}</span>
            </div>
          )}
          {manifest && (
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
                    输出 · {formatBytes(manifest.output_bytes)}
                    {manifest.result?.truncated ? ' · Agent 上下文已截断' : ''}
                  </span>
                  {outputText && (
                    <button type="button" onClick={() => copyText(outputText, 'output')}>
                      {copied === 'output' ? '已复制' : <Copy size={10} />}
                    </button>
                  )}
                </div>
                {manifest.result?.failure && (
                  <div className="mb-2 border-l-2 border-[var(--color-error)] pl-2 text-[10px] text-[var(--text-secondary)]">
                    {manifest.result.failure.category} · {manifest.result.failure.recovery}
                  </div>
                )}
                {manifest.result && Object.keys(manifest.result.metadata).length > 0 && (
                  <details className="mb-2 text-[10px] text-[var(--text-secondary)]">
                    <summary className="cursor-pointer text-[var(--text-tertiary)]">
                      metadata
                    </summary>
                    <pre className="mt-1 overflow-auto whitespace-pre-wrap break-words bg-[var(--bg-code)] p-2 text-[10px] leading-relaxed text-[var(--color-code-text)]">
                      {formatArgs(manifest.result.metadata)}
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
                {!complete && (tool.status !== 'running' || liveLoadingPaused) && (
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
