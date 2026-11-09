import { useRef, useState } from 'react';
import {
  ArrowRight,
  Check,
  ChevronDown,
  CircleHelp,
  Clock3,
  ListChecks,
  MessageSquare,
  PencilLine,
  ShieldCheck,
  X,
} from 'lucide-react';
import { Modal } from '../common/Modal';
import type { PendingHitlRequest } from '../../stores/chatStore';
import './HitlDialog.css';

type Reply = void | boolean;

interface HitlDialogProps {
  requests: readonly PendingHitlRequest[];
  conversationLabel?: string;
  onApproval: (
    request: PendingHitlRequest,
    approved: boolean,
    reason?: string,
    scope?: string
  ) => Promise<Reply>;
  onInput: (request: PendingHitlRequest, text: string) => Promise<Reply>;
  onSelection: (
    request: PendingHitlRequest,
    selection: string,
    instructions?: string
  ) => Promise<Reply>;
}

const requestLabels = {
  approval: '操作确认',
  input: '补充信息',
  selection: '选择下一步',
};

/** Presentation only: the original queue and reply callbacks retain request ownership. */
export function HitlDialog(props: HitlDialogProps) {
  const request = props.requests.at(0);
  if (!request) return null;

  return <HitlRequestDialog key={JSON.stringify(request.identity)} {...props} request={request} />;
}

