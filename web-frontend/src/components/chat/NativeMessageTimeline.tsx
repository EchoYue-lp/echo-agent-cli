import { useId, useState } from 'react';
import { Brain, ChevronDown, ChevronRight, Wrench } from 'lucide-react';
import {
  isNativeTurnSource,
  type AssistantBlockView,
  type ChatMessage,
  type NativeJournalCursor,
  type ToolExecution,
} from '../../types/api';
import {
  isNativeJournalCursor,
  recordBelongsToTurn,
  uniquePresentationRecords,
} from '../../lib/nativePresentation';
import { useSubagentRunStore, type SubagentRunState } from '../../stores/subagentRunStore';
import { SubagentStreamBlock } from './SubagentStreamBlock';
import { useToolExecutionStore } from '../../stores/toolExecutionStore';
import MarkdownContent from '../common/MarkdownContent';
import { ToolExecutionGroup, toolExecutionGroupPresentation } from './ToolExecutionGroup';

/** 只读取原 native 块的公开文字；签名、redacted 和 opaque data 不作正文回退。 */
export function nativeAssistantBlockText(view: AssistantBlockView): string {
  if (view.content.trim() || view.block.phase !== 'reasoning') return view.content;
  const reasoning = view.block.reasoning;
  if (!reasoning || typeof reasoning !== 'object') return '';
  const value = reasoning as { type?: unknown; thinking?: unknown; summary?: unknown };
  if (value.type === 'signed' && typeof value.thinking === 'string') return value.thinking;
  if (
    value.type === 'opaque' &&
    Array.isArray(value.summary) &&
    value.summary.every((part) => typeof part === 'string')
  )
    return value.summary.join('\n\n');
  return '';
}

type ProcessEntry =
  | { type: 'reasoning'; key: string; content: string; cursor?: NativeJournalCursor | null }
  | {
      type: 'tool';
      key: string;
      callId: string;
      message: ChatMessage;
      cursor?: NativeJournalCursor | null;
    }
  | { type: 'subagent'; key: string; run: SubagentRunState; cursor: NativeJournalCursor };

type TimelineItem =
  | { type: 'body'; key: string; content: string; cursor?: NativeJournalCursor | null }
  | { type: 'process'; key: string; entries: ProcessEntry[] };

/** 分组只影响呈现；保留原块身份、原工具顺序和既有最终发布去重边界。 */
function messageTimelineItems(message: ChatMessage): TimelineItem[] {
  const blocks = message.assistantBlocks ?? [];
  const steps =
    message.executionSteps ?? blocks.map((_, index) => ({ type: 'assistant' as const, index }));
  const status = message.executionStatus;
  const source = message.finalAnswerSource;
  const selected =
    message.finalAnswerConfirmed &&
    status?.status === 'settled' &&
    source &&
    message.finalAnswerExecutionId === source.execution_id &&
    status.execution_id === source.execution_id &&
    status.request_id === source.request_id &&
    status.sequence === source.sequence
      ? source
      : undefined;
  const renderedBlockIds = new Set<string>();
  const items: TimelineItem[] = [];
  const appendProcess = (entry: ProcessEntry) => {
    const previous = items.at(-1);
    if (previous?.type === 'process') previous.entries.push(entry);
    else items.push({ type: 'process', key: entry.key, entries: [entry] });
  };
  for (const step of steps) {
    if (step.type === 'tool') {
      appendProcess({ type: 'tool', key: `tool:${step.callId}`, callId: step.callId, message });
      continue;
    }
    if (step.type !== 'assistant') continue;
    const view = blocks[step.index];
    if (!view) continue;
    const { block } = view;
    const content = nativeAssistantBlockText(view);
    if (!content.trim() || block.phase === 'final_answer') continue;
    // 仅原 Settled 三元和 approved block 精确身份允许抑制重复的 Unknown 最终候选。
    if (
      block.phase === 'unknown' &&
      selected &&
      view.execution_id === selected.execution_id &&
      selected.blocks.some(
        (identity) =>
          identity.request_id === block.request_id &&
          identity.block_id === block.block_id &&
          identity.sequence === block.sequence &&
          identity.publication_event_id === view.publication_event_id
      )
    )
      continue;
    // 原块ID只在所属消息内唯一；整轮合并时保留消息容器，避免不同来源复用同一DOM节点。
    const identity = JSON.stringify([
      message.id,
      view.execution_id ?? null,
      block.request_id,
      block.block_id,
    ]);
    if (renderedBlockIds.has(identity)) continue;
    renderedBlockIds.add(identity);
    if (block.phase === 'reasoning')
      appendProcess({ type: 'reasoning', key: identity, content, cursor: view.journal_source });
    else items.push({ type: 'body', key: identity, content, cursor: view.journal_source });
  }
  return items;
}

