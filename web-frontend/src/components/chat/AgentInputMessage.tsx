import { useState } from 'react';
import type { ChatMessage } from '../../types/api';

export function AgentInputMessage({ message }: { message: ChatMessage }) {
  const [expanded, setExpanded] = useState(false);
  const record = message.agentInput;
  if (!record) return null;
  const source = record.payload.from;
  // 当前spawn/resume/handoff允许未提供地址；typed Agent来源不因此变成人类或凭空补地址。
  const sourceLabel = source ? `${source.workspace_id}/${source.conversation_id}` : '来源未提供';
  return (
    <div className="ml-auto w-fit max-w-[85%] py-3" data-agent-input={record.message_id}>
      <div className="mb-1 text-right text-xs text-[var(--text-tertiary)]">
        来自 Agent · {sourceLabel}
      </div>
      <div className="rounded-xl bg-[var(--bg-user-bubble)] px-4 py-3 text-sm">
        <div className={`whitespace-pre-wrap break-words ${expanded ? '' : 'line-clamp-3'}`}>
          {message.content}
        </div>
        {message.attachments?.map((attachment, index) => (
          <div key={index} className="mt-2">
            {attachment.mime_type.startsWith('image/') && attachment.url ? (
              <img
                src={attachment.url}
                alt={attachment.name}
                className="max-h-64 max-w-full rounded-lg"
              />
            ) : attachment.url ? (
              <a href={attachment.url} download={attachment.name} className="underline">
                {attachment.name}
              </a>
            ) : (
              <span>{attachment.name} · 历史资源引用</span>
            )}
          </div>
        ))}
        <button
          type="button"
          className="mt-2 text-xs text-[var(--text-secondary)]"
          aria-expanded={expanded}
          onClick={() => setExpanded((value) => !value)}
        >
          {expanded ? '收起全文' : '展开全文'}
        </button>
        <div className="mt-1 text-[10px] text-[var(--text-tertiary)]">
          投递阶段：{record.phase}
          {record.outcome ? ` · ${record.outcome}` : ''}
          {record.reason ? ` · ${record.reason}` : ''}
        </div>
      </div>
    </div>
  );
}
