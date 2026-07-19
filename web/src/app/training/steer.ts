import { useState } from 'react';

/*
 * Shared QE-458/QE-459 **steering-control** logic — the single source of truth for the whitelisted steer
 * knobs, the compiled-floor constants, and the deflation-scaling projection, reused by both the `type:"train"`
 * form ({@link import('./NewTraining').NewTraining}) and the `type:"flow"` form
 * ({@link import('./NewFlow').NewFlow}). Extracting them here guarantees the train and flow paths can never
 * diverge on *what is steerable*: the same catalogue mirror, the same client floors, and the same disabled
 * guardrail chips (see {@link import('./steerCards')}) drive both. Client-side validation only *removes*
 * affordances — the server's `validate_train` / `validate_flow` stays the single enforcement point.
 */

/** Bar resolutions offered by the window pickers (mirrors the engine-supported set). */
export const RESOLUTIONS = ['1m', '5m', '1h', '4h', '1d'];

/**
 * The compiled indicator catalogue ids — a **documented client mirror** of `crates/signal/src/indicator/`
 * (`price.rs` + `flow.rs`, assembled by `catalogue()`), in catalogue order. There is no `/api/indicators`
 * endpoint, so — exactly like `NewCampaign`'s `WINDOW_LATTICE`/`CAP_*` mirrors — the picker hard-codes the
 * ids. Drift is **fail-closed**: the server's `validate_train`/`validate_flow` rejects any `indicator_subset`
 * id not in the live catalogue (`400`), so a stale mirror can only over-reject, never run a silently-wrong
 * search.
 */
export const CATALOGUE_INDICATORS = [
  'return_1',
  'sma_ratio_20',
  'ema_ratio_20',
  'roc_10',
  'rsi_14',
  'stoch_k_14',
  'williams_r_14',
  'cci_20',
  'mfi_14',
  'cmf_20',
  'aroon_osc_25',
  'macd_hist_12_26_9',
  'atr_pct_14',
  'bb_percent_20',
  'bb_bandwidth_20',
  'std_returns_20',
  'volume_ratio_20',
  'signed_volume_ratio_14',
  'funding_avg_8',
  'funding_state',
  'oi_roc_10',
  'premium_state',
] as const;

/** Server-mirrored floors (`crates/validation/src/steer.rs`) — client validation only *removes* affordances. */
export const MIN_WINDOWS = 4; // MIN_WFO_WINDOWS
export const MIN_FOLDS = 2; // MIN_WFO_FOLDS
export const HOLDOUT_FLOOR = 250; // HOLDOUT_FLOOR
export const EMBARGO_FLOOR = 1; // EMBARGO_FLOOR

/** MAP-Elites descriptor-space cell count (`DESCRIPTOR_SPACE_CELLS`) used for the *projected* trial basis N. */
export const DESCRIPTOR_CELLS = 45;
/** Indicative budget used for the projected-N figure when the budget fields are left blank. */
const INDICATIVE_GENERATIONS = 40;
const INDICATIVE_WINDOWS = 4;

/**
 * The **compiled gate floors** (`crates/validation/src/steer.rs`) rendered as fixed, disabled guardrail chips
 * (design §6.2 / QE-450 §13.4). These ride the G1 gate's own decision and are **not steerable** — no control
 * can set any of them; a request that so much as names one is a `400`. The chips exist to *teach the
 * boundary*, mirroring the `evolve` NewCampaign disabled caps chips.
 */
export const GUARDRAIL_FLOORS: { label: string; value: string }[] = [
  { label: 'Cost-stress ×', value: '≥ 1×' },
  { label: 'Turnover cap', value: '≤ 0.25' },
  { label: 'Capacity floor', value: '≥ $250k' },
  { label: 'DSR cutoff', value: '≥ 0.95' },
  { label: 'Uncensored PBO', value: '≤ 0.50' },
  { label: 'IC / FDR', value: '≥ 0.10' },
  { label: 'Holdout floor', value: `≥ ${HOLDOUT_FLOOR} bars` },
  { label: 'Embargo floor', value: `≥ ${EMBARGO_FLOOR} bar` },
];

/** Parse an optional positive-integer budget field: blank → undefined; invalid → the sentinel `NaN`. */
export function optInt(raw: string): number | undefined {
  const s = raw.trim();
  if (s === '') return undefined;
  const n = Number(s);
  return Number.isInteger(n) && n >= 0 ? n : NaN;
}

/**
 * The shared steer-control state + validation + param projection. Owns the whitelisted knobs
 * (`generations`/`population`/`holdout`/`embargo`/`windows`/`folds` + the indicator subset), the projected
 * distinct-trial `N`, the client-floor validation, and {@link SteerControls.applyTo} which writes **only**
 * the whitelisted fields onto an outgoing params object (a strict indicator subset, or omitted when the full
 * catalogue is selected). Seed + window live in the host form (they differ between train and flow).
 */
