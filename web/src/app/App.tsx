import { useEffect, useState } from 'react';
import { AppShell } from '../design';
import { Login } from './Login';
import { Placeholder } from './Placeholder';
import { BacktestsArea } from './backtest/BacktestsArea';
import { TrainingArea } from './training/TrainingArea';
import { MarketData } from './MarketData';
import { fetchMe, logout, detectRejection, type Me } from '../api/session';

type Status = 'loading' | 'unauth' | 'auth';

/** Human titles for each Research destination (spec §7.2, + QE-261 Training). */
const SCREEN_TITLES: Record<string, string> = {
  strategies: 'Strategies',
  training: 'Training',
  backtest: 'Backtests',
  data: 'Market data',
};

export function App() {
  const [status, setStatus] = useState<Status>('loading');
  const [me, setMe] = useState<Me | null>(null);
  const [rejected] = useState<boolean>(() => detectRejection());
  const [active, setActive] = useState<string>('backtest');
  // A pending cross-area deep-link: a sealed vintage to preselect in the New-backtest flow (QE-261).
  // Set when the training monitor's "Backtest this vintage" is clicked; consumed when Backtests mounts.
  const [backtestVintage, setBacktestVintage] = useState<string | undefined>(undefined);

  // Manual nav clears any pending deep-link seed so a later plain visit to Backtests isn't re-seeded.
  const navigate = (id: string) => {
    setBacktestVintage(undefined);
    setActive(id);
  };

  // Programmatic deep-link from Training → Backtests, carrying the sealed vintage id.
  const openBacktestForVintage = (vintage: string) => {
    setBacktestVintage(vintage);
    setActive('backtest');
  };

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
      onNav={navigate}
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
      ) : active === 'training' ? (
        <TrainingArea onBacktestVintage={openBacktestForVintage} />
      ) : (
        <BacktestsArea initialVintage={backtestVintage} />
      )}
    </AppShell>
  );
}
