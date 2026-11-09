import { useEffect, useState } from 'react';
import { MessageSquare } from 'lucide-react';
import { toolExecutionApi } from '../../api/endpoints';
import { sentAgentReceipt, receiptConversation } from '../../lib/agentCommunication';
import { openAgentConversation } from '../../lib/openAgentConversation';
import { useToastStore } from '../../stores/toastStore';
import type {
  ToolExecution,
  ToolExecutionDetailManifest,
  TranscriptToolRecord,
} from '../../types/api';
import { InlineToolCall } from './InlineToolCall';

/** A compact observation of one exact persisted tool result; never parse output strings. */
export function AgentSentMessage({
  tool,
  transcript,
}: {
  tool: ToolExecution;
  transcript?: TranscriptToolRecord;
}) {
  const binding = JSON.stringify([
    tool.workspace_id,
    tool.conversation_id,
    tool.id,
    tool.call_id,
    tool.detail_ref,
    tool.status,
  ]);
  const [detail, setDetail] = useState<{
    binding: string;
    manifest: ToolExecutionDetailManifest;
  } | null>(null);
  useEffect(() => {
    let current = true;
    if (tool.status === 'succeeded' && tool.detail_ref) {
      void toolExecutionApi
        .detail(tool.workspace_id, tool.detail_ref)
        .then((manifest) => {
          if (current && manifest.id === tool.id && manifest.invocation.name === tool.name)
            setDetail({ binding, manifest });
        })
        .catch(() => {
          /* The original tool row retains failure/detail access. */
        });
    }
    return () => {
      current = false;
    };
  }, [binding, tool.workspace_id, tool.detail_ref, tool.id, tool.name, tool.status]);
  const receipt = sentAgentReceipt(detail?.binding === binding ? detail.manifest : null);
  const target = receipt ? receiptConversation(receipt) : null;
  if (!receipt || !target) return <InlineToolCall toolId={tool.id} transcript={transcript} />;
  const label = ['failed', 'rejected', 'cancelled'].includes(receipt.outcome ?? '')
    ? '消息投递失败'
    : receipt.outcome === 'unknown'
      ? '消息投递记录'
      : '已发送消息到会话';
  return (
    <div
      className="my-1 min-w-0 text-xs text-[var(--text-tertiary)]"
      data-agent-sent={receipt.message_id ?? undefined}
    >
      <div className="flex min-h-7 min-w-0 items-center gap-2">
        <MessageSquare size={13} />
        <button
          type="button"
          className="truncate text-left hover:underline"
          onClick={() => {
            void openAgentConversation(target).catch((error: unknown) =>
              useToastStore
                .getState()
                .addToast('error', error instanceof Error ? error.message : '打开目标会话失败')
            );
          }}
        >
          {label} · {target.conversation_id}
        </button>
        <span className="shrink-0">
          {receipt.phase}
          {receipt.outcome ? ` · ${receipt.outcome}` : ''}
        </span>
      </div>
      <details className="ml-5">
        <summary className="cursor-pointer py-1">消息工具详情</summary>
        {receipt.detail && <p className="whitespace-pre-wrap break-words py-1">{receipt.detail}</p>}
        <InlineToolCall toolId={tool.id} transcript={transcript} />
      </details>
    </div>
  );
}
