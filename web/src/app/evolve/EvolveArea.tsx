import { useState } from 'react';
import { CampaignList } from './CampaignList';
import { NewCampaign } from './NewCampaign';
import { CampaignMonitor } from './CampaignMonitor';
import { PoolBrowser } from './PoolBrowser';
import { PoolReview } from './PoolReview';

type View =
  | { view: 'list' }
  | { view: 'new' }
  | { view: 'monitor'; runId: string }
  | { view: 'pool' }
  | { view: 'review'; poolId: string };

/**
 * The Research → Indicator-evolution area (QE-453). A router-less view-state machine
 * (`list | new | monitor | pool | review`), mirroring {@link TrainingArea} verbatim. `list` is the
 * evolve-campaign runs list; `pool` is the PoolBrowser over frozen pools; `review` is the governance
 * gate (PoolReview) for one pool. Launching a campaign opens its live monitor; browsing a pool opens the
 * review gate.
 */
export function EvolveArea() {
  const [state, setState] = useState<View>({ view: 'list' });

  switch (state.view) {
    case 'new':
      return (
        <NewCampaign
          onCreated={(id) => setState({ view: 'monitor', runId: id })}
          onCancel={() => setState({ view: 'list' })}
        />
      );
    case 'monitor':
      return <CampaignMonitor runId={state.runId} onBack={() => setState({ view: 'list' })} />;
    case 'pool':
      return (
        <PoolBrowser
          onBack={() => setState({ view: 'list' })}
          onOpen={(poolId) => setState({ view: 'review', poolId })}
        />
      );
    case 'review':
      return <PoolReview poolId={state.poolId} onBack={() => setState({ view: 'pool' })} />;
    case 'list':
    default:
      return (
        <CampaignList
          onOpen={(id) => setState({ view: 'monitor', runId: id })}
          onNew={() => setState({ view: 'new' })}
          onBrowsePools={() => setState({ view: 'pool' })}
        />
      );
  }
}
