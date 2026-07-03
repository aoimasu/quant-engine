import type { HTMLAttributes, ReactNode } from 'react';
import { injectCss } from './injectCss';

/* CSS ported verbatim from the Claude Design "Quant Engine Design System"
   (components/feedback/Tag.jsx). */
const CSS = `
.qe-tag {
  display: inline-flex; align-items: center; gap: 6px;
  height: 24px; padding: 0 8px; border-radius: var(--radius-sm);
  background: var(--surface-raised); border: var(--border-w) solid var(--border-default);
  color: var(--text-secondary); font-family: var(--font-sans); font-size: var(--fs-sm);
  white-space: nowrap;
}
.qe-tag--mono { font-family: var(--font-mono); font-size: var(--fs-caption); }
.qe-tag__x {
  display: inline-flex; align-items: center; justify-content: center;
  width: 15px; height: 15px; margin-right: -2px; border-radius: var(--radius-xs);
  cursor: pointer; color: var(--text-muted); border: none; background: transparent; padding: 0;
}
.qe-tag__x:hover { background: var(--surface-active); color: var(--text-primary); }
.qe-tag__x svg { width: 11px; height: 11px; }
.qe-tag__swatch { width: 8px; height: 8px; border-radius: 2px; flex: none; }
`;

injectCss('qe-tag-css', CSS);

export interface TagProps extends HTMLAttributes<HTMLSpanElement> {
  mono?: boolean;
  color?: string;
  onRemove?: () => void;
  children?: ReactNode;
}

/** Tag — removable token for filters, symbols, and labels. */
export function Tag({ children, mono = false, color, onRemove, className = '', ...rest }: TagProps) {
  const cls = ['qe-tag', mono ? 'qe-tag--mono' : '', className].filter(Boolean).join(' ');
  return (
    <span className={cls} {...rest}>
      {color && <span className="qe-tag__swatch" style={{ background: color }} />}
      {children}
      {onRemove && (
        <button type="button" className="qe-tag__x" onClick={onRemove} aria-label="Remove">
          <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2.5" strokeLinecap="round">
            <path d="M18 6 6 18M6 6l12 12" />
          </svg>
        </button>
      )}
    </span>
  );
}
