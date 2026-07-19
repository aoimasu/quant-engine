import { useState } from 'react';
import { MarketData } from '../MarketData';
import { NewIngest } from './NewIngest';
import { IngestMonitor } from './IngestMonitor';

type View = { view: 'coverage' } | { view: 'new' } | { view: 'monitor'; runId: string };

/**
 * The Research → Market-data area (QE-465). A router-less view-state machine
 * (`coverage | new | monitor`), mirroring {@link EvolveArea}/{@link TrainingArea}. `coverage` is the
 * read-only coverage table (now with the provenance column) + an "Ingest data" entry point; `new` is the
 * ingest-trigger form; launching an ingest opens its live standard run monitor (progress + cancel).
 */
export function MarketDataArea() {
  const [state, setState] = useState<View>({ view: 'coverage' });

  switch (state.view) {
    case 'new':
      return (
        <NewIngest
          onCreated={(id) => setState({ view: 'monitor', runId: id })}
          onCancel={() => setState({ view: 'coverage' })}
        />
      );
    case 'monitor':
      return <IngestMonitor runId={state.runId} onBack={() => setState({ view: 'coverage' })} />;
    case 'coverage':
    default:
      return <MarketData onNewIngest={() => setState({ view: 'new' })} />;
  }
}
