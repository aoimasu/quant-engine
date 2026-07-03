import { useEffect, useState } from 'react';
import { AppShell } from '../design';
import { Login } from './Login';
import { Placeholder } from './Placeholder';
import { BacktestsArea } from './backtest/BacktestsArea';
import { MarketData } from './MarketData';
import { fetchMe, logout, detectRejection, type Me } from '../api/session';

type Status = 'loading' | 'unauth' | 'auth';

/** Human titles for each Research destination (spec §7.2). */
const SCREEN_TITLES: Record<string, string> = {
  strategies: 'Strategies',
  backtest: 'Backtests',
  data: 'Market data',
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

  const title = SCREEN_TITLES[active] ?? SCREEN_TITLES.backtest;
  return (
    <AppShell
      active={active}
      onNav={setActive}
      title={title}
      userEmail={me?.email}
      onSignOut={() => {
        void logout();
      }}
    >
      {active === 'data' ? (
        <MarketData />
      ) : active === 'strategies' ? (
        <Placeholder
          icon="git-branch"
          title="Strategies"
          description="Sealed vintages and the evolved genomes within them. The strategies browser is on the way."
          ticket="a later ticket"
        />
      ) : (
        <BacktestsArea />
      )}
    </AppShell>
  );
}
