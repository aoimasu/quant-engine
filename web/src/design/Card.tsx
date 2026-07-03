import type { HTMLAttributes, ReactNode } from 'react';
import { injectCss } from './injectCss';

/* CSS ported verbatim from the Claude Design "Quant Engine Design System"
   (components/data/Card.jsx). */
const CSS = `
.qe-card {
  background: var(--surface-card); border: var(--border-w) solid var(--border-subtle);
  border-radius: var(--radius-lg); box-shadow: var(--highlight-top);
  color: var(--text-primary); overflow: hidden;
}
.qe-card--raised { background: var(--surface-raised); box-shadow: var(--shadow-sm), var(--highlight-top); }
.qe-card--flat { box-shadow: none; }
.qe-card--pad { padding: var(--pad-card); }
.qe-card--interactive { transition: var(--transition-control); cursor: pointer; }
.qe-card--interactive:hover { border-color: var(--border-strong); background: var(--surface-raised); }
.qe-card__head {
  display: flex; align-items: center; justify-content: space-between; gap: 12px;
  padding: 12px var(--pad-card); border-bottom: var(--border-w) solid var(--border-subtle);
}
.qe-card__title { font-family: var(--font-display); font-size: var(--fs-md); font-weight: var(--fw-semibold); letter-spacing: var(--ls-snug); }
.qe-card__sub { font-size: var(--fs-caption); color: var(--text-muted); margin-top: 1px; }
.qe-card__body { padding: var(--pad-card); }
`;

injectCss('qe-card-css', CSS);

export interface CardProps extends Omit<HTMLAttributes<HTMLDivElement>, 'title'> {
  title?: ReactNode;
  subtitle?: ReactNode;
  actions?: ReactNode;
  raised?: boolean;
  flat?: boolean;
  interactive?: boolean;
  pad?: boolean;
  children?: ReactNode;
}

/** Card — the base surface container for panels and modules. */
export function Card({
  title,
  subtitle,
  actions,
  raised = false,
  flat = false,
  interactive = false,
  pad = false,
  children,
  className = '',
  ...rest
}: CardProps) {
  const cls = [
    'qe-card',
    raised ? 'qe-card--raised' : '',
    flat ? 'qe-card--flat' : '',
    interactive ? 'qe-card--interactive' : '',
    pad ? 'qe-card--pad' : '',
    className,
  ]
    .filter(Boolean)
    .join(' ');
  const hasHead = title || actions;
  return (
    <div className={cls} {...rest}>
      {hasHead && (
        <div className="qe-card__head">
          <div>
            {title && <div className="qe-card__title">{title}</div>}
            {subtitle && <div className="qe-card__sub">{subtitle}</div>}
          </div>
          {actions && <div style={{ display: 'flex', gap: 8, alignItems: 'center' }}>{actions}</div>}
        </div>
      )}
      {hasHead ? <div className="qe-card__body">{children}</div> : children}
    </div>
  );
}