export interface SteerControls {
  generations: string;
  setGenerations: (v: string) => void;
  population: string;
  setPopulation: (v: string) => void;
  holdout: string;
  setHoldout: (v: string) => void;
  embargo: string;
  setEmbargo: (v: string) => void;
  windows: string;
  setWindows: (v: string) => void;
  folds: string;
  setFolds: (v: string) => void;
  indicators: string[];
  toggleIndicator: (id: string) => void;
  /** Number of indicators selected (feeds the projected-N figure). */
  subsetSize: number;
  /** Projected distinct-trial `N` (indicative, grows with subset/budget). */
  projectedN: number;
  projGen: number;
  projWin: number;
  budgetIsIndicative: boolean;
  /** Validate the whitelisted steer fields (NaN + compiled floors + ≥1 indicator). Message or `null`. */
  validate: () => string | null;
  /** Write the whitelisted steer fields onto `params` (strict subset; full catalogue ⇒ omit). */
  applyTo: (params: Record<string, unknown>) => void;
}

export function useSteerControls(): SteerControls {
  const [generations, setGenerations] = useState('');
  const [population, setPopulation] = useState('');
  const [holdout, setHoldout] = useState('');
  const [embargo, setEmbargo] = useState('');
  const [windows, setWindows] = useState('');
  const [folds, setFolds] = useState('');
  // Indicator subset — default to the whole catalogue (all selected ⇒ omit ⇒ engine-default full catalogue).
  const [indicators, setIndicators] = useState<string[]>([...CATALOGUE_INDICATORS]);

  const toggleIndicator = (id: string) =>
    setIndicators((prev) => (prev.includes(id) ? prev.filter((x) => x !== id) : [...prev, id]));

  // Projected distinct-trial N — a PURE client-side function of subset cardinality + budget, mirroring
  // QE-458's `effective_trials_with_features(cells, gens, windows, feature_space)` = 45·gens·windows·|subset|.
  const subsetSize = indicators.length;
  const genForN = optInt(generations);
  const winForN = optInt(windows);
  const projGen = genForN && !Number.isNaN(genForN) ? genForN : INDICATIVE_GENERATIONS;
  const projWin = winForN && !Number.isNaN(winForN) ? winForN : INDICATIVE_WINDOWS;
  const projectedN = DESCRIPTOR_CELLS * projGen * projWin * Math.max(1, subsetSize);
  const budgetIsIndicative = genForN === undefined || winForN === undefined;

  const validate = (): string | null => {
    if (indicators.length === 0) return 'Select at least one catalogue indicator for the search.';
    for (const [label, raw] of [
      ['Generations', generations],
      ['Population', population],
      ['Holdout', holdout],
      ['Embargo', embargo],
      ['Windows', windows],
      ['Folds', folds],
    ] as const) {
      if (Number.isNaN(optInt(raw))) return `${label} must be a non-negative whole number.`;
    }
    // Floors mirror the server (`validate_train`/`validate_flow`) so the client removes the affordance
    // before the POST.
    for (const [label, raw, floor] of [
      ['Windows', windows, MIN_WINDOWS],
      ['Folds', folds, MIN_FOLDS],
      ['Holdout', holdout, HOLDOUT_FLOOR],
      ['Embargo', embargo, EMBARGO_FLOOR],
    ] as const) {
      const n = optInt(raw);
      if (n !== undefined && !Number.isNaN(n) && n < floor) {
        return `${label} cannot be set below its compiled floor ${floor}.`;
      }
    }
    return null;
  };

  const applyTo = (params: Record<string, unknown>) => {
    const gen = optInt(generations);
    if (gen !== undefined) params.generations = gen;
    const pop = optInt(population);
    if (pop !== undefined) params.population = pop;
    const hold = optInt(holdout);
    if (hold !== undefined) params.holdout = hold;
    const emb = optInt(embargo);
    if (emb !== undefined) params.embargo = emb;
    const win = optInt(windows);
    if (win !== undefined) params.windows = win;
    const fld = optInt(folds);
    if (fld !== undefined) params.folds = fld;
    // Only send `indicator_subset` when it is a STRICT subset; the full catalogue ⇒ omit ⇒ engine default
    // (mirrors NewCampaign's window-lattice logic). Never send `evolved_pool`/`evolved_formulas` or any
    // blocklisted knob — no enabled control sets them (the server rejects them as a hard `400`).
    if (indicators.length !== CATALOGUE_INDICATORS.length) {
      params.indicator_subset = CATALOGUE_INDICATORS.filter((id) => indicators.includes(id));
    }
  };

  return {
    generations,
    setGenerations,
    population,
    setPopulation,
    holdout,
    setHoldout,
    embargo,
    setEmbargo,
    windows,
    setWindows,
    folds,
    setFolds,
    indicators,
    toggleIndicator,
    subsetSize,
    projectedN,
    projGen,
    projWin,
    budgetIsIndicative,
    validate,
    applyTo,
  };
}
