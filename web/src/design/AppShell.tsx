import { useEffect, useRef, useState, type ReactNode } from 'react';
import { Icon } from './Icon';
import { Badge } from './Badge';
import { Button } from './Button';
import { injectCss } from './injectCss';
import logoLockup from '../assets/logo-lockup.svg';

/* Shell CSS ported verbatim from the Claude Design "Quant Engine Design System"
   (ui_kits/trading-dashboard/AppShell.jsx), plus a disabled-navitem rule for the
   present-but-disabled Trade/Risk placeholders (QE-258 scope). */
const CSS = `
.qe-app { display: grid; grid-template-columns: var(--sidebar-w) 1fr; height: 100vh; background: var(--bg-app); color: var(--text-primary); }
.qe-side { display: flex; flex-direction: column; border-right: 1px solid var(--border-subtle); background: var(--surface-base); min-height: 0; }
.qe-side__brand { display: flex; align-items: center; gap: 10px; height: var(--topbar-h); padding: 0 16px; border-bottom: 1px solid var(--border-subtle); }
.qe-side__brand img { height: 26px; }
.qe-side__nav { flex: 1; overflow-y: auto; padding: 10px 10px; display: flex; flex-direction: column; gap: 2px; }
.qe-side__sec { font: 500 10px var(--font-mono); text-transform: uppercase; letter-spacing: .1em; color: var(--text-muted); padding: 14px 10px 6px; }
.qe-navitem { display: flex; align-items: center; gap: 10px; padding: 7px 10px; border-radius: var(--radius-md); color: var(--text-tertiary); font-size: 13px; font-weight: 500; cursor: pointer; transition: var(--transition-control); border: none; background: transparent; text-align: left; width: 100%; font-family: var(--font-sans); }
.qe-navitem:hover { background: var(--surface-hover); color: var(--text-primary); }
.qe-navitem--active { background: var(--accent-fill-soft); color: var(--violet-200); }
.qe-navitem--active .qe-navitem__ic { color: var(--violet-300); }
.qe-navitem:disabled { opacity: 0.4; cursor: not-allowed; pointer-events: none; }
.qe-navitem__ic { color: var(--text-muted); display: inline-flex; }
.qe-navitem__badge { margin-left: auto; }
.qe-navitem__soon { margin-left: auto; font: 500 9px var(--font-mono); text-transform: uppercase; letter-spacing: .08em; color: var(--text-muted); }
.qe-side__foot { border-top: 1px solid var(--border-subtle); padding: 12px; display: flex; align-items: center; gap: 10px; }
.qe-side__foot .qe-foot__avatar { width: 28px; height: 28px; border-radius: var(--radius-md); background: var(--accent-fill-soft); color: var(--violet-200); display: flex; align-items: center; justify-content: center; font: 600 12px var(--font-display); flex: none; }
.qe-side__foot .n { font-size: 12px; font-weight: 600; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
.qe-side__foot .s { font: 500 10px var(--font-mono); color: var(--up-500); display: flex; align-items: center; gap: 4px; }
.qe-side__foot .dot { width: 6px; height: 6px; border-radius: 50%; background: var(--up-500); box-shadow: 0 0 8px var(--up-500); }

.qe-main { display: flex; flex-direction: column; min-width: 0; min-height: 0; }
.qe-top { display: flex; align-items: center; gap: 14px; height: var(--topbar-h); padding: 0 18px; border-bottom: 1px solid var(--border-subtle); background: var(--surface-base); flex: none; }
.qe-top__title { font-family: var(--font-display); font-size: 15px; font-weight: 600; }
.qe-top__spacer { flex: 1; }
.qe-top__clock { font: 500 12px var(--font-mono); color: var(--text-tertiary); font-variant-numeric: tabular-nums; }
.qe-top__search { display: flex; align-items: center; gap: 8px; height: 32px; padding: 0 10px; width: 220px; background: var(--surface-inset); border: 1px solid var(--border-default); border-radius: var(--radius-md); color: var(--text-muted); font-size: 13px; }
.qe-top__search input { flex: 1; background: transparent; border: none; outline: none; color: var(--text-primary); font-family: var(--font-sans); font-size: 13px; }
.qe-content { flex: 1; overflow-y: auto; min-height: 0; }
`;

injectCss('qe-shell-css', CSS);

export interface NavItem {
  /** Section header row (mutually exclusive with an item). */
  sec?: string;
  id?: string;
  label?: string;
  icon?: string;
  badge?: string;
  /** Present-but-disabled placeholder (Trade / Risk — Phase 3). */
  disabled?: boolean;
}