export function nativeTimelineItems(input: ChatMessage | readonly ChatMessage[]): TimelineItem[] {
  const messages: readonly ChatMessage[] = Array.isArray(input) ? input : [input as ChatMessage];
  const items: TimelineItem[] = [];
  for (const message of messages) {
    const next = messageTimelineItems(message);
    if (!message.assistantBlocks?.length && message.content.trim() && !message.finalAnswerConfirmed)
      next.unshift({ type: 'body', key: `message:${message.id}`, content: message.content });
    for (const item of next) {
      const previous = items.at(-1);
      if (item.type === 'process' && previous?.type === 'process')
        previous.entries.push(...item.entries);
      else items.push(item);
    }
  }
  return items;
}

export interface NativeTimelineProjection {
  items: TimelineItem[];
  subagents: SubagentRunState[];
  unplaced: SubagentRunState[];
}

/** 合入实际parent关系和同一原journal游标；无游标不以时间或最后消息猜位置。 */
export function projectNativeTimeline(
  input: ChatMessage | readonly ChatMessage[],
  runs: readonly SubagentRunState[],
  tools: Readonly<Record<string, ToolExecution>>
): NativeTimelineProjection {
  const messages: readonly ChatMessage[] = Array.isArray(input) ? input : [input as ChatMessage];
  const original = nativeTimelineItems(messages);
  const source = messages.map((message) => message.turnSource).find(isNativeTurnSource);
  if (!source) return { items: original, subagents: [], unplaced: [] };
  const records = uniquePresentationRecords(
    messages.flatMap((message) => message.presentationRecords ?? [])
  ).filter((record) => recordBelongsToTurn(record, source));
  type Atom =
    | Extract<TimelineItem, { type: 'body' }>
    | { type: 'entry'; entry: ProcessEntry; cursor?: NativeJournalCursor | null };
  const atoms: Atom[] = original.flatMap<Atom>((item) =>
    item.type === 'body'
      ? [item]
      : item.entries.map((entry) => {
          let cursor = entry.cursor;
          if (entry.type === 'tool') {
            const transcript = entry.message.transcriptTools?.find(
              (record) => record.id === entry.callId && !record.identityAmbiguous
            );
            const callId = transcript?.callId ?? tools[entry.callId]?.call_id;
            const matches = records.filter(
              (record) =>
                record.payload.kind === 'tool_call' && record.payload.data.call_id === callId
            );
            if (matches.length === 1) cursor = matches.at(0)?.cursor;
          }
          return { type: 'entry' as const, entry, cursor };
        })
  );
  const subagents = runs.filter(
    (run) =>
      run.workspaceId === source.workspace_id &&
      run.conversationId === source.conversation_id &&
      run.events.some((event) => event.framework_event?.parent_execution_id === source.execution_id)
  );
  const insertions = new Map<number, Extract<ProcessEntry, { type: 'subagent' }>[]>();
  const unplaced: SubagentRunState[] = [];
  for (const run of subagents) {
    const starts = run.events
      .filter(
        (event) =>
          event.event === 'started' &&
          event.framework_event?.parent_execution_id === source.execution_id
      )
      .map((event) => ({
        stream_id: event.journal_stream_id ?? '',
        sequence: event.journal_sequence ?? 0,
      }))
      .filter(isNativeJournalCursor);
    const cursor = starts.at(0);
    if (!cursor || starts.some((value) => value.stream_id !== cursor.stream_id)) {
      unplaced.push(run);
      continue;
    }
    const parentIds = new Set(
      run.events.flatMap((event) => (event.parent_event_id ? [event.parent_event_id] : []))
    );
    const parentTools = records.filter(
      (record) =>
        record.payload.kind === 'tool_call' &&
        parentIds.has(record.payload.data.publication_event_id)
    );
    const anchor = parentTools.length === 1 ? (parentTools.at(0)?.cursor ?? cursor) : cursor;
    let before = -1;
    let after = atoms.length;
    atoms.forEach((atom, index) => {
      if (!isNativeJournalCursor(atom.cursor) || atom.cursor.stream_id !== anchor.stream_id) return;
      if (atom.cursor.sequence <= anchor.sequence) before = index;
      else if (after === atoms.length) after = index;
    });
    if (
      (before === -1 && after === atoms.length) ||
      atoms.slice(before + 1, after).some((atom) => !isNativeJournalCursor(atom.cursor))
    ) {
      unplaced.push(run);
      continue;
    }
    const position = before + 1;
    const entries = insertions.get(position) ?? [];
    entries.push({
      type: 'subagent',
      key: `subagent:${run.runId}:${run.subagentRunId}`,
      run,
      cursor,
    });
    insertions.set(position, entries);
  }
  const items: TimelineItem[] = [];
  const append = (entry: ProcessEntry) => {
    const previous = items.at(-1);
    if (previous?.type === 'process') previous.entries.push(entry);
    else items.push({ type: 'process', key: entry.key, entries: [entry] });
  };
  for (let index = 0; index <= atoms.length; index += 1) {
    for (const entry of (insertions.get(index) ?? []).sort(
      (a, b) => a.cursor.sequence - b.cursor.sequence
    ))
      append(entry);
    const atom = atoms.at(index);
    if (atom?.type === 'body') items.push(atom);
    else if (atom?.type === 'entry') append(atom.entry);
  }
  return { items, subagents, unplaced };
}

