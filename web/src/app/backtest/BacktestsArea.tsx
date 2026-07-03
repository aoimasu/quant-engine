import { useState } from 'react';
import { BacktestsList } from './BacktestsList';
import { NewBacktest } from './NewBacktest';
import { BacktestResult } from './BacktestResult';

type View = { view: 'list' } | { view: 'new' } | { view: 'result'; runId: string };

export interface BacktestsAreaProps {
  /** A sealed vintage to deep-link into the New-backtest flow with (QE-261 cross-area nav). When set,
   *  the area opens on the New-backtest form with this vintage preselected. */
  initialVintage?: string;
}

/**
 * The Research → Backtests area. Owns a small view state (list / new / result) so the four v1 backtest
 * screens compose without a router dependency (decision D-routing). Creating or opening a run moves to
 * the result screen; "All backtests" returns to the list. When mounted with an `initialVintage`
 * (a QE-261 training → backtest deep-link), it opens directly on the New-backtest form.
 */
export function BacktestsArea({ initialVintage }: BacktestsAreaProps = {}) {
  const [state, setState] = useState<View>(initialVintage ? { view: 'new' } : { view: 'list' });

  switch (state.view) {
    case 'new':
      return (
        <NewBacktest
          initialVintage={initialVintage}
          onCreated={(id) => setState({ view: 'result', runId: id })}
          onCancel={() => setState({ view: 'list' })}
        />
      );
    case 'result':
      return (
        <BacktestResult
          runId={state.runId}
          onBack={() => setState({ view: 'list' })}
          onReRun={(id) => setState({ view: 'result', runId: id })}
        />
      );
    case 'list':
    default:
      return (
        <BacktestsList
          onOpen={(id) => setState({ view: 'result', runId: id })}
          onNew={() => setState({ view: 'new' })}
        />
      );
  }
}
