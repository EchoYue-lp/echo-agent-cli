/** 仅由性能构建入口引用；复用产品事件消费与组件，不提供产品路由或执行器。 */
import { createElement, useLayoutEffect } from 'react';
import { createRoot } from 'react-dom/client';
import { MessageBubble } from '../components/chat/MessageBubble';
import { handleChatEventEnvelope } from '../hooks/chatEventHandler';
import { useChatStore } from '../stores/chatStore';
import { useConversationStore } from '../stores/conversationStore';
import { useToolExecutionStore } from '../stores/toolExecutionStore';
import { useTaskRuntimeStore } from '../stores/taskRuntimeStore';
import { useSubagentRunStore } from '../stores/subagentRunStore';
import type {
  ChatEventEnvelope,
  NativeFinalAnswerSource,
  SavedMessage,
  ToolExecution,
} from '../types/api';

export const operations = ['mount', 'single-row', 'batch', 'run-state'] as const;
export type Operation = (typeof operations)[number];
type Action =
  | { kind: 'agent'; envelope: ChatEventEnvelope }
  | { kind: 'tool'; event: 'started' | 'finished'; summary: ToolExecution }
  | { kind: 'publication'; message: SavedMessage };
type Expected = {
  content: string;
  finalAnswerConfirmed: boolean;
  executionStatus?: unknown;
  finalAnswerSource?: NativeFinalAnswerSource;
  blockIds: string[];
  tools: { id: string; status: string }[];
  visible: { text: string; count: number }[];
  absent: string[];
};
export interface TimelineFixture {
  kind: 'native-chat-timeline';
  input_class: 'synthetic-control' | 'actual-backend-capture';
  workspace_id: string;
  conversation_id: string;
  message_id: string;
  execution_id: string;
  source_sha256: string;
  stages: { operation: Operation; actions: Action[]; expected: Expected }[];
}
export interface MeasurementIdentity {
  fixture_sha256: string;
  source_sha256: string;
  dataset_sha256: string;
  protocol_sha256: string;
  environment_id: string;
  process_id: string;
}

function fact(condition: unknown, message: string): asserts condition {
  if (!condition) throw new Error(message);
}
function same(left: unknown, right: unknown): boolean {
  return JSON.stringify(left) === JSON.stringify(right);
}

