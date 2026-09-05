import type { HTMLAttributes, ReactNode } from 'react';

interface AgentComposerFrameProps extends HTMLAttributes<HTMLDivElement> {
  children: ReactNode;
}

export function AgentComposerFrame({
  children,
  className = '',
  ...props
}: AgentComposerFrameProps) {
  return (
    <div
      className={`overflow-visible rounded-lg border border-[var(--border-primary)] bg-[var(--bg-input)] transition-colors focus-within:border-[var(--border-focus)] ${className}`}
      {...props}
    >
      <div className="flex min-h-11 items-end px-2.5 py-1.5">{children}</div>
    </div>
  );
}
