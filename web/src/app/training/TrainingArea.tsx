import { useState } from 'react';
import { TrainingList } from './TrainingList';
import { NewTraining } from './NewTraining';
import { TrainingMonitor } from './TrainingMonitor';

type View = { view: 'list' } | { view: 'new' } | { view: 'monitor'; runId: string };

export interface TrainingAreaProps {
  /** Deep-link into the Backtests area's New-backtest flow for a sealed vintage id (cross-area nav). */
  onBacktestVintage: (vintage: string) => void;
}

/**
 * The Research → Training area (QE-261). A router-less view-state machine (list / new / monitor),
 * mirroring `BacktestsArea`. Creating a run opens the live monitor; "All training runs" returns to the
 * list. On completion the monitor bubbles the sealed vintage up to `onBacktestVintage` so the app can
 * deep-link into the QE-259 backtest flow.
 */
export function TrainingArea({ onBacktestVintage }: TrainingAreaProps) {
  const [state, setState] = useState<View>({ view: 'list' });

  switch (state.view) {
    case 'new':
      return (
        <NewTraining
          onCreated={(id) => setState({ view: 'monitor', runId: id })}
          onCancel={() => setState({ view: 'list' })}
        />
      );
    case 'monitor':
      return (
        <TrainingMonitor
          runId={state.runId}
          onBack={() => setState({ view: 'list' })}
          onBacktestVintage={onBacktestVintage}
        />
      );
    case 'list':
    default:
      return (
        <TrainingList
          onOpen={(id) => setState({ view: 'monitor', runId: id })}
          onNew={() => setState({ view: 'new' })}
        />
      );
  }
}
