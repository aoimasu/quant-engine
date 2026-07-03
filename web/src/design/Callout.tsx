import type { HTMLAttributes, ReactNode } from 'react';
import { injectCss } from './injectCss';

/* CSS ported verbatim from the Claude Design "Quant Engine Design System"
   (components/feedback/Callout.jsx). */
const CSS = `
.qe-callout {
  display: flex; gap: 10px; padding: 12px 14px;
  border-radius: var(--radius-md); border: var(--border-w) solid var(--border-default);
  background: var(--surface-card); font-size: var(--fs-sm); line-height: 1.5;
  border-left-width: 3px;
}
.qe-callout__icon { flex: none; margin-top: 1px; }
.qe-callout__icon svg { width: 16px; height: 16px; }
.qe-callout__body { color: var(--text-secondary); }
.qe-callout__title { color: var(--text-primary); font-weight: var(--fw-semibold); margin-bottom: 2px; }
.qe-callout--info { border-left-color: var(--info-500); }
.qe-callout--info .qe-callout__icon { color: var(--info-500); }
.qe-callout--warn { border-left-color: var(--warn-500); }
.qe-callout--warn .qe-callout__icon { color: var(--warn-500); }
.qe-callout--danger { border-left-color: var(--down-500); }
.qe-callout--danger .qe-callout__icon { color: var(--down-500); }
.qe-callout--success { border-left-color: var(--up-500); }
.qe-callout--success .qe-callout__icon { color: var(--up-500); }
.qe-callout--accent { border-left-color: var(--accent); }
.qe-callout--accent .qe-callout__icon { color: var(--violet-400); }
`;

injectCss('qe-callout-css', CSS);

export type CalloutVariant = 'info' | 'warn' | 'danger' | 'success' | 'accent';

const GLYPH: Record<CalloutVariant, string> = {
  info: 'M12 16v-4M12 8h.01M12 22a10 10 0 1 0 0-20 10 10 0 0 0 0 20Z',
  warn: 'M12 9v4M12 17h.01M10.3 3.9 1.8 18a2 2 0 0 0 1.7 3h17a2 2 0 0 0 1.7-3L13.7 3.9a2 2 0 0 0-3.4 0Z',
  danger: 'M12 8v4M12 16h.01M12 22a10 10 0 1 0 0-20 10 10 0 0 0 0 20Z',
  success: 'M22 11.08V12a10 10 0 1 1-5.93-9.14M22 4 12 14.01l-3-3',
  accent: 'M12 16v-4M12 8h.01M12 22a10 10 0 1 0 0-20 10 10 0 0 0 0 20Z',
};

export interface CalloutProps extends Omit<HTMLAttributes<HTMLDivElement>, 'title'> {
  variant?: CalloutVariant;
  title?: ReactNode;
  children?: ReactNode;
}

/** Callout — inline banner for context, warnings, or risk notes. */
export function Callout({
  variant = 'info',
  title,
  children,
  className = '',
  ...rest
}: CalloutProps) {
  return (
    <div className={`qe-callout qe-callout--${variant} ${className}`.trim()} {...rest}>
      <span className="qe-callout__icon">
        <svg
          viewBox="0 0 24 24"
          fill="none"
          stroke="currentColor"
          strokeWidth="2"
          strokeLinecap="round"
          strokeLinejoin="round"
        >
          <path d={GLYPH[variant]} />
        </svg>
      </span>
      <div className="qe-callout__body">
        {title && <div className="qe-callout__title">{title}</div>}
        {children}
      </div>
    </div>
  );
}
