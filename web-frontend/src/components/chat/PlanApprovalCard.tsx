import { Play, X, Edit3, AlertTriangle, Lightbulb } from 'lucide-react';
import { useTaskRuntimeStore } from '../../stores/taskRuntimeStore';

interface PlanApprovalCardProps {
  /** Dismiss the card without taking action (user keeps the plan for later). */
  onDismiss: () => void;
}

/**
 * Shown below the message stream when the runtime is awaiting plan approval.
 *
 * Content: plan goal / assumptions / risks / task count, plus three actions:
 * execute all / edit plan / cancel.
 *
 * Reuses the same store actions as the right-rail plan approval block.
 * Spec §3.4: 计划确认卡。
 */
export function PlanApprovalCard({ onDismiss }: PlanApprovalCardProps) {
  const activeRun = useTaskRuntimeStore((s) => s.activeRun);
  const plan = useTaskRuntimeStore((s) => s.plan);
  const approve = useTaskRuntimeStore((s) => s.approve);
  const reject = useTaskRuntimeStore((s) => s.reject);
  const execute = useTaskRuntimeStore((s) => s.execute);

  if (!plan || !activeRun) return null;

  const taskCount = plan.tasks.length ?? 0;

  const handleApprove = () => {
    approve(activeRun.run_id);
  };

  const handleExecute = () => {
    execute(activeRun.run_id);
  };

  const handleCancel = () => {
    reject(activeRun.run_id);
    onDismiss();
  };

  return (
    <div className="my-3 rounded-lg border border-[var(--border-primary)] bg-[var(--bg-secondary)] p-4">
      {/* Header */}
      <div className="mb-3 flex items-center gap-2">
        <Edit3 size={14} style={{ color: 'var(--accent)' }} />
        <span className="text-sm font-semibold" style={{ color: 'var(--text-primary)' }}>
          计划确认
        </span>
        <span className="ml-auto text-[10px]" style={{ color: 'var(--text-tertiary)' }}>
          {taskCount} 个任务
        </span>
      </div>

      {/* Goal */}
      <div className="mb-3 rounded-md px-2 py-1.5 text-xs" style={{ background: 'var(--bg-primary)', color: 'var(--text-secondary)' }}>
        {plan.goal}
      </div>

      {/* Assumptions */}
      {plan.assumptions.length > 0 && (
        <div className="mb-2">
          <div className="mb-1 flex items-center gap-1 text-[10px] font-medium" style={{ color: 'var(--text-tertiary)' }}>
            <Lightbulb size={10} />
            假设
          </div>
          <ul className="space-y-0.5 text-[11px]" style={{ color: 'var(--text-secondary)' }}>
            {plan.assumptions.slice(0, 5).map((a, i) => (
              <li key={i} className="truncate">· {a}</li>
            ))}
            {plan.assumptions.length > 5 && (
              <li className="text-[10px]" style={{ color: 'var(--text-tertiary)' }}>
                另有 {plan.assumptions.length - 5} 条...
              </li>
            )}
          </ul>
        </div>
      )}

      {/* Risks */}
      {plan.risks.length > 0 && (
        <div className="mb-3">
          <div className="mb-1 flex items-center gap-1 text-[10px] font-medium" style={{ color: 'var(--text-tertiary)' }}>
            <AlertTriangle size={10} />
            风险
          </div>
          <ul className="space-y-0.5 text-[11px]" style={{ color: 'var(--text-secondary)' }}>
            {plan.risks.slice(0, 5).map((r, i) => (
              <li key={i} className="truncate">· {r}</li>
            ))}
            {plan.risks.length > 5 && (
              <li className="text-[10px]" style={{ color: 'var(--text-tertiary)' }}>
                另有 {plan.risks.length - 5} 条...
              </li>
            )}
          </ul>
        </div>
      )}

      {/* Actions */}
      <div className="flex gap-2">
        <button
          onClick={handleExecute}
          className="flex flex-1 items-center justify-center gap-1.5 rounded-md px-3 py-1.5 text-xs font-medium transition-colors"
          style={{ background: 'var(--accent)', color: 'var(--text-on-accent)' }}
        >
          <Play size={12} />
          执行全部
        </button>
        <button
          onClick={handleApprove}
          className="flex items-center justify-center gap-1.5 rounded-md px-3 py-1.5 text-xs transition-colors"
          style={{ background: 'var(--bg-hover)', color: 'var(--text-secondary)' }}
        >
          <Edit3 size={12} />
          编辑计划
        </button>
        <button
          onClick={handleCancel}
          className="flex items-center justify-center gap-1.5 rounded-md px-3 py-1.5 text-xs transition-colors"
          style={{ background: 'var(--bg-hover)', color: 'var(--text-secondary)' }}
        >
          <X size={12} />
          取消
        </button>
      </div>
    </div>
  );
}
