import { useEffect, useRef, type CSSProperties, type ReactNode, type RefObject } from 'react';

const FOCUSABLE_SELECTOR = [
  'a[href]',
  'button:not([disabled])',
  'input:not([disabled])',
  'select:not([disabled])',
  'textarea:not([disabled])',
  '[tabindex]:not([tabindex="-1"])',
].join(',');

interface ModalProps {
  children: ReactNode;
  onClose: () => void;
  ariaLabel?: string;
  ariaLabelledBy?: string;
  className?: string;
  overlayClassName?: string;
  style?: CSSProperties;
  initialFocusRef?: RefObject<HTMLElement | null>;
  closeOnBackdrop?: boolean;
  active?: boolean;
}

export function Modal({
  children,
  onClose,
  ariaLabel,
  ariaLabelledBy,
  className,
  overlayClassName,
  style,
  initialFocusRef,
  closeOnBackdrop = true,
  active = true,
}: ModalProps) {
  const dialogRef = useRef<HTMLDivElement>(null);
  const onCloseRef = useRef(onClose);

  useEffect(() => {
    onCloseRef.current = onClose;
  }, [onClose]);

  useEffect(() => {
    if (!active) return;
    const previouslyFocused =
      document.activeElement instanceof HTMLElement ? document.activeElement : null;
    const dialog = dialogRef.current;
    const preferredTarget = initialFocusRef?.current;
    const firstFocusable = dialog?.querySelector<HTMLElement>(FOCUSABLE_SELECTOR);
    (preferredTarget ?? firstFocusable ?? dialog)?.focus();

    const handleKeyDown = (event: KeyboardEvent) => {
      if (event.key === 'Escape') {
        event.preventDefault();
        onCloseRef.current();
        return;
      }
      if (event.key !== 'Tab') return;

      const focusable = Array.from(
        dialogRef.current?.querySelectorAll<HTMLElement>(FOCUSABLE_SELECTOR) ?? []
      );
      const first = focusable.at(0);
      const last = focusable.at(-1);
      if (!first || !last) {
        event.preventDefault();
        dialogRef.current?.focus();
      } else if (!dialogRef.current?.contains(document.activeElement)) {
        event.preventDefault();
        first.focus();
      } else if (event.shiftKey && document.activeElement === first) {
        event.preventDefault();
        last.focus();
      } else if (!event.shiftKey && document.activeElement === last) {
        event.preventDefault();
        first.focus();
      }
    };
    document.addEventListener('keydown', handleKeyDown);

    return () => {
      document.removeEventListener('keydown', handleKeyDown);
      previouslyFocused?.focus();
    };
  }, [active, initialFocusRef]);

  return (
    <div
      aria-hidden={!active}
      inert={!active}
      className={`fixed inset-0 z-50 flex items-center justify-center ${overlayClassName ?? ''}`}
    >
      {closeOnBackdrop ? (
        <button
          type="button"
          tabIndex={-1}
          aria-label="关闭对话框"
          className="absolute inset-0 cursor-default"
          style={{ background: 'var(--bg-overlay)' }}
          onClick={active ? onClose : undefined}
        />
      ) : (
        <div
          aria-hidden="true"
          className="absolute inset-0"
          style={{ background: 'var(--bg-overlay)' }}
        />
      )}
      <div
        ref={dialogRef}
        role="dialog"
        aria-modal="true"
        aria-label={ariaLabel}
        aria-labelledby={ariaLabelledBy}
        tabIndex={-1}
        className={className}
        style={style}
      >
        {children}
      </div>
    </div>
  );
}