function HitlRequestDialog({
  request,
  requests,
  conversationLabel,
  onApproval,
  onInput,
  onSelection,
}: HitlDialogProps & { request: PendingHitlRequest }) {
  const [open, setOpen] = useState(true);
  const [answer, setAnswer] = useState('');
  const [selection, setSelection] = useState('');
  const [feedbackMode, setFeedbackMode] = useState<'reject' | 'modify' | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [submitted, setSubmitted] = useState(false);
  const [busy, setBusy] = useState(false);
  // A ref closes the same-frame double-click gap without becoming a reply authority.
  const submitting = useRef(false);
  const answerRef = useRef<HTMLTextAreaElement>(null);
  const closeRef = useRef<HTMLButtonElement>(null);
  const isComposing = useRef(false);
  const label = requestLabels[request.kind];
  const Icon =
    request.kind === 'approval'
      ? ShieldCheck
      : request.kind === 'input'
        ? MessageSquare
        : ListChecks;

  const submit = async (send: () => Promise<Reply>) => {
    if (submitting.current || submitted) return;
    submitting.current = true;
    setBusy(true);
    setError(null);
    try {
      const accepted = await send();
      if (accepted === false) throw new Error('回复未能提交，请重试。');
      // Submission is an acknowledgement, never a claim that this response won a race.
      setSubmitted(true);
    } catch (cause) {
      setError(cause instanceof Error ? cause.message : '回复未能提交，请重试。');
    } finally {
      submitting.current = false;
      setBusy(false);
    }
  };

  const submitForm = () => {
    if (request.kind === 'input') {
      if (answer.trim()) void submit(() => onInput(request, answer));
    } else if (request.kind === 'selection') {
      const choice = request.options.length === 0 ? 'approve' : selection;
      if (choice) void submit(() => onSelection(request, choice, answer.trim() || undefined));
    } else if (feedbackMode === 'reject') {
      void submit(() => onApproval(request, false, answer.trim() || undefined));
    } else if (feedbackMode === 'modify' && answer.trim()) {
      void submit(() => onApproval(request, false, `修改意见: ${answer.trim()}`));
    }
  };

  return (
    <>
      <button type="button" className="hitl-pending" onClick={() => setOpen(true)}>
        <span className="hitl-pending-icon">
          <Icon size={17} />
        </span>
        <span className="hitl-pending-copy">
          <strong>{submitted ? '回复已提交，等待确认' : 'Eclip 正在等你回复'}</strong>
          <span>{request.prompt || label}</span>
        </span>
        <span className="hitl-count">{requests.length} 项待处理</span>
        <ArrowRight size={16} aria-hidden="true" />
      </button>

      {open && (
        <Modal
          ariaLabel={label}
          onClose={() => setOpen(false)}
          className="hitl-dialog"
          overlayClassName="hitl-overlay"
          initialFocusRef={request.kind === 'input' ? answerRef : closeRef}
        >
          <div className="hitl-dialog-heading">
            <div className="hitl-heading-icon">
              <Icon size={23} aria-hidden="true" />
            </div>
            <div className="hitl-heading-copy">
              <p className="hitl-eyebrow">Eclip · 需要你的参与</p>
              <h2>{label}</h2>
            </div>
            <button
              ref={closeRef}
              type="button"
              className="hitl-icon-button"
              aria-label="稍后处理"
              onClick={() => setOpen(false)}
            >
              <X size={19} />
            </button>
          </div>

          <form
            className="hitl-form"
            onSubmit={(event) => {
              event.preventDefault();
              submitForm();
            }}
          >
            <div className="hitl-dialog-body">
              <div className="hitl-source">
                <span>
                  <MessageSquare size={12} aria-hidden="true" />
                  {conversationLabel || '当前会话'}
                </span>
                {request.kind === 'approval' && <code>{request.toolName}</code>}
                {request.kind === 'selection' && request.phase && <span>{request.phase}</span>}
              </div>
              <p className="hitl-question">
                {request.prompt ||
                  (request.kind === 'approval'
                    ? '请确认是否允许 Eclip 执行以下操作。'
                    : '继续之前，还需要你补充一些信息。')}
              </p>

              {request.kind === 'approval' && (
                <div className="hitl-operation">
                  <div className="hitl-section-label">
                    <span>操作内容</span>
                    <code>{request.toolName}</code>
                  </div>
                  <pre>{JSON.stringify(request.args, null, 2)}</pre>
                </div>
              )}

              {request.kind === 'selection' && request.options.length > 0 && (
                <fieldset className="hitl-options" disabled={busy || submitted}>
                  <legend className="hitl-field-label">选择一个选项</legend>
                  {request.options.map((option, index) => (
                    <label className="hitl-option" key={`${index}:${option}`}>
                      <input
                        type="radio"
                        name="hitl-selection"
                        value={option}
                        checked={selection === option}
                        onChange={() => setSelection(option)}
                      />
                      <span className="hitl-option-index">
                        {String(index + 1).padStart(2, '0')}
                      </span>
                      <span className="hitl-option-copy">{option}</span>
                      {selection === option && <Check size={16} aria-hidden="true" />}
                    </label>
                  ))}
                </fieldset>
              )}

              {(request.kind !== 'approval' || feedbackMode) && (
                <div className="hitl-answer">
                  <label htmlFor="hitl-answer" className="hitl-field-label">
                    {request.kind === 'input'
                      ? '你的回答'
                      : request.kind === 'selection'
                        ? '补充说明（可选）'
                        : feedbackMode === 'reject'
                          ? '拒绝原因（可选）'
                          : '希望如何调整？'}
                  </label>
                  <textarea
                    id="hitl-answer"
                    ref={answerRef}
                    value={answer}
                    onChange={(event) => setAnswer(event.target.value)}
                    onKeyDown={(event) => {
                      if (
                        event.key === 'Enter' &&
                        (event.metaKey || event.ctrlKey) &&
                        !event.nativeEvent.isComposing &&
                        !isComposing.current
                      ) {
                        event.preventDefault();
                        submitForm();
                      }
                    }}
                    onCompositionStart={() => {
                      isComposing.current = true;
                    }}
                    onCompositionEnd={() => {
                      isComposing.current = false;
                    }}
                    placeholder={
                      request.kind === 'selection'
                        ? '给 Eclip 一点额外说明…'
                        : '在这里输入，中文和多行内容均可…'
                    }
                    rows={request.kind === 'input' ? 4 : 3}
                    disabled={busy || submitted}
                  />
                  <span className="hitl-input-hint">换行使用 Enter，提交使用 ⌘ / Ctrl + Enter</span>
                </div>
              )}

              <details className="hitl-details">
                <summary>
                  请求详情 <ChevronDown size={13} aria-hidden="true" />
                </summary>
                <dl>
                  <dt>请求</dt>
                  <dd>{request.requestId}</dd>
                  {request.kind === 'selection' && request.taskId && (
                    <>
                      <dt>任务</dt>
                      <dd>{request.taskId}</dd>
                    </>
                  )}
                </dl>
                {request.kind === 'selection' && request.context !== undefined && (
                  <pre>{JSON.stringify(request.context, null, 2)}</pre>
                )}
              </details>

              {requests.length > 1 && (
                <details className="hitl-queue">
                  <summary>
                    <Clock3 size={14} aria-hidden="true" />
                    <span>另有 {requests.length - 1} 项等待处理</span>
                    <ChevronDown size={13} aria-hidden="true" />
                  </summary>
                  <ol>
                    {requests.slice(1, 6).map((pending) => (
                      <li key={pending.requestId}>
                        <span>{requestLabels[pending.kind]}</span>
                        <p>
                          {pending.prompt ||
                            (pending.kind === 'approval' ? pending.toolName : '等待补充信息')}
                        </p>
                      </li>
                    ))}
                  </ol>
                  {requests.length > 6 && (
                    <p className="hitl-input-hint">
                      其余 {requests.length - 6} 项将在前面的请求处理后显示。
                    </p>
                  )}
                </details>
              )}

              {error && (
                <div role="alert" className="hitl-error">
                  <CircleHelp size={16} />
                  <span>
                    {error} 当前回答已保留，可重新提交；若已在其它渠道回复，请等待请求状态更新。
                  </span>
                </div>
              )}
              {submitted && (
                <p role="status" className="hitl-submitted">
                  <Check size={16} />
                  回复已提交，等待请求方确认。请勿重复操作。
                </p>
              )}
            </div>

            <div className="hitl-dialog-footer">
              <p className="hitl-footer-note">
                <ShieldCheck size={13} aria-hidden="true" />
                关闭弹窗不会自动同意
              </p>
              <div className="hitl-actions">
                {request.kind === 'approval' && !feedbackMode ? (
                  <>
                    <button
                      type="button"
                      className="hitl-button hitl-button-quiet"
                      disabled={busy || submitted}
                      onClick={() => {
                        setFeedbackMode('reject');
                        setTimeout(() => answerRef.current?.focus(), 0);
                      }}
                    >
                      拒绝
                    </button>
                    <button
                      type="button"
                      className="hitl-button hitl-button-secondary"
                      disabled={busy || submitted}
                      onClick={() => {
                        setFeedbackMode('modify');
                        setTimeout(() => answerRef.current?.focus(), 0);
                      }}
                    >
                      <PencilLine size={14} />
                      提出修改
                    </button>
                    <button
                      type="button"
                      className="hitl-button hitl-button-primary"
                      disabled={busy || submitted}
                      onClick={() => void submit(() => onApproval(request, true))}
                    >
                      <Check size={15} />
                      {busy ? '提交中…' : '允许这次'}
                    </button>
                    <button
                      type="button"
                      className="hitl-session-allow"
                      disabled={busy || submitted}
                      onClick={() =>
                        void submit(() => onApproval(request, true, undefined, 'session_tool'))
                      }
                    >
                      本会话允许此工具
                    </button>
                  </>
                ) : (
                  <>
                    <button
                      type="button"
                      className="hitl-button hitl-button-secondary"
                      disabled={busy || submitted}
                      onClick={() => (feedbackMode ? setFeedbackMode(null) : setOpen(false))}
                    >
                      {feedbackMode ? '返回' : '稍后处理'}
                    </button>
                    <button
                      type="submit"
                      className="hitl-button hitl-button-primary"
                      disabled={
                        busy ||
                        submitted ||
                        (request.kind === 'input' && !answer.trim()) ||
                        (feedbackMode === 'modify' && !answer.trim()) ||
                        (request.kind === 'selection' && request.options.length > 0 && !selection)
                      }
                    >
                      {busy
                        ? '提交中…'
                        : feedbackMode === 'reject'
                          ? '确认拒绝'
                          : feedbackMode === 'modify'
                            ? '提交修改意见'
                            : '提交回答'}
                      <ArrowRight size={15} />
                    </button>
                  </>
                )}
              </div>
            </div>
          </form>
        </Modal>
      )}
    </>
  );
}
