import { useState, useEffect, useRef, useCallback } from 'react';
import { FileEdit, Eye, Save, Clock } from 'lucide-react';
import { scratchpadApi, type ScratchpadContent } from '../../api/endpoints';

export function ScratchpadPanel() {
  const [content, setContent] = useState('');
  const [modifiedAt, setModifiedAt] = useState('');
  const [preview, setPreview] = useState(false);
  const [saving, setSaving] = useState(false);
  const [dirty, setDirty] = useState(false);
  const timerRef = useRef<ReturnType<typeof setTimeout> | null>(null);

  // Load on mount
  useEffect(() => {
    scratchpadApi
      .get()
      .then((data: ScratchpadContent) => {
        setContent(data.content);
        setModifiedAt(data.modified_at);
      })
      .catch(() => {
        // ignore
      });
  }, []);

  // Debounced auto-save
  const save = useCallback(async (text: string) => {
    setSaving(true);
    try {
      const res = await scratchpadApi.update(text);
      setModifiedAt(res.modified_at);
      setDirty(false);
    } catch {
      // ignore
    } finally {
      setSaving(false);
    }
  }, []);

  const handleChange = (e: React.ChangeEvent<HTMLTextAreaElement>) => {
    const text = e.target.value;
    setContent(text);
    setDirty(true);

    if (timerRef.current) clearTimeout(timerRef.current);
    timerRef.current = setTimeout(() => {
      save(text);
    }, 500);
  };

  // Cleanup timer
  useEffect(() => {
    return () => {
      if (timerRef.current) clearTimeout(timerRef.current);
    };
  }, []);

  const formatTime = (iso: string) => {
    if (!iso) return '';
    try {
      return new Date(iso).toLocaleString();
    } catch {
      return iso;
    }
  };

  return (
    <div className="flex flex-col h-full">
      {/* Toolbar */}
      <div
        className="flex items-center justify-between px-4 py-2 border-b shrink-0"
        style={{ borderColor: 'var(--border-primary)' }}
      >
        <div className="flex items-center gap-2">
          <FileEdit size={16} style={{ color: 'var(--text-secondary)' }} />
          <span className="text-sm font-medium" style={{ color: 'var(--text-primary)' }}>
            Scratchpad
          </span>
          {dirty && (
            <span className="text-xs px-1.5 py-0.5 rounded-md bg-yellow-500/20 text-yellow-500">
              unsaved
            </span>
          )}
          {saving && (
            <span className="text-xs" style={{ color: 'var(--text-tertiary)' }}>
              saving...
            </span>
          )}
        </div>
        <div className="flex items-center gap-1">
          {modifiedAt && (
            <span
              className="text-xs flex items-center gap-1 mr-2"
              style={{ color: 'var(--text-tertiary)' }}
            >
              <Clock size={12} />
              {formatTime(modifiedAt)}
            </span>
          )}
          <button
            onClick={() => setPreview(!preview)}
            className="p-1.5 rounded-md transition-colors hover:bg-[var(--bg-hover)]"
            style={{ color: preview ? 'var(--accent)' : 'var(--text-secondary)' }}
            title={preview ? 'Edit' : 'Preview'}
          >
            {preview ? <FileEdit size={14} /> : <Eye size={14} />}
          </button>
          <button
            onClick={() => save(content)}
            className="p-1.5 rounded-md transition-colors hover:bg-[var(--bg-hover)]"
            style={{ color: 'var(--text-secondary)' }}
            title="Save now"
          >
            <Save size={14} />
          </button>
        </div>
      </div>

      {/* Content */}
      <div className="flex-1 min-h-0 overflow-auto">
        {preview ? (
          <div
            className="p-4 text-sm whitespace-pre-wrap font-mono"
            style={{ color: 'var(--text-primary)' }}
          >
            {content || '(empty)'}
          </div>
        ) : (
          <textarea
            value={content}
            onChange={handleChange}
            className="w-full h-full p-4 resize-none outline-none text-sm font-mono bg-transparent"
            style={{ color: 'var(--text-primary)' }}
            placeholder="# Start typing your notes here..."
            spellCheck={false}
          />
        )}
      </div>
    </div>
  );
}
