import type { ButtonHTMLAttributes, ReactNode } from 'react';
import { injectCss } from './injectCss';

/* CSS ported verbatim from the Claude Design "Quant Engine Design System"
   (components/forms/Button.jsx). */
const CSS = `
.qe-btn {
  --_h: 36px; --_px: 14px; --_fs: 14px;
  display: inline-flex; align-items: center; justify-content: center;
  gap: var(--gap-inline);
  height: var(--_h); padding: 0 var(--_px);
  font-family: var(--font-sans); font-size: var(--_fs); font-weight: var(--fw-semibold);
  letter-spacing: var(--ls-snug); line-height: 1;
  border-radius: var(--radius-md); border: var(--border-w) solid transparent;
  cursor: pointer; white-space: nowrap; user-select: none;
  transition: var(--transition-control);
}
.qe-btn:focus-visible { outline: none; box-shadow: var(--ring); }
.qe-btn:disabled { opacity: 0.45; cursor: not-allowed; pointer-events: none; }
.qe-btn--sm { --_h: 28px; --_px: 10px; --_fs: 13px; }
.qe-btn--lg { --_h: 44px; --_px: 20px; --_fs: 15px; }
.qe-btn--block { width: 100%; }

.qe-btn--primary { background: var(--accent); color: #fff; }
.qe-btn--primary:hover { background: var(--accent-hover); }
.qe-btn--primary:active { background: var(--accent-active); transform: translateY(0.5px); }

.qe-btn--secondary { background: var(--surface-raised); color: var(--text-primary); border-color: var(--border-strong); }
.qe-btn--secondary:hover { background: var(--surface-hover); border-color: var(--border-strong); }
.qe-btn--secondary:active { background: var(--surface-active); }

.qe-btn--ghost { background: transparent; color: var(--text-secondary); }
.qe-btn--ghost:hover { background: var(--surface-hover); color: var(--text-primary); }

.qe-btn--danger { background: var(--down-600); color: #fff; }
.qe-btn--danger:hover { background: var(--down-500); }

.qe-btn--long { background: rgba(52,211,153,0.14); color: var(--up-500); border-color: rgba(52,211,153,0.32); }
.qe-btn--long:hover { background: rgba(52,211,153,0.22); }
.qe-btn--short { background: rgba(255,93,108,0.14); color: var(--down-500); border-color: rgba(255,93,108,0.32); }
.qe-btn--short:hover { background: rgba(255,93,108,0.22); }

.qe-btn__spin {
  width: 14px; height: 14px; border-radius: 50%;
  border: 2px solid currentColor; border-right-color: transparent;
  animation: qe-btn-spin 0.6s linear infinite;
}
@keyframes qe-btn-spin { to { transform: rotate(360deg); } }
`;

injectCss('qe-btn-css', CSS);

export type ButtonVariant =
  | 'primary'
  | 'secondary'
  | 'ghost'
  | 'danger'
  | 'long'
  | 'short';
export type ButtonSize = 'sm' | 'md' | 'lg';

export interface ButtonProps extends ButtonHTMLAttributes<HTMLButtonElement> {
  variant?: ButtonVariant;
  size?: ButtonSize;
  block?: boolean;
  loading?: boolean;
  iconLeft?: ReactNode;
  iconRight?: ReactNode;
}

/** Button — primary action control. */
export function Button({
  variant = 'primary',
  size = 'md',
  block = false,
  loading = false,
  disabled = false,
  iconLeft = null,
  iconRight = null,
  children,
  className = '',
  ...rest
}: ButtonProps) {
  const cls = [
    'qe-btn',
    `qe-btn--${variant}`,
    size !== 'md' ? `qe-btn--${size}` : '',
    block ? 'qe-btn--block' : '',
    className,
  ]
    .filter(Boolean)
    .join(' ');

  return (
    <button className={cls} disabled={disabled || loading} {...rest}>
      {loading && <span className="qe-btn__spin" aria-hidden="true" />}
      {!loading && iconLeft}
      {children && <span>{children}</span>}
      {!loading && iconRight}
    </button>
  );
}