interface ProcessChoice {
  anchor: string;
  expanded: boolean;
  order: number;
  explicit: boolean;
}

/** 两段公开正文之间的连续思考与工具只占一行，展开后仍按原序读取详情。 */
function NativeProcessSegment({
  item,
  choice,
  onChoose,
  onFocus,
  targetScope,
}: {
  item: Extract<TimelineItem, { type: 'process' }>;
  choice?: ProcessChoice;
  onChoose: (expanded: boolean) => void;
  onFocus: () => void;
  targetScope?: { workspaceId: string; conversationId: string | null };
}) {
  const expanded = choice?.expanded ?? false;
  const detailId = useId();
  const tools = useToolExecutionStore((state) => state.tools);
  const toolIds = item.entries.flatMap((entry) => (entry.type === 'tool' ? [entry.callId] : []));
  const transcriptTools = item.entries.flatMap((entry) =>
    entry.type === 'tool' ? (entry.message.transcriptTools ?? []) : []
  );
  const presentation = toolExecutionGroupPresentation(toolIds, tools, transcriptTools);
  const reasoningCount = item.entries.filter((entry) => entry.type === 'reasoning').length;
  const subagents = item.entries.filter((entry) => entry.type === 'subagent');
  const label =
    [
      toolIds.length > 0 ? presentation.label : '',
      subagents.length > 0 ? `${subagents.length} 个子代理` : '',
    ]
      .filter(Boolean)
      .join(' · ') || '思考';
  const action = toolIds.length > 0 || subagents.length > 0 ? '中间过程' : '思考';
  const suffix = [
    presentation.failedCount > 0 ? `${presentation.failedCount} 个失败` : '',
    presentation.cancelledCount > 0 ? `${presentation.cancelledCount} 个已取消` : '',
    presentation.uncertainCount > 0 ? `${presentation.uncertainCount} 个状态未知` : '',
  ]
    .filter(Boolean)
    .join(' · ');
  return (
    <div className="min-w-0" onFocusCapture={onFocus}>
      <button
        type="button"
        className="flex min-h-7 w-full min-w-0 items-center gap-1.5 rounded-md px-1 py-0.5 text-left text-xs text-[var(--text-tertiary)] hover:bg-[var(--bg-hover)] hover:text-[var(--text-secondary)]"
        aria-expanded={expanded}
        aria-controls={detailId}
        aria-label={`${expanded ? '折叠' : '展开'}${action}`}
        onClick={() => onChoose(!expanded)}
      >
        {expanded ? <ChevronDown size={12} /> : <ChevronRight size={12} />}
        {toolIds.length > 0 ? <Wrench size={13} /> : <Brain size={13} />}
        <span className="min-w-0 truncate">{label}</span>
        {reasoningCount > 0 && toolIds.length > 0 && <span>· 含思考</span>}
        {suffix && (
          <span
            className={`shrink-0 ${presentation.failedCount > 0 ? 'text-[var(--color-error)]' : ''}`}
          >
            · {suffix}
          </span>
        )}
      </button>
      {expanded && (
        <div
          id={detailId}
          className="ml-3 space-y-2 border-l border-[var(--border-primary)] py-1 pl-3"
        >
          {item.entries.map((entry) => (
            <div key={entry.key} data-process-entry={entry.key}>
              {entry.type === 'reasoning' ? (
                <MarkdownContent
                  content={entry.content}
                  className="text-sm text-[var(--text-secondary)]"
                />
              ) : entry.type === 'tool' ? (
                <ToolExecutionGroup
                  toolIds={[entry.callId]}
                  transcriptTools={entry.message.transcriptTools}
                  detailsOnly
                />
              ) : (
                <SubagentStreamBlock
                  run={entry.run}
                  readOnlySource={
                    !targetScope ||
                    entry.run.workspaceId !== targetScope.workspaceId ||
                    entry.run.conversationId !== targetScope.conversationId
                  }
                />
              )}
            </div>
          ))}
        </div>
      )}
    </div>
  );
}

