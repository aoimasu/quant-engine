import { useState } from 'react';
import { TrainingList } from './TrainingList';
import { NewTraining } from './NewTraining';
import { TrainingMonitor } from './TrainingMonitor';
import { NewFlow } from './NewFlow';
import { FlowMonitor } from './FlowMonitor';

type View =
  | { view: 'list' }
  | { view: 'new' }
  | { view: 'monitor'; runId: string }
  | { view: 'flow-new' }
  | { view: 'flow-monitor'; runId: string };

export interface TrainingAreaProps {
  /** Deep-link into the Backtests area's New-backtest flow for a sealed vintage id (cross-area nav). */
  onBacktestVintage: (vintage: string) => void;
  /** Deep-link into the Strategies area's Vintage Inspector for a sealed vintage id (QE-462 flow result). */
  onInspectVintage: (vintage: string) => void;
}

/**
 * The Research → Training area (QE-261) + the composite-flow stepped page (QE-462). A router-less view-state
 * machine (list / new / monitor / flow-new / flow-monitor), mirroring `BacktestsArea`. Creating a training
 * run opens the training monitor; "New flow run" opens the stepped {@link NewFlow} form which, on launch,
 * opens the {@link FlowMonitor}. On a training completion the monitor bubbles the sealed vintage up to
 * `onBacktestVintage`; on a flow completion the flow monitor deep-links into the Vintage Inspector via
 * `onInspectVintage`. "All training runs" returns to the list.
 */
export function TrainingArea({ onBacktestVintage, onInspectVintage }: TrainingAreaProps) {
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
    case 'flow-new':
      return (
        <NewFlow
          onCreated={(id) => setState({ view: 'flow-monitor', runId: id })}
          onCancel={() => setState({ view: 'list' })}
        />
      );
    case 'flow-monitor':
      return (
        <FlowMonitor
          runId={state.runId}
          onBack={() => setState({ view: 'list' })}
          onInspectVintage={onInspectVintage}
        />
      );
    case 'list':
    default:
      return (
        <TrainingList
          onOpen={(id) => setState({ view: 'monitor', runId: id })}
          onNew={() => setState({ view: 'new' })}
          onNewFlow={() => setState({ view: 'flow-new' })}
        />
      );
  }
}
