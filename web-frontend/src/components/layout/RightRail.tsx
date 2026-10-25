import { AlertCircle, ListTodo } from 'lucide-react';
import { useTaskRuntimeStore } from '../../stores/taskRuntimeStore';
import { TaskRuntimePanel } from '../task/TaskRuntimePanel';

export function RightRail() {
  const activeRun = useTaskRuntimeStore((state) => state.activeRun);
  const error = useTaskRuntimeStore((state) => state.error);

  return (
    <div className="h-full min-h-0 overflow-y-auto">
      {activeRun ? (
        <TaskRuntimePanel key={activeRun.run_id} />
      ) : (
        <div className="flex h-full flex-col items-center justify-center gap-2 px-8 text-center text-[var(--text-tertiary)]">
          {error ? (
            <AlertCircle size={28} strokeWidth={1.4} />
          ) : (
            <ListTodo size={28} strokeWidth={1.4} />
          )}
          <div className="text-sm font-medium text-[var(--text-secondary)]">
            {error ? '暂时无法读取任务' : '给目标一个清晰的计划'}
          </div>
          <div className="max-w-[260px] text-xs leading-relaxed">
            {error ??
              '当前会话还没有任务运行。复杂任务启动后，可以在这里查看每一步、执行回执与产物。'}
          </div>
        </div>
      )}
    </div>
  );
}
