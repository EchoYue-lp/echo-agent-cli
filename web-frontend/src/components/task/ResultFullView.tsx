import MarkdownContent from '../common/MarkdownContent';

interface ResultFullViewProps {
  content: string;
  onClose: () => void;
}

export function ResultFullView({ content, onClose }: ResultFullViewProps) {
  return (
    <div className="fixed inset-0 z-50 bg-black/40 flex items-center justify-center" onClick={onClose}>
      <div
        className="w-[900px] max-w-[95vw] h-[85vh] rounded-lg flex flex-col"
        style={{ background: 'var(--bg-primary)', border: '1px solid var(--border-primary)' }}
        onClick={(e) => e.stopPropagation()}
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
      </div>
    </div>
  );
}
