import { useEffect, useState } from 'react';
import { AppShell } from '../design';
import { Login } from './Login';
import { BacktestsArea } from './backtest/BacktestsArea';
import { TrainingArea } from './training/TrainingArea';
import { EvolveArea } from './evolve/EvolveArea';
import { StrategiesArea } from './strategies/StrategiesArea';
import { MarketData } from './MarketData';
import { fetchMe, logout, detectRejection, type Me } from '../api/session';
import { onUnauthorized } from '../api/authEvents';
import { ErrorBoundary } from './ErrorBoundary';

type Status = 'loading' | 'unauth' | 'auth';

/** Human titles for each Research destination (spec §7.2, + QE-261 Training). */
const SCREEN_TITLES: Record<string, string> = {
  strategies: 'Strategies',
  training: 'Training',
  evolve: 'Indicator evolution',
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

  // QE-409: any 401 seen by the API client mid-session (an expired/cleared cookie) flips the shell
  // back to the unauth state and remounts Login — without a full-page reload. Subscribe once.
  useEffect(() => {
    return onUnauthorized(() => {
      setMe(null);
      setStatus('unauth');
    });
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
  // QE-424: a top-level error boundary around the authed shell — a render throw in any screen shows a
  // recoverable fallback instead of blanking the whole SPA. `resetKeys` clears a stale error on an
  // auth/session change (a new signed-in identity). The unauth/Login shell is returned above, so it is
  // never wrapped and can't be blanked by a screen throw.
  return (
    <ErrorBoundary resetKeys={[me?.email]}>
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
          <StrategiesArea />
        ) : active === 'training' ? (
          <TrainingArea onBacktestVintage={openBacktestForVintage} />
        ) : active === 'evolve' ? (
          <EvolveArea />
        ) : (
          <BacktestsArea initialVintage={backtestVintage} />
        )}
      </AppShell>
    </ErrorBoundary>
  );
}
