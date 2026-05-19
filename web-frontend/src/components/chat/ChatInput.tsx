import { useState, useRef, useEffect, useCallback } from 'react';
import { ArrowUp, Square, Paperclip, X, File } from 'lucide-react';
import type { Attachment } from '../../types/api';

interface PendingFile {
  id: string;
  file: File;
  previewUrl: string;
  name: string;
  mime_type: string;
  size: number;
}

interface ChatInputProps {
  onSend: (text: string, attachments?: Attachment[]) => void;
  isStreaming?: boolean;
  onCancel?: () => void;
}

const MAX_FILE_SIZE = 10 * 1024 * 1024; // 10MB

function fileToBase64(file: File): Promise<string> {
  return new Promise((resolve, reject) => {
    const reader = new FileReader();
    reader.onload = () => {
      const result = reader.result as string;
      resolve(result.split(',')[1]); // 去掉 data:...;base64, 前缀
    };
    reader.onerror = reject;
    reader.readAsDataURL(file);
  });
}

function isImageFile(mime: string): boolean {
  return mime.startsWith('image/');
}

export function ChatInput({ onSend, isStreaming, onCancel }: ChatInputProps) {
  const [text, setText] = useState('');
  const [pendingFiles, setPendingFiles] = useState<PendingFile[]>([]);
  const textareaRef = useRef<HTMLTextAreaElement>(null);
  const fileInputRef = useRef<HTMLInputElement>(null);

  useEffect(() => {
    const el = textareaRef.current;
    if (!el) return;
    el.style.height = 'auto';
    el.style.height = Math.min(el.scrollHeight, 200) + 'px';
  }, [text]);

  const addFiles = useCallback((files: FileList | File[]) => {
    const newFiles: PendingFile[] = [];
    for (const file of Array.from(files)) {
      if (file.size > MAX_FILE_SIZE) {
        alert(`文件 "${file.name}" 超过 10MB 大小限制，已跳过`);
        continue;
      }
      newFiles.push({
        id: `pending-${Date.now()}-${Math.random().toString(36).slice(2, 8)}`,
        file,
        previewUrl: isImageFile(file.type) ? URL.createObjectURL(file) : '',
        name: file.name,
        mime_type: file.type || 'application/octet-stream',
        size: file.size,
      });
    }
    setPendingFiles((prev) => [...prev, ...newFiles]);
  }, []);

  const removeFile = useCallback((id: string) => {
    setPendingFiles((prev) => {
      const file = prev.find((f) => f.id === id);
      if (file?.previewUrl) URL.revokeObjectURL(file.previewUrl);
      return prev.filter((f) => f.id !== id);
    });
  }, []);

  const handleFileSelect = useCallback((e: React.ChangeEvent<HTMLInputElement>) => {
    if (e.target.files?.length) {
      addFiles(e.target.files);
      e.target.value = '';
    }
  }, [addFiles]);

  const handlePaste = useCallback((e: React.ClipboardEvent<HTMLTextAreaElement>) => {
    const items = e.clipboardData?.items;
    if (!items) return;

    const imageFiles: File[] = [];
    for (let i = 0; i < items.length; i++) {
      const item = items[i];
      if (item.type.startsWith('image/')) {
        const file = item.getAsFile();
        if (file) imageFiles.push(file);
      }
    }
    if (imageFiles.length > 0) {
      e.preventDefault();
      addFiles(imageFiles);
    }
  }, [addFiles]);

  const handleSend = async () => {
    const trimmed = text.trim();
    if (!trimmed && pendingFiles.length === 0) return;

    const attachmentData: Attachment[] = await Promise.all(
      pendingFiles.map(async (pf) => ({
        name: pf.name,
        mime_type: pf.mime_type,
        data: await fileToBase64(pf.file),
        size: pf.size,
      }))
    );

    // 释放预览 URL
    pendingFiles.forEach((pf) => {
      if (pf.previewUrl) URL.revokeObjectURL(pf.previewUrl);
    });

    onSend(trimmed, attachmentData.length > 0 ? attachmentData : undefined);
    setText('');
    setPendingFiles([]);

    requestAnimationFrame(() => {
      if (textareaRef.current) textareaRef.current.style.height = 'auto';
    });
  };

  const handleKeyDown = (e: React.KeyboardEvent<HTMLTextAreaElement>) => {
    if (e.key === 'Enter' && !e.shiftKey) {
      e.preventDefault();
      handleSend();
    }
  };

  const formatFileSize = (bytes: number): string => {
    if (bytes < 1024) return `${bytes} B`;
    if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(1)} KB`;
    return `${(bytes / (1024 * 1024)).toFixed(1)} MB`;
  };

  const hasContent = text.trim().length > 0 || pendingFiles.length > 0;

  return (
    <div className="px-4 pb-4 pt-2">
      <div className="glass mx-auto flex max-w-3xl flex-col rounded-2xl shadow-[var(--shadow-sm)] transition-shadow focus-within:shadow-[var(--shadow-md)]">
        {/* Attachment previews */}
        {pendingFiles.length > 0 && (
          <div className="flex flex-wrap gap-2 px-4 pt-3">
            {pendingFiles.map((pf) => (
              <div
                key={pf.id}
                className="group relative flex items-center gap-2 rounded-lg border border-[var(--border-primary)] bg-[var(--bg-secondary)] p-2 pr-8"
              >
                {pf.previewUrl ? (
                  <div className="h-10 w-10 shrink-0 overflow-hidden rounded-md">
                    <img
                      src={pf.previewUrl}
                      alt={pf.name}
                      className="h-full w-full object-cover"
                    />
                  </div>
                ) : (
                  <div className="flex h-10 w-10 shrink-0 items-center justify-center rounded-md bg-[var(--bg-hover)]">
                    <File size={16} className="text-[var(--text-tertiary)]" />
                  </div>
                )}
                <div className="min-w-0 max-w-[120px]">
                  <div className="truncate text-xs font-medium text-[var(--text-primary)]">
                    {pf.name}
                  </div>
                  <div className="text-[10px] text-[var(--text-tertiary)]">
                    {formatFileSize(pf.size)}
                  </div>
                </div>
                <button
                  onClick={() => removeFile(pf.id)}
                  className="absolute right-1 top-1 flex h-5 w-5 items-center justify-center rounded-full bg-[var(--bg-primary)] text-[var(--text-tertiary)] opacity-0 shadow-[var(--shadow-sm)] transition-opacity group-hover:opacity-100 hover:text-[var(--text-primary)]"
                >
                  <X size={10} />
                </button>
              </div>
            ))}
          </div>
        )}

        {/* Input row */}
        <div className="flex items-end px-4 py-3">
          {/* Attachment button */}
          <button
            onClick={() => fileInputRef.current?.click()}
            className="mr-1 flex h-8 w-8 shrink-0 items-center justify-center rounded-lg text-[var(--text-tertiary)] transition-colors hover:bg-[var(--bg-hover)] hover:text-[var(--text-primary)]"
            title="上传附件"
          >
            <Paperclip size={16} />
          </button>
          <input
            ref={fileInputRef}
            type="file"
            multiple
            accept="image/*,.pdf,.txt,.doc,.docx,.xls,.xlsx,.csv,.json,.xml,.yaml,.yml,.md,.log"
            className="hidden"
            onChange={handleFileSelect}
          />

          <textarea
            ref={textareaRef}
            value={text}
            onChange={(e) => setText(e.target.value)}
            onKeyDown={handleKeyDown}
            onPaste={handlePaste}
            rows={1}
            placeholder="给 Echo Agent 发送消息..."
            className="max-h-[200px] min-h-[24px] flex-1 resize-none bg-transparent text-sm leading-relaxed text-[var(--text-primary)] outline-none placeholder:text-[var(--text-tertiary)]"
          />
          {isStreaming ? (
            <button
              onClick={onCancel}
              className="ml-2 flex h-8 w-8 shrink-0 items-center justify-center rounded-lg bg-[var(--text-secondary)] text-white transition-all hover:bg-[var(--text-primary)]"
              title="停止生成"
            >
              <Square size={14} fill="white" color="white" />
            </button>
          ) : (
            <button
              onClick={handleSend}
              disabled={!hasContent}
              className="ml-2 flex h-8 w-8 shrink-0 items-center justify-center rounded-lg bg-[var(--accent)] text-white transition-all hover:shadow-[var(--shadow-glow)] disabled:opacity-20 disabled:hover:shadow-none"
            >
              <ArrowUp size={16} strokeWidth={2.5} />
            </button>
          )}
        </div>
      </div>
      <p className="mx-auto mt-2 max-w-3xl text-center text-[11px] text-[var(--text-tertiary)]">
        Echo Agent 可能会犯错，请核实重要信息
      </p>
    </div>
  );
}
