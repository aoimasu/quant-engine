import { useState } from 'react';
import { VintageBrowser } from './VintageBrowser';
import { VintageInspector } from './VintageInspector';
import { VintageLeaderboard } from './VintageLeaderboard';

type View =
  | { view: 'list' }
  | { view: 'inspect'; vintageId: string }
  | { view: 'leaderboard' };

export interface StrategiesAreaProps {
  /**
   * A pending deep-link: a sealed vintage id to open directly in the inspector (mirrors
   * `BacktestsArea`'s `initialVintage`). Set when another area links to the Inspector — e.g. the QE-462
   * composite-flow result. Absent ⇒ the area opens on the vintage browser.
   */
  initialVintage?: string;
}

/**
 * The Research → Strategies area (QE-457) — a router-less view-state machine (`list | inspect`), mirroring
 * {@link import('../evolve/EvolveArea').EvolveArea}. `list` is the {@link VintageBrowser} over the sealed
 * vintages; `inspect` is the read-only {@link VintageInspector} for one vintage. Opening a vintage from the
 * browser transitions to its inspector; the inspector's back button returns to the list. An `initialVintage`
 * deep-link (QE-462) opens straight into the inspector. The browser can also open the read-only QE-466
 * {@link VintageLeaderboard} (`leaderboard`); the leaderboard opens a row in the inspector or returns to the
 * list — it offers no promote/select action.
 */
export function StrategiesArea({ initialVintage }: StrategiesAreaProps = {}) {
  const [state, setState] = useState<View>(
    initialVintage ? { view: 'inspect', vintageId: initialVintage } : { view: 'list' },
  );

  switch (state.view) {
    case 'inspect':
      return <VintageInspector vintageId={state.vintageId} onBack={() => setState({ view: 'list' })} />;
    case 'leaderboard':
      return (
        <VintageLeaderboard
          onOpen={(vintageId) => setState({ view: 'inspect', vintageId })}
          onBack={() => setState({ view: 'list' })}
        />
      );
    case 'list':
    default:
      return (
        <VintageBrowser
          onOpen={(vintageId) => setState({ view: 'inspect', vintageId })}
          onLeaderboard={() => setState({ view: 'leaderboard' })}
        />
      );
  }
}