/** 只核测量输入和消费范围；最终publication仍交原chat store/renderer判断。 */
export function parseTimelineFixture(text: string): TimelineFixture {
  const fixture = JSON.parse(text) as TimelineFixture;
  fact(fixture.kind === 'native-chat-timeline', '不是聊天时间线夹具');
  fact(
    ['synthetic-control', 'actual-backend-capture'].includes(fixture.input_class),
    '输入来源缺失'
  );
  for (const key of [
    'workspace_id',
    'conversation_id',
    'message_id',
    'execution_id',
    'source_sha256',
  ] as const)
    fact(typeof fixture[key] === 'string' && fixture[key].trim(), `缺少${key}`);
  fact(fixture.stages?.length === 4, '必须保留原probe四操作');
  const all: Action[] = [];
  const textByBlock = new Map<string, string>();
  fixture.stages.forEach((stage, index) => {
    fact(stage.operation === operations[index] && stage.actions?.length, '操作顺序或输入缺失');
    fact(
      stage.expected &&
        Array.isArray(stage.expected.visible) &&
        Array.isArray(stage.expected.absent),
      '缺少DOM oracle'
    );
    for (const action of stage.actions) {
      // 本离线负载只测文字/工具摘要；先拒绝会自动取子资源的Markdown媒体。
      // 这是合成基准范围，不改变产品对真实附件/图片的支持。
      fact(
        !/!\[|<(?:img|iframe|video|audio|source|object|embed|link)\b/i.test(JSON.stringify(action)),
        '离线负载不接受自动媒体子资源'
      );
      fact(['agent', 'tool', 'publication'].includes(action.kind), '未知消费路径');
      if (action.kind === 'agent') {
        const e = action.envelope;
        fact(
          e.workspace_id === fixture.workspace_id && e.conversation_id === fixture.conversation_id,
          '消息scope不同'
        );
        fact(
          e.payload.source === 'agent' && e.payload.event.execution_id === fixture.execution_id,
          '不是原Agent envelope'
        );
        fact(
          [
            'assistant_message',
            'tool_call',
            'tool_result',
            'execution_status',
            'final_answer',
          ].includes(e.payload.event.payload.type),
          '不允许后台任务或生命周期副作用'
        );
        const payload = e.payload.event.payload;
        if (payload.type === 'assistant_message') {
          const block = payload.data.block;
          const key = JSON.stringify([
            e.payload.event.execution_id,
            block.request_id,
            block.block_id,
          ]);
          const text = (textByBlock.get(key) ?? '') + block.delta;
          fact(
            !/!\[|<(?:img|iframe|video|audio|source|object|embed|link)\b/i.test(text),
            '离线负载不接受分段自动媒体'
          );
          textByBlock.set(key, text);
        }
      } else if (action.kind === 'tool') {
        const t = action.summary;
        fact(
          t.workspace_id === fixture.workspace_id &&
            t.conversation_id === fixture.conversation_id &&
            t.owner.kind === 'chat' &&
            t.owner.message_id === fixture.message_id,
          '工具owner不同'
        );
        fact(
          ['started', 'finished'].includes(action.event) && t.id && t.call_id && t.name,
          '工具摘要身份缺失'
        );
      } else {
        fact(
          action.message.role === 'assistant' &&
            action.message.final_answer_confirmed &&
            action.message.final_answer_source,
          '缺少原get publication DTO'
        );
      }
      all.push(action);
    }
  });
  const types = all.flatMap((a) =>
    a.kind === 'agent' && a.envelope.payload.source === 'agent'
      ? [a.envelope.payload.event.payload.type]
      : []
  );
  for (const type of [
    'assistant_message',
    'tool_call',
    'tool_result',
    'execution_status',
    'final_answer',
  ])
    fact(types.includes(type as (typeof types)[number]), `缺少${type}真实入口`);
  fact(
    all.some((a) => a.kind === 'tool') && all.some((a) => a.kind === 'publication'),
    '缺少工具/最终publication消费'
  );
  return fixture;
}

/** 唯一原生ABI：prepare/execute/snapshot/cleanup/finish；不生成统计或样本文件。 */
export async function createTimelineMeasurement(
  fixture: TimelineFixture,
  identity: MeasurementIdentity,
  options: { functionalTest?: boolean; watchdogMs?: number } = {}
) {
  fact(
    options.functionalTest || fixture.input_class === 'actual-backend-capture',
    '合成控制不能进入原生测量'
  );
  fact(identity.source_sha256 === fixture.source_sha256, '夹具source不同');
  for (const value of Object.values(identity))
    fact(typeof value === 'string' && value.trim(), '测量身份缺失');
  const priorChat = useChatStore.getState();
  const priorTools = useToolExecutionStore.getState();
  fact(
    priorChat.messages.length === 0 &&
      !priorChat.isStreaming &&
      priorChat.pendingHitlRequests.length === 0,
    '不能接管已有聊天'
  );
  fact(
    useConversationStore.getState().activeId === null && Object.keys(priorTools.tools).length === 0,
    '不能接管已有会话或工具'
  );
  fact(
    useTaskRuntimeStore.getState().activeRun === null &&
      Object.keys(useSubagentRunStore.getState().runs).length === 0,
    '不能接管运行owner'
  );
  const previousClass = document.documentElement.getAttribute('class');
  document.documentElement.classList.add('dark');
  const outlet = document.createElement('section');
  outlet.setAttribute('aria-label', '原生聊天时间线性能观察');
  outlet.style.cssText =
    'position:fixed;inset:0;overflow:auto;background:var(--bg-primary);z-index:1000';
  document.body.append(outlet);
  const root = createRoot(outlet);
  let commits = 0;
  let committedAt = 0;
  const commitListeners = new Set<() => void>();
  let active: {
    operation: Operation;
    phase: string;
    index: number;
    executed: boolean;
    commitsBefore: number;
    dispatch?: { started_ms: number; finished_ms: number };
    domReady?: number;
  } | null = null;
  let closed = false;
  let failed: Error | null = null;
  const documentEpoch = `${identity.process_id}:${performance.timeOrigin}`;
  const ctx = {
    assistantIdRef: { current: fixture.message_id as string | null },
    currentMessageKeyRef: { current: null as string | null },
    currentMessageIdRef: { current: fixture.message_id as string | null },
    isCancelledRef: { current: false },
    currentThinkingIdRef: { current: null as string | null },
  };
  function View() {
    const messages = useChatStore((s) => s.messages);
    const tools = useToolExecutionStore((s) => s.tools);
    useLayoutEffect(() => {
      commits += 1;
      committedAt = performance.now();
      // 空树也有真实React提交；不能只靠DOM mutation等待首次空挂载。
      queueMicrotask(() => commitListeners.forEach((listener) => listener()));
    }, [messages, tools]);
    return (
      <>{messages.map((message) => createElement(MessageBubble, { key: message.id, message }))}</>
    );
  }
  const message = () => useChatStore.getState().messages.find((m) => m.id === fixture.message_id);
  function consume(action: Action) {
    if (action.kind === 'agent') handleChatEventEnvelope(action.envelope, ctx);
    else if (action.kind === 'tool') {
      // 与原useTauriChat的summary listener完全相同；raw ToolResult不伪造summary。
      useToolExecutionStore.getState().ingest(action.summary);
      if (action.event === 'started' && action.summary.owner.kind === 'chat')
        useChatStore.getState().recordToolStart(action.summary.owner.message_id, action.summary.id);
    } else {
      // 原refreshNativeFinalPresentation的既有只读DTO→store接缝，不改final标记/正文。
      const source = action.message.final_answer_source;
      fact(source, '原publication来源缺失');
      useChatStore.getState().applyFinalAnswerSource(fixture.message_id, source);
    }
  }
  function correct(expected: Expected, checkDom: boolean) {
    const m = message();
    fact(
      m &&
        m.content === expected.content &&
        Boolean(m.finalAnswerConfirmed) === expected.finalAnswerConfirmed,
      '消息正文/发布资格不同'
    );
    fact(
      same(m.executionStatus, expected.executionStatus) &&
        same(m.finalAnswerSource, expected.finalAnswerSource),
      '原执行终态/publication未匹配'
    );
    fact(
      same(m.assistantBlocks?.map((v) => v.block.block_id) ?? [], expected.blockIds),
      'AssistantMessage身份不同'
    );
    fact(
      Object.keys(useToolExecutionStore.getState().tools).length === expected.tools.length,
      '工具集合不同'
    );
    for (const tool of expected.tools)
      fact(
        useToolExecutionStore.getState().tools[tool.id]?.status === tool.status,
        '工具真实摘要状态不同'
      );
    if (!checkDom) return;
    const text = outlet.textContent ?? '';
    for (const item of expected.visible)
      fact(
        item.text && text.split(item.text).length - 1 === item.count,
        `DOM文本计数不同:${item.text}`
      );
    for (const value of expected.absent)
      fact(value && !text.includes(value), '未公开/私有正文泄漏');
    if (!options.functionalTest) {
      fact(
        innerWidth === 1400 &&
          innerHeight === 900 &&
          document.documentElement.classList.contains('dark'),
        '原生viewport/theme发生漂移'
      );
      fact(
        location.protocol === 'tauri:' &&
          location.hostname === 'localhost' &&
          document.visibilityState === 'visible' &&
          document.hasFocus(),
        '原生来源/可见性/焦点发生漂移'
      );
      fact(
        outlet.getBoundingClientRect().width > 0 && outlet.getBoundingClientRect().height > 0,
        '真实布局不可用'
      );
    }
  }
  function wait(predicate: () => boolean) {
    return new Promise<void>((resolve, reject) => {
      const observer = new MutationObserver(check);
      const timer = setTimeout(
        () => done(new Error('DOM/提交watchdog，拒绝成功样本')),
        options.functionalTest ? (options.watchdogMs ?? 1000) : 8000
      );
      function done(error?: Error) {
        observer.disconnect();
        commitListeners.delete(check);
        clearTimeout(timer);
        if (error) reject(error);
        else resolve();
      }
      function check() {
        try {
          if (closed) {
            done(new Error('观察已取消/关闭'));
            return;
          }
          if (predicate()) done();
        } catch (error) {
          done(error instanceof Error ? error : new Error(String(error)));
        }
      }
      observer.observe(outlet, {
        subtree: true,
        childList: true,
        characterData: true,
        attributes: true,
      });
      commitListeners.add(check);
      check();
    });
  }
  const dispose = () => {
    if (closed) return;
    closed = true;
    commitListeners.forEach((listener) => listener());
    root.unmount();
    outlet.remove();
    useChatStore.setState(priorChat);
    useToolExecutionStore.setState(priorTools);
    if (previousClass === null) document.documentElement.removeAttribute('class');
    else document.documentElement.setAttribute('class', previousClass);
  };
  const fail = (error: unknown) => {
    failed = error instanceof Error ? error : new Error(String(error));
    dispose();
    return failed;
  };
  root.render(<View />);
  try {
    await wait(() => commits > 0);
  } catch (error) {
    dispose();
    throw error;
  }
  const api = {
    async prepare(operation: Operation, phase: string, index: number) {
      fact(!closed && !failed && active === null, '旧case仍活跃或失败');
      fact(
        operations.includes(operation) &&
          ['warmup', 'measurement'].includes(phase) &&
          Number.isSafeInteger(index) &&
          index >= 0 &&
          index < (phase === 'warmup' ? 5 : 30),
        '操作/样本身份非法'
      );
      try {
        useChatStore.getState().clearMessages();
        useToolExecutionStore.getState().clear();
        useChatStore.getState().startAssistantMessage(fixture.message_id);
        for (const stage of fixture.stages) {
          if (stage.operation === operation) break;
          for (const action of stage.actions) consume(action);
        }
        // 前缀消费与DOM清空都在timer外；原生probe在prepare返回之后才起Mach时钟。
        const before = commits;
        await wait(() => commits > before);
        active = {
          operation,
          phase,
          index,
          executed: false,
          commitsBefore: commits,
        };
        return { prepared: true, document_epoch: documentEpoch };
      } catch (error) {
        throw fail(error);
      }
    },
    async execute() {
      fact(active && !active.executed && !closed && !failed, '未prepare或重复execute');
      const current = active;
      const stage = fixture.stages.find((s) => s.operation === current.operation);
      fact(stage, '缺少真实操作');
      try {
        const started = performance.now();
        for (const action of stage.actions) consume(action);
        current.dispatch = {
          started_ms: started,
          finished_ms: performance.now(),
        };
        correct(stage.expected, false);
        await wait(() => {
          if (commits <= current.commitsBefore) return false;
          correct(stage.expected, true);
          return true;
        });
        current.domReady = performance.now();
        current.executed = true;
        return api.snapshot();
      } catch (error) {
        throw fail(error);
      }
    },
    snapshot() {
      fact(active?.executed && !closed && !failed, '未完成DOM/提交，不产生成功见证');
      const stage = fixture.stages.find((s) => s.operation === active?.operation);
      fact(stage, '操作缺失');
      correct(stage.expected, true);
      const m = message();
      fact(m, '消息缺失');
      const boundaries = {
        dispatch: active.dispatch,
        react_commits: commits - active.commitsBefore,
        react_layout_effect_ms: committedAt,
        dom_ready_ms: active.domReady,
        clock: 'performance.now, same document only',
        transaction_ack: 'native probe only',
        os_paint: 'not_measured',
      };
      return {
        ready: true,
        scenario: `gui/NativeMessageTimeline/${active.operation}`,
        phase: active.phase,
        sample_index: active.index,
        document_epoch: documentEpoch,
        fixture_sha256: identity.fixture_sha256,
        source_sha256: identity.source_sha256,
        dataset_sha256: identity.dataset_sha256,
        protocol_sha256: identity.protocol_sha256,
        environment_id: identity.environment_id,
        process_observation_id: identity.process_id,
        correctness: {
          ok: true,
          workload: fixture.kind,
          input_class: fixture.input_class,
          operation: active.operation,
          boundaries,
          projection: {
            content: m.content,
            finalAnswerConfirmed: Boolean(m.finalAnswerConfirmed),
            executionStatus: m.executionStatus,
            finalAnswerSource: m.finalAnswerSource,
            blockIds: m.assistantBlocks?.map((v) => v.block.block_id) ?? [],
            tools: Object.values(useToolExecutionStore.getState().tools)
              .map((t) => ({ id: t.id, status: t.status }))
              .sort((a, b) => a.id.localeCompare(b.id)),
          },
        },
        dom_elements: outlet.querySelectorAll('*').length,
        visibility: document.visibilityState,
        has_focus: document.hasFocus(),
        boundaries,
      };
    },
    async cleanup() {
      fact(active?.executed && !failed, '未完成case不能冒成功cleanup');
      try {
        useChatStore.getState().clearMessages();
        useToolExecutionStore.getState().clear();
        const before = commits;
        await wait(() => commits > before && outlet.textContent === '');
        active = null;
        return { closed: true, restored: true };
      } catch (error) {
        throw fail(error);
      }
    },
    finish() {
      fact(active === null && !failed, '仍有case/失败不能结束为成功');
      dispose();
    },
    abort() {
      failed = new Error('观察已取消，不可生成成功见证');
      dispose();
    },
  };
  return api;
}