/** 只呈现显式消息块及原工具引用；执行终态始终来自运行时。 */
export function NativeMessageTimeline({
  message,
  messages,
  projection,
  targetScope,
  includeSubagents = true,
}: {
  message?: ChatMessage;
  messages?: readonly ChatMessage[];
  projection?: NativeTimelineProjection;
  includeSubagents?: boolean;
  targetScope?: { workspaceId: string; conversationId: string | null };
}) {
  const runs = useSubagentRunStore((state) => state.runs);
  const tools = useToolExecutionStore((state) => state.tools);
  const projected =
    projection ??
    projectNativeTimeline(
      messages ?? (message ? [message] : []),
      includeSubagents ? Object.values(runs) : [],
      tools
    );
  const items = projected.items;
  // 展开选择绑定仍在原段内的事件身份。补页改变段首时沿原anchor复用UI状态。
  const [choices, setChoices] = useState<Record<string, ProcessChoice>>({});
  const [focusedAnchor, setFocusedAnchor] = useState<string | null>(null);
  const choose = (
    item: Extract<TimelineItem, { type: 'process' }>,
    anchor: string,
    expanded: boolean,
    explicit: boolean
  ) => {
    setChoices((current) => {
      if (!explicit && item.entries.some((entry) => current[entry.key])) return current;
      const order =
        Object.values(current).reduce((max, value) => Math.max(max, value.order), 0) + 1;
      const next = { ...current };
      for (const entry of item.entries) next[entry.key] = { anchor, expanded, explicit, order };
      return next;
    });
  };
  return (
    <div className="space-y-2" aria-label="消息与工具时间线">
      {items.map((item) => {
        if (item.type === 'process') {
          const known = item.entries.flatMap((entry) =>
            choices[entry.key] ? [choices[entry.key]] : []
          );
          const choice = known.reduce<ProcessChoice | undefined>(
            (latest, value) =>
              !latest ||
              (value.explicit && !latest.explicit) ||
              (value.explicit === latest.explicit && value.order > latest.order)
                ? value
                : latest,
            undefined
          );
          const anchor =
            known.find((value) => value.anchor === focusedAnchor)?.anchor ??
            choice?.anchor ??
            item.key;
          return (
            <NativeProcessSegment
              key={anchor}
              item={item}
              choice={choice}
              targetScope={targetScope}
              onChoose={(expanded) => choose(item, anchor, expanded, true)}
              onFocus={() => {
                setFocusedAnchor(anchor);
                choose(item, anchor, choice?.expanded ?? false, false);
              }}
            />
          );
        }
        return (
          <div key={item.key} className="text-sm leading-relaxed text-[var(--text-assistant-msg)]">
            <MarkdownContent className="break-words" content={item.content} />
          </div>
        );
      })}
      {projected.unplaced.length > 0 && (
        <details className="py-1 text-xs text-[var(--text-tertiary)]">
          <summary className="cursor-pointer">
            子代理位置未恢复 · {projected.unplaced.length} 个
          </summary>
          {projected.unplaced.map((run) => (
            <SubagentStreamBlock
              key={`${run.runId}:${run.subagentRunId}`}
              run={run}
              readOnlySource={
                !targetScope ||
                run.workspaceId !== targetScope.workspaceId ||
                run.conversationId !== targetScope.conversationId
              }
            />
          ))}
        </details>
      )}
    </div>
  );
}
