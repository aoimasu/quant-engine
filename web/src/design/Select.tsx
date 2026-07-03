import type { ReactNode, SelectHTMLAttributes } from 'react';
import { injectCss } from './injectCss';

/* CSS ported verbatim from the Claude Design "Quant Engine Design System"
   (components/forms/Select.jsx). */
const CSS = `
.qe-select-wrap { position: relative; display: inline-flex; width: 100%; }
.qe-select {
  appearance: none; width: 100%; height: 36px;
  padding: 0 34px 0 10px;
  background: var(--surface-inset); color: var(--text-primary);
  border: var(--border-w) solid var(--border-default); border-radius: var(--radius-md);
  font-family: var(--font-sans); font-size: var(--fs-body); cursor: pointer;
  transition: var(--transition-control);
}
.qe-select:hover { border-color: var(--border-strong); }
.qe-select:focus-visible { outline: none; border-color: var(--accent); box-shadow: var(--ring); }
.qe-select:disabled { opacity: 0.5; cursor: not-allowed; }
.qe-select--sm { height: 28px; font-size: var(--fs-sm); }
.qe-select--lg { height: 44px; }
.qe-select__chev {
  position: absolute; right: 10px; top: 50%; transform: translateY(-50%);
  pointer-events: none; color: var(--text-muted);
  width: 16px; height: 16px;
}
`;

injectCss('qe-select-css', CSS);

export type SelectSize = 'sm' | 'md' | 'lg';

/** An option is either a bare string or an explicit `{ value, label }`. */
export type SelectOption = string | { value: string; label: string };

export interface SelectProps extends Omit<SelectHTMLAttributes<HTMLSelectElement>, 'size'> {
  options?: SelectOption[];
  size?: SelectSize;
  children?: ReactNode;
}

/** Select — native dropdown with brand chrome. */
export function Select({ options = [], size = 'md', className = '', children, ...rest }: SelectProps) {
  const cls = ['qe-select', size !== 'md' ? `qe-select--${size}` : ''].filter(Boolean).join(' ');
  return (
    <span className={`qe-select-wrap ${className}`.trim()}>
      <select className={cls} {...rest}>
        {children ||
          options.map((o) => {
            const value = typeof o === 'string' ? o : o.value;
            const label = typeof o === 'string' ? o : o.label;
            return (
              <option key={value} value={value}>
                {label}
              </option>
            );
          })}
      </select>
      <svg
        className="qe-select__chev"
        viewBox="0 0 24 24"
        fill="none"
        stroke="currentColor"
        strokeWidth="2"
        strokeLinecap="round"
        strokeLinejoin="round"
        aria-hidden="true"
      >
        <path d="m6 9 6 6 6-6" />
      </svg>
    </span>
  );
}
