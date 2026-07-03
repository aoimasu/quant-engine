import { useEffect, useState } from 'react';
import { AppShell } from '../design';
import { Login } from './Login';
import { Placeholder } from './Placeholder';
import { fetchMe, logout, detectRejection, type Me } from '../api/session';

type Status = 'loading' | 'unauth' | 'auth';

/** Research destinations active in v1 (spec §7.2). Screens land in QE-259. */
const RESEARCH_SCREENS: Record<string, { title: string; icon: string; description: string }> = {
  strategies: {
    title: 'Strategies',
    icon: 'git-branch',
    description: 'Sealed vintages and the evolved genomes within them. The strategies browser is on the way.',
  },
  backtest: {
    title: 'Backtests',
    icon: 'flask-conical',
    description:
      'Trigger a backtest of a sealed vintage over a window, watch progress, and review the full result.',
  },
  data: {
    title: 'Market data',
    icon: 'database',
    description: 'Read-only coverage of the local market-data store — symbols and the date ranges present.',
  },
};

export function App() {
  const [status, setStatus] = useState<Status>('loading');
  const [me, setMe] = useState<Me | null>(null);
  const [rejected] = useState<boolean>(() => detectRejection());
  const [active, setActive] = useState<string>('backtest');

  useEffect(() => {
    let cancelled = false;
    fetchMe()
      .then((user) => {
        if (cancelled) return;
        if (user) {
          setMe(user);
          setStatus('auth');
        } else {
          setStatus('unauth');
        }
      })
      .catch(() => {
        if (!cancelled) setStatus('unauth');
      });
    return () => {
      cancelled = true;
    };
  }, []);

  if (status === 'loading') {
    return (
      <div
        style={{
          minHeight: '100vh',
          display: 'grid',
          placeItems: 'center',
          color: 'var(--text-muted)',
          fontFamily: 'var(--font-mono)',
          fontSize: 12,
          letterSpacing: '0.08em',
          textTransform: 'uppercase',
        }}
        role="status"
        aria-live="polite"
      >
        Loading…
      </div>
    );
  }

  if (status === 'unauth') {
    return <Login rejected={rejected} />;
  }

  const screen = RESEARCH_SCREENS[active] ?? RESEARCH_SCREENS.backtest;
  return (
    <AppShell
      active={active}
      onNav={setActive}
      title={screen.title}
      userEmail={me?.email}
      onSignOut={() => {
        void logout();
      }}
    >
      <Placeholder
        icon={screen.icon}
        title={screen.title}
        description={screen.description}
        ticket="QE-259"
      />
    </AppShell>
  );
}
