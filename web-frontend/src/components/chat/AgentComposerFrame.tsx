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
        className={`flex items-end ${variant === 'primary' ? 'min-h-[138px] gap-2 px-6 py-4' : 'min-h-11 px-2.5 py-1.5'}`}
      >
        {children}
      </div>
    </div>
  );
}