/*
 * v1 nav (spec §7.2): only the Research group is active. Trade (Dashboard /
 * Positions / Orders) and Risk / API & docs are present-but-disabled
 * placeholders — their screens land in Phase 3 / QE-259+.
 */
const NAV: NavItem[] = [
  { sec: 'Trade' },
  { id: 'dashboard', label: 'Dashboard', icon: 'layout-dashboard', disabled: true },
  { id: 'positions', label: 'Positions', icon: 'wallet', disabled: true },
  { id: 'orders', label: 'Orders', icon: 'list-checks', disabled: true },
  { sec: 'Research' },
  { id: 'strategies', label: 'Strategies', icon: 'git-branch' },
  { id: 'training', label: 'Training', icon: 'activity' },
  { id: 'backtest', label: 'Backtests', icon: 'flask-conical' },
  { id: 'data', label: 'Market data', icon: 'database' },
  { sec: 'System' },
  { id: 'risk', label: 'Risk', icon: 'shield', disabled: true },
  { id: 'api', label: 'API & docs', icon: 'terminal', disabled: true },
];

function Clock() {
  const [t, setT] = useState<Date>(() => new Date());
  useEffect(() => {
    const i = setInterval(() => setT(new Date()), 1000);
    return () => clearInterval(i);
  }, []);
  const hh = String(t.getUTCHours()).padStart(2, '0');
  const mm = String(t.getUTCMinutes()).padStart(2, '0');
  const ss = String(t.getUTCSeconds()).padStart(2, '0');
  return (
    <span className="qe-top__clock">
      {hh}:{mm}:{ss} UTC
    </span>
  );
}

export interface AppShellProps {
  active?: string;
  onNav?: (id: string) => void;
  title?: ReactNode;
  actions?: ReactNode;
  children?: ReactNode;
  /** Signed-in user's email (from GET /api/me). */
  userEmail?: string;
  onSignOut?: () => void;
}

export function AppShell({
  active = 'backtest',
  onNav,
  title,
  actions,
  children,
  userEmail,
  onSignOut,
}: AppShellProps) {
  const searchRef = useRef<HTMLInputElement>(null);
  const initial = (userEmail?.trim()?.[0] ?? '?').toUpperCase();
  return (
    <div className="qe-app">
      <aside className="qe-side">
        <div className="qe-side__brand">
          <img src={logoLockup} alt="Quant Engine" />
        </div>
        <nav className="qe-side__nav" aria-label="Primary">
          {NAV.map((n, i) =>
            n.sec ? (
              <div key={`sec-${i}`} className="qe-side__sec">
                {n.sec}
              </div>
            ) : (
              <button
                key={n.id}
                type="button"
                className={`qe-navitem ${active === n.id ? 'qe-navitem--active' : ''}`}
                disabled={n.disabled}
                aria-current={active === n.id ? 'page' : undefined}
                onClick={() => !n.disabled && n.id && onNav?.(n.id)}
              >
                <span className="qe-navitem__ic">
                  <Icon name={n.icon ?? 'circle'} size={17} />
                </span>
                {n.label}
                {n.badge && (
                  <span className="qe-navitem__badge">
                    <Badge variant={active === n.id ? 'accent' : 'neutral'}>{n.badge}</Badge>
                  </span>
                )}
                {n.disabled && <span className="qe-navitem__soon">Soon</span>}
              </button>
            ),
          )}
        </nav>
        <div className="qe-side__foot">
          <div className="qe-foot__avatar" aria-hidden="true">
            {initial}
          </div>
          <div style={{ flex: 1, minWidth: 0 }}>
            <div className="n" title={userEmail}>
              {userEmail ?? 'Signed in'}
            </div>
            <div className="s">
              <span className="dot" />
              SESSION
            </div>
          </div>
          {onSignOut && (
            <Button variant="ghost" size="sm" onClick={onSignOut} aria-label="Sign out">
              Sign out
            </Button>
          )}
        </div>
      </aside>
      <div className="qe-main">
        <header className="qe-top">
          <span className="qe-top__title">{title}</span>
          <div className="qe-top__spacer" />
          <Clock />
          <div className="qe-top__search">
            <Icon name="search" size={14} />
            <input ref={searchRef} placeholder="Search…" aria-label="Search" />
          </div>
          {actions}
        </header>
        <main className="qe-content">{children}</main>
      </div>
    </div>
  );
}
