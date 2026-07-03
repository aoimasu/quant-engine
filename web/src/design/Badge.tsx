import type { HTMLAttributes, ReactNode } from 'react';
import { injectCss } from './injectCss';

/* CSS ported verbatim from the Claude Design "Quant Engine Design System"
   (components/feedback/Badge.jsx). */
const CSS = `
.qe-badge {
  display: inline-flex; align-items: center; gap: 5px;
  height: 20px; padding: 0 8px; border-radius: var(--radius-sm);
  font-family: var(--font-mono); font-size: 11px; font-weight: var(--fw-medium);
  letter-spacing: 0.02em; white-space: nowrap; line-height: 1;
  border: var(--border-w) solid transparent;
}
.qe-badge__dot { width: 6px; height: 6px; border-radius: 50%; background: currentColor; flex: none; }
.qe-badge--neutral { background: var(--surface-raised); color: var(--text-secondary); border-color: var(--border-default); }
.qe-badge--accent { background: var(--accent-fill-soft); color: var(--violet-300); border-color: rgba(124,92,255,0.3); }
.qe-badge--up { background: var(--up-050); color: var(--up-500); border-color: rgba(52,211,153,0.3); }
.qe-badge--down { background: var(--down-050); color: var(--down-500); border-color: rgba(255,93,108,0.3); }
.qe-badge--warn { background: var(--warn-050); color: var(--warn-500); border-color: rgba(247,185,85,0.3); }
.qe-badge--info { background: var(--info-050); color: var(--info-500); border-color: rgba(76,199,245,0.3); }
.qe-badge--solid { border: none; }
.qe-badge--solid.qe-badge--up { background: var(--up-500); color: #04140d; }
.qe-badge--solid.qe-badge--down { background: var(--down-500); color: #1a0407; }
.qe-badge--solid.qe-badge--accent { background: var(--accent); color: #fff; }
`;

injectCss('qe-badge-css', CSS);

export type BadgeVariant = 'neutral' | 'accent' | 'up' | 'down' | 'warn' | 'info';

export interface BadgeProps extends HTMLAttributes<HTMLSpanElement> {
  variant?: BadgeVariant;
  solid?: boolean;
  dot?: boolean;
  children?: ReactNode;
}

/** Badge — compact status label, optionally with a leading status dot. */
export function Badge({
  variant = 'neutral',
  solid = false,
  dot = false,
  children,
  className = '',
  ...rest
}: BadgeProps) {
  const cls = ['qe-badge', `qe-badge--${variant}`, solid ? 'qe-badge--solid' : '', className]
    .filter(Boolean)
    .join(' ');
  return (
    <span className={cls} {...rest}>
      {dot && <span className="qe-badge__dot" />}
      {children}
    </span>
  );
}
