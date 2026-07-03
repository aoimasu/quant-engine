import type { InputHTMLAttributes, ReactNode } from 'react';
import { useId } from 'react';
import { injectCss } from './injectCss';

/* CSS ported verbatim from the Claude Design "Quant Engine Design System"
   (components/forms/Input.jsx). */
const CSS = `
.qe-field { display: flex; flex-direction: column; gap: 6px; }
.qe-field__label {
  font-family: var(--font-sans); font-size: var(--fs-sm); font-weight: var(--fw-medium);
  color: var(--text-secondary);
}
.qe-field__hint { font-size: var(--fs-caption); color: var(--text-muted); }
.qe-field__err { font-size: var(--fs-caption); color: var(--down-500); }

.qe-input-wrap {
  display: flex; align-items: center; gap: 8px;
  height: 36px; padding: 0 10px;
  background: var(--surface-inset); color: var(--text-primary);
  border: var(--border-w) solid var(--border-default); border-radius: var(--radius-md);
  transition: var(--transition-control);
}
.qe-input-wrap:hover { border-color: var(--border-strong); }
.qe-input-wrap:focus-within { border-color: var(--accent); box-shadow: var(--ring); }
.qe-input-wrap--err { border-color: var(--down-500); }
.qe-input-wrap--err:focus-within { box-shadow: 0 0 0 3px rgba(255,93,108,0.3); }
.qe-input-wrap--sm { height: 28px; }
.qe-input-wrap--lg { height: 44px; }
.qe-input-wrap--mono .qe-input { font-family: var(--font-mono); font-variant-numeric: tabular-nums; }

.qe-input {
  flex: 1; min-width: 0; height: 100%;
  background: transparent; border: none; outline: none;
  color: inherit; font-family: var(--font-sans); font-size: var(--fs-body);
}
.qe-input::placeholder { color: var(--text-muted); }
.qe-input:disabled { cursor: not-allowed; }
.qe-input-wrap:has(.qe-input:disabled) { opacity: 0.5; }
.qe-input__affix { color: var(--text-muted); display: inline-flex; align-items: center; font-size: var(--fs-sm); }
.qe-input__affix svg { width: 16px; height: 16px; }
`;

injectCss('qe-input-css', CSS);

export type InputSize = 'sm' | 'md' | 'lg';

export interface InputProps extends Omit<InputHTMLAttributes<HTMLInputElement>, 'size' | 'prefix'> {
  label?: ReactNode;
  hint?: ReactNode;
  error?: ReactNode;
  prefix?: ReactNode;
  suffix?: ReactNode;
  size?: InputSize;
  mono?: boolean;
}

/** Input — text/number field with optional label, affixes, and error. */
export function Input({
  label,
  hint,
  error,
  prefix = null,
  suffix = null,
  size = 'md',
  mono = false,
  id,
  className = '',
  ...rest
}: InputProps) {
  const autoId = useId();
  const fieldId = id ?? (label ? `qe-in-${autoId}` : undefined);
  const wrapCls = [
    'qe-input-wrap',
    size !== 'md' ? `qe-input-wrap--${size}` : '',
    error ? 'qe-input-wrap--err' : '',
    mono ? 'qe-input-wrap--mono' : '',
  ]
    .filter(Boolean)
    .join(' ');

  const control = (
    <div className={wrapCls}>
      {prefix && <span className="qe-input__affix">{prefix}</span>}
      <input id={fieldId} className="qe-input" {...rest} />
      {suffix && <span className="qe-input__affix">{suffix}</span>}
    </div>
  );

  if (!label && !hint && !error) return <div className={className}>{control}</div>;
  return (
    <div className={`qe-field ${className}`.trim()}>
      {label && (
        <label className="qe-field__label" htmlFor={fieldId}>
          {label}
        </label>
      )}
      {control}
      {error ? (
        <span className="qe-field__err">{error}</span>
      ) : (
        hint && <span className="qe-field__hint">{hint}</span>
      )}
    </div>
  );
}
