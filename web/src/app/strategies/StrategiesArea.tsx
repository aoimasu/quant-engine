import { useState } from 'react';
import { VintageBrowser } from './VintageBrowser';
import { VintageInspector } from './VintageInspector';

type View = { view: 'list' } | { view: 'inspect'; vintageId: string };

/**
 * The Research → Strategies area (QE-457) — a router-less view-state machine (`list | inspect`), mirroring
 * {@link import('../evolve/EvolveArea').EvolveArea}. `list` is the {@link VintageBrowser} over the sealed
 * vintages; `inspect` is the read-only {@link VintageInspector} for one vintage. Opening a vintage from the
 * browser transitions to its inspector; the inspector's back button returns to the list.
 *
 * Replaces the former `App.tsx` "strategies browser is on the way" placeholder.
 */
export function StrategiesArea() {
  const [state, setState] = useState<View>({ view: 'list' });

  switch (state.view) {
    case 'inspect':
      return <VintageInspector vintageId={state.vintageId} onBack={() => setState({ view: 'list' })} />;
    case 'list':
    default:
      return <VintageBrowser onOpen={(vintageId) => setState({ view: 'inspect', vintageId })} />;
  }
}
