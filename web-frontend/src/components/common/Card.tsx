import { type ReactNode, type HTMLAttributes } from 'react';

/**
 * Card — shared surface primitive (compound-component pattern, shadcn lineage).
 *
 * The app previously hand-rolled `rounded-* border bg-*` on ~69 components,
 * producing inconsistent radii and the "cards pieced together" look. This
 * primitive is the single source of truth for bordered surfaces. All radii /
 * borders / backgrounds flow from CSS tokens (`--radius-*`, `--border-*`,
 * `--bg-*`), so a token change still cascades everywhere.
 *
 * Three variants cover the three recipes that were duplicated across the codebase:
 *  - `elevated` — a content card (border + primary bg). Replaces the
 *    `rounded-lg border bg-primary` recipe in ProviderPanel / SubagentCard / etc.
 *  - `flat` — a nested sub-card / tile (secondary bg, no border). Replaces the
 *    `rounded-md bg-secondary` metric-tile / icon-tile recipe.
 *  - `overlay` — a floating layer (border + primary bg + shadow). Replaces the
 *    `rounded-lg border bg-primary shadow-md` dropdown / popover recipe.
 *
 * Usage:
 *   <Card variant="elevated">
 *     <CardHeader>...</CardHeader>
 *     <CardContent>...</CardContent>
 *     <CardFooter>...</CardFooter>
 *   </Card>
 *
 * `className` is merged AFTER the variant defaults so callers can override
 * padding / width / etc. without fighting the base styles.
 */

type CardVariant = 'elevated' | 'flat' | 'overlay';

interface CardProps extends HTMLAttributes<HTMLDivElement> {
  variant?: CardVariant;
  /** Rendered element; defaults to div. */
  as?: 'div' | 'section' | 'article' | 'aside';
}

const VARIANT_BASE: Record<CardVariant, string> = {
  // Content card — hairline border + canvas surface.
  elevated: 'rounded-lg border border-[var(--border-primary)] bg-[var(--bg-primary)]',
  // Nested tile — no border, sits on a surface one step down.
  flat: 'rounded-md bg-[var(--bg-secondary)]',
  // Floating layer — same chrome as elevated plus a token shadow.
  overlay:
    'rounded-lg border border-[var(--border-primary)] bg-[var(--bg-primary)] shadow-[var(--shadow-md)]',
};

export function Card({
  variant = 'elevated',
  as: Tag = 'div',
  className = '',
  ...rest
}: CardProps) {
  return <Tag className={`${VARIANT_BASE[variant]} ${className}`.trim()} {...rest} />;
}

interface CardHeaderProps extends HTMLAttributes<HTMLDivElement> {
  children: ReactNode;
}

export function CardHeader({ children, className = '', ...rest }: CardHeaderProps) {
  return (
    <div className={`flex flex-col gap-1 px-4 pt-3 ${className}`.trim()} {...rest}>
      {children}
    </div>
  );
}

interface CardContentProps extends HTMLAttributes<HTMLDivElement> {
  children: ReactNode;
}

export function CardContent({ children, className = '', ...rest }: CardContentProps) {
  return (
    <div className={`px-4 py-3 ${className}`.trim()} {...rest}>
      {children}
    </div>
  );
}

interface CardFooterProps extends HTMLAttributes<HTMLDivElement> {
  children: ReactNode;
}

export function CardFooter({ children, className = '', ...rest }: CardFooterProps) {
  return (
    <div
      className={`flex items-center gap-2 border-t border-[var(--border-secondary)] px-4 py-3 ${className}`.trim()}
      {...rest}
    >
      {children}
    </div>
  );
}
