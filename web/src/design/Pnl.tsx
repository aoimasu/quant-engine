import type { HTMLAttributes } from 'react';
import { injectCss } from './injectCss';

/* CSS ported verbatim from the Claude Design "Quant Engine Design System"
   (components/data/Pnl.jsx). */
const CSS = `
.qe-pnl { font-family: var(--font-mono); font-variant-numeric: tabular-nums; font-weight: var(--fw-medium); display: inline-flex; align-items: center; gap: 3px; }
.qe-pnl--up { color: var(--up-500); }
.qe-pnl--down { color: var(--down-500); }
.qe-pnl--flat { color: var(--text-tertiary); }
.qe-pnl__arrow { font-size: 0.85em; }
.qe-pnl--pill { padding: 2px 7px; border-radius: var(--radius-sm); font-size: var(--fs-caption); }
.qe-pnl--pill.qe-pnl--up { background: var(--up-050); }
.qe-pnl--pill.qe-pnl--down { background: var(--down-050); }
.qe-pnl--pill.qe-pnl--flat { background: var(--surface-raised); }
`;

injectCss('qe-pnl-css', CSS);

function fmt(n: number, digits: number): string {
  const abs = Math.abs(n);
  return abs.toLocaleString('en-US', {
    minimumFractionDigits: digits,
    maximumFractionDigits: digits,
  });
}

export type PnlFormat = 'number' | 'currency' | 'percent';

export interface PnlProps extends HTMLAttributes<HTMLSpanElement> {
  value: number;
  format?: PnlFormat;
  digits?: number;
  prefix?: string;
  suffix?: string;
  showArrow?: boolean;
  showSign?: boolean;
  pill?: boolean;
}

/** Pnl — signed, color-coded numeric value (P&L, deltas, returns). */
export function Pnl({
  value,
  format = 'number',
  digits = 2,
  prefix = '',
  suffix = '',
  showArrow = false,
  showSign = true,
  pill = false,
  className = '',
  ...rest
}: PnlProps) {
  const dir = value > 0 ? 'up' : value < 0 ? 'down' : 'flat';
  let body: string;
  if (format === 'currency')
    body = `${value < 0 ? '-' : showSign && value > 0 ? '+' : ''}$${fmt(value, digits)}`;
  else if (format === 'percent')
    body = `${value < 0 ? '' : showSign && value > 0 ? '+' : ''}${fmt(value, digits)}%`;
  else body = `${value < 0 ? '' : showSign && value > 0 ? '+' : ''}${fmt(value, digits)}`;
  if (format === 'percent' && value < 0) body = `-${fmt(value, digits)}%`;

  const cls = ['qe-pnl', `qe-pnl--${dir}`, pill ? 'qe-pnl--pill' : '', className]
    .filter(Boolean)
    .join(' ');
  return (
    <span className={cls} {...rest}>
      {showArrow && dir !== 'flat' && (
        <span className="qe-pnl__arrow">{dir === 'up' ? '▲' : '▼'}</span>
      )}
      {prefix}
      {body}
      {suffix}
    </span>
  );
}
