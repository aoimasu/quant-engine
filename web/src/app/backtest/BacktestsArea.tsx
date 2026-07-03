import { useState } from 'react';
import { BacktestsList } from './BacktestsList';
import { NewBacktest } from './NewBacktest';
import { BacktestResult } from './BacktestResult';

type View = { view: 'list' } | { view: 'new' } | { view: 'result'; runId: string };

/**
 * The Research → Backtests area. Owns a small view state (list / new / result) so the four v1 backtest
 * screens compose without a router dependency (decision D-routing). Creating or opening a run moves to
 * the result screen; "All backtests" returns to the list.
 */
export function BacktestsArea() {
  const [state, setState] = useState<View>({ view: 'list' });

  switch (state.view) {
    case 'new':
      return (
        <NewBacktest
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
