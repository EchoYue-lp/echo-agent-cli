import {
  useEffect,
  useLayoutEffect,
  useRef,
  type CSSProperties,
  type ReactNode,
  type RefObject,
} from 'react';

const FOCUSABLE_SELECTOR = [
  'a[href]',
  'button:not([disabled])',
  'input:not([disabled])',
  'select:not([disabled])',
  'textarea:not([disabled])',
  '[tabindex]:not([tabindex="-1"])',
].join(',');

interface ModalStackEntry {
  id: symbol;
  active: boolean;
  overlayRef: RefObject<HTMLDivElement | null>;
  dialogRef: RefObject<HTMLDivElement | null>;
  restoreTarget: HTMLElement | null;
  hasRestoreTarget: boolean;
}

const modalStack: ModalStackEntry[] = [];

function getTopmostModal(): ModalStackEntry | undefined {
  return modalStack
    .slice()
    .reverse()
    .find((entry) => entry.active);
}

function syncModalLayers() {
  const topmost = getTopmostModal();
  for (const entry of modalStack) {
    const blocked = entry !== topmost;
    const overlay = entry.overlayRef.current;
    if (overlay) {
      overlay.inert = blocked;
      overlay.setAttribute('aria-hidden', String(blocked));
    }
  }
}

function focusFirstControl(entry: ModalStackEntry, preferred?: HTMLElement | null) {
  const dialog = entry.dialogRef.current;
  const firstFocusable = dialog?.querySelector<HTMLElement>(FOCUSABLE_SELECTOR);
  (preferred ?? firstFocusable ?? dialog)?.focus();
}

function restoreModalFocus(entry: ModalStackEntry) {
  if (entry.restoreTarget?.isConnected) {
    entry.restoreTarget.focus();
    return;
  }

  const nextTopmost = getTopmostModal();
  if (nextTopmost) focusFirstControl(nextTopmost);
}

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
  const overlayRef = useRef<HTMLDivElement>(null);
  const dialogRef = useRef<HTMLDivElement>(null);
  const onCloseRef = useRef(onClose);
  const entryRef = useRef<ModalStackEntry | null>(null);
  if (!entryRef.current) {
    entryRef.current = {
      id: Symbol('modal'),
      active,
      overlayRef,
      dialogRef,
      restoreTarget: null,
      hasRestoreTarget: false,
    };
  }
  const entry = entryRef.current;
  entry.active = active;

  useEffect(() => {
    onCloseRef.current = onClose;
  }, [onClose]);

  useLayoutEffect(() => {
    modalStack.push(entry);
    syncModalLayers();
    const handleKeyDown = (event: KeyboardEvent) => {
      if (!entry.active || getTopmostModal() !== entry) return;

      if (event.key === 'Escape') {
        event.preventDefault();
        event.stopImmediatePropagation();
        onCloseRef.current();
        return;
      }
      if (event.key !== 'Tab') return;

      const focusable = Array.from(
        entry.dialogRef.current?.querySelectorAll<HTMLElement>(FOCUSABLE_SELECTOR) ?? []
      );
      const first = focusable.at(0);
      const last = focusable.at(-1);
      if (!first || !last) {
        event.preventDefault();
        event.stopImmediatePropagation();
        entry.dialogRef.current?.focus();
      } else if (!entry.dialogRef.current?.contains(document.activeElement)) {
        event.preventDefault();
        event.stopImmediatePropagation();
        first.focus();
      } else if (event.shiftKey && document.activeElement === first) {
        event.preventDefault();
        event.stopImmediatePropagation();
        last.focus();
      } else if (!event.shiftKey && document.activeElement === last) {
        event.preventDefault();
        event.stopImmediatePropagation();
        first.focus();
      }
    };
    document.addEventListener('keydown', handleKeyDown, true);

    return () => {
      document.removeEventListener('keydown', handleKeyDown, true);
      const wasTopmost = getTopmostModal() === entry;
      const index = modalStack.findIndex((candidate) => candidate.id === entry.id);
      if (index >= 0) modalStack.splice(index, 1);
      syncModalLayers();
      if (wasTopmost) restoreModalFocus(entry);
    };
  }, [entry]);

  useLayoutEffect(() => {
    syncModalLayers();
    if (!active || getTopmostModal() !== entry) return;

    if (!entry.hasRestoreTarget) {
      entry.restoreTarget =
        document.activeElement instanceof HTMLElement ? document.activeElement : null;
      entry.hasRestoreTarget = true;
    }

    const dialog = entry.dialogRef.current;
    if (!dialog?.contains(document.activeElement)) {
      focusFirstControl(entry, initialFocusRef?.current);
    }
  }, [active, entry, initialFocusRef]);

  return (
    <div
      ref={overlayRef}
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
          onClick={() => {
            if (getTopmostModal() === entry) onCloseRef.current();
          }}
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
