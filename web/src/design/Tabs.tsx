import { injectCss } from './injectCss';

/* CSS ported verbatim from the Claude Design "Quant Engine Design System"
   (components/navigation/Tabs.jsx). */
const CSS = `
.qe-tabs { display: flex; align-items: center; gap: 2px; border-bottom: var(--border-w) solid var(--border-default); }
.qe-tab {
  appearance: none; border: none; background: transparent; cursor: pointer;
  position: relative; padding: 9px 12px; margin-bottom: -1px;
  font-family: var(--font-sans); font-size: var(--fs-sm); font-weight: var(--fw-medium);
  color: var(--text-tertiary); display: inline-flex; align-items: center; gap: 7px;
  border-bottom: 2px solid transparent; transition: var(--transition-control); white-space: nowrap;
}
.qe-tab:hover { color: var(--text-primary); }
.qe-tab[aria-selected="true"] { color: var(--text-primary); border-bottom-color: var(--accent); }
.qe-tab__count {
  font-family: var(--font-mono); font-size: 10px; padding: 1px 5px; border-radius: var(--radius-full);
  background: var(--surface-raised); color: var(--text-muted);
}
.qe-tab[aria-selected="true"] .qe-tab__count { background: var(--accent-fill-soft); color: var(--violet-300); }
.qe-tabs--pill { border-bottom: none; gap: 4px; padding: 3px; background: var(--surface-inset); border-radius: var(--radius-md); border: var(--border-w) solid var(--border-default); display: inline-flex; }
.qe-tabs--pill .qe-tab { padding: 5px 12px; margin-bottom: 0; border-bottom: none; border-radius: var(--radius-sm); }
.qe-tabs--pill .qe-tab[aria-selected="true"] { background: var(--surface-raised); box-shadow: var(--shadow-xs); }
`;

injectCss('qe-tabs-css', CSS);

/** A tab is either a bare string value or an explicit `{ value, label, count? }`. */
export type TabItem = string | { value: string; label: string; count?: number };

export interface TabsProps {
  tabs?: TabItem[];
  value?: string;
  onChange?: (value: string) => void;
  variant?: 'underline' | 'pill';
  className?: string;
}

/** Tabs — horizontal section switcher (underline or pill style). */
export function Tabs({ tabs = [], value, onChange, variant = 'underline', className = '' }: TabsProps) {
  const cls = ['qe-tabs', variant === 'pill' ? 'qe-tabs--pill' : '', className]
    .filter(Boolean)
    .join(' ');
  return (
    <div className={cls} role="tablist">
      {tabs.map((t) => {
        const v = typeof t === 'string' ? t : t.value;
        const label = typeof t === 'string' ? t : t.label;
        const count = typeof t === 'object' ? t.count : undefined;
        return (
          <button
            key={v}
            type="button"
            role="tab"
            aria-selected={v === value}
            className="qe-tab"
            onClick={() => onChange && onChange(v)}
          >
            {label}
            {count != null && <span className="qe-tab__count">{count}</span>}
          </button>
        );
      })}
    </div>
  );
}
