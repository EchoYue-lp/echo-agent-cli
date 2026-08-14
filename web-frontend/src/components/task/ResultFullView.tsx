import MarkdownContent from '../common/MarkdownContent';
import { Modal } from '../common/Modal';

interface ResultFullViewProps {
  content: string;
  onClose: () => void;
}

export function ResultFullView({ content, onClose }: ResultFullViewProps) {
  return (
    <Modal
      onClose={onClose}
      ariaLabel="最终任务结果"
      className="flex h-[85vh] w-[900px] max-w-[95vw] flex-col rounded-lg border border-[var(--border-primary)] bg-[var(--bg-primary)]"
    >
      <header
        className="flex justify-between items-center px-4 py-3 border-b"
        style={{ borderColor: 'var(--border-primary)' }}
      >
        <h2 className="font-medium" style={{ color: 'var(--text-primary)' }}>
          最终任务结果
        </h2>
        <button onClick={onClose} className="text-sm" style={{ color: 'var(--text-tertiary)' }}>
          ✕
        </button>
      </header>
      <div className="flex-1 overflow-y-auto p-4">
        <MarkdownContent content={content} />
      </div>
    </Modal>
  );
}
