import type { HTMLAttributes, ReactNode } from 'react';

interface AgentComposerFrameProps extends HTMLAttributes<HTMLDivElement> {
  children: ReactNode;
  variant?: 'compact' | 'primary';
}

export function AgentComposerFrame({
  children,
  className = '',
  variant = 'compact',
  ...props
}: AgentComposerFrameProps) {
  return (
    <div
      data-composer-variant={variant}
      className={`overflow-visible ${variant === 'primary' ? 'relative rounded-[21px] shadow-[var(--shadow-sm)]' : 'rounded-lg'} border border-[var(--border-primary)] bg-[var(--bg-input)] transition-colors focus-within:border-[var(--border-focus)] ${className}`}
      {...props}
    >
      <div
        className={`flex ${variant === 'primary' ? 'min-h-[108px] flex-col items-stretch gap-1 px-4 py-2.5' : 'min-h-11 items-end px-2.5 py-1.5'}`}
      >
        {children}
      </div>
    </div>
  );
}
