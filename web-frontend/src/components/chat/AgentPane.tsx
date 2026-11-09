import type { AriaRole, ReactNode, Ref, UIEventHandler } from 'react';

interface AgentPaneProps {
  ariaLabel: string;
  header: ReactNode;
  children: ReactNode;
  footer?: ReactNode;
  role?: AriaRole;
  bodyRef?: Ref<HTMLDivElement>;
  bodyRole?: AriaRole;
  bodyAriaLabel?: string;
  bodyAriaLive?: 'off' | 'polite' | 'assertive';
  bodyClassName?: string;
  onBodyScroll?: UIEventHandler<HTMLDivElement>;
  layout?: 'conversation' | 'home';
}

export function AgentPane({
  ariaLabel,
  header,
  children,
  footer,
  role = 'region',
  bodyRef,
  bodyRole,
  bodyAriaLabel,
  bodyAriaLive,
  bodyClassName = '',
  onBodyScroll,
  layout = 'conversation',
}: AgentPaneProps) {
  return (
    <section
      className="flex h-full min-h-0 min-w-0 flex-col bg-[var(--bg-chat)]"
      role={role}
      aria-label={ariaLabel}
    >
      <header className="flex h-[52px] shrink-0 items-center border-b border-[var(--border-secondary)] bg-[var(--bg-primary)] px-3">
        {header}
      </header>
      <div
        className={`flex min-h-0 flex-1 flex-col ${layout === 'home' ? 'justify-center overflow-y-auto pb-[min(12vh,100px)]' : ''}`}
      >
        <div
          ref={bodyRef}
          role={bodyRole}
          aria-label={bodyAriaLabel}
          aria-live={bodyAriaLive}
          onScroll={onBodyScroll}
          className={`min-h-0 ${layout === 'home' ? 'shrink-0' : 'flex-1 overflow-y-auto'} ${bodyClassName}`}
        >
          {children}
        </div>
        {footer ? <footer className="shrink-0 bg-[var(--bg-chat)]">{footer}</footer> : null}
      </div>
    </section>
  );
}
