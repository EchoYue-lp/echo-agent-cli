import { ListTodo } from 'lucide-react';
import { useTaskRuntimeStore } from '../../stores/taskRuntimeStore';
import { TaskRuntimePanel } from '../task/TaskRuntimePanel';

export function RightRail() {
  const activeRun = useTaskRuntimeStore((state) => state.activeRun);

  return (
    <div className="h-full min-h-0 overflow-y-auto">
      {activeRun ? (
        <TaskRuntimePanel />
      ) : (
        <div className="flex h-full flex-col items-center justify-center gap-2 px-8 text-center text-[var(--text-tertiary)]">
          <ListTodo size={28} strokeWidth={1.4} />
          <div className="text-xs font-medium text-[var(--text-secondary)]">暂无运行中的任务</div>
          <div className="max-w-[260px] text-[11px] leading-relaxed">
            复杂任务启动后，这里会显示目标、运行状态和执行进度。
          </div>
        </div>
      )}
    </div>
  );
}
