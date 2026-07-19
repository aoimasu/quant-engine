import { useState } from 'react';
import { Button, Callout, Card, Icon, Input, Select } from '../../design';
import { injectCss } from '../../design/injectCss';
import { ApiError, createTrainRun, type TrainParams } from '../../api/runs';

const CSS = `
.qe-nt { max-width: 880px; margin: 0 auto; padding: 24px; display: flex; flex-direction: column; gap: 16px; }
.qe-nt__hd h2 { font-family: var(--font-display); font-size: var(--fs-lg); font-weight: 600; }
.qe-nt__grid { display: grid; grid-template-columns: 1fr 1fr; gap: 14px; }
.qe-nt__row { display: flex; flex-direction: column; gap: 6px; }
.qe-nt__lbl { font-family: var(--font-sans); font-size: var(--fs-sm); font-weight: var(--fw-medium); color: var(--text-secondary); }
.qe-nt__hint { font-size: var(--fs-caption); color: var(--text-muted); margin-top: 2px; }
.qe-nt__actions { display: flex; justify-content: flex-end; gap: 10px; }
.qe-nt__chips { display: flex; flex-wrap: wrap; gap: 6px; }
.qe-nt__chip { font: 500 11px var(--font-mono); padding: 5px 10px; border-radius: var(--radius-pill); border: 1px solid var(--border-default); color: var(--text-tertiary); background: transparent; cursor: pointer; }
.qe-nt__chip[aria-pressed="true"] { background: var(--accent-fill-soft); border-color: var(--violet-400); color: var(--violet-200); }
/* Fixed, compiled-floor guardrail chips — deliberately non-interactive (mirrors the evolve caps chips). */
.qe-nt__chip--fixed { cursor: not-allowed; opacity: 0.85; border-style: dashed; color: var(--text-muted); }
.qe-nt__chip--fixed:disabled { cursor: not-allowed; }
.qe-nt__feedback { display: flex; flex-direction: column; gap: 8px; }
.qe-nt__stat { display: flex; align-items: baseline; gap: 8px; }
.qe-nt__statN { font: 600 22px var(--font-mono); color: var(--violet-200); }
.qe-nt__statLbl { font-size: var(--fs-sm); color: var(--text-secondary); }
`;

injectCss('qe-nt-css', CSS);

const RESOLUTIONS = ['1m', '5m', '1h', '4h', '1d'];

/**
 * The compiled indicator catalogue ids — a **documented client mirror** of `crates/signal/src/indicator/`
 * (`price.rs` + `flow.rs`, assembled by `catalogue()`), in catalogue order. There is no `/api/indicators`
 * endpoint, so — exactly like `NewCampaign`'s `WINDOW_LATTICE`/`CAP_*` mirrors — the picker hard-codes the
 * ids. Drift is **fail-closed**: the server's `validate_train` rejects any `indicator_subset` id not in the
 * live catalogue (`400`), so a stale mirror can only over-reject, never run a silently-wrong search.
 */
const CATALOGUE_INDICATORS = [
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
const MIN_WINDOWS = 4; // MIN_WFO_WINDOWS
const MIN_FOLDS = 2; // MIN_WFO_FOLDS
const HOLDOUT_FLOOR = 250; // HOLDOUT_FLOOR
const EMBARGO_FLOOR = 1; // EMBARGO_FLOOR

/** MAP-Elites descriptor-space cell count (`DESCRIPTOR_SPACE_CELLS`) used for the *projected* trial basis N. */
const DESCRIPTOR_CELLS = 45;
/** Indicative budget used for the projected-N figure when the budget fields are left blank. */
const INDICATIVE_GENERATIONS = 40;
const INDICATIVE_WINDOWS = 4;

/**
 * The **compiled gate floors** (`crates/validation/src/steer.rs`) rendered as fixed, disabled guardrail chips
 * (design §6.2 / QE-450 §13.4). These ride the G1 gate's own decision and are **not steerable** — the form
 * has no control that can set any of them; a request that so much as names one is a `400`. The chips exist to
 * *teach the boundary*, mirroring the `evolve` NewCampaign disabled caps chips.
 */
const GUARDRAIL_FLOORS: { label: string; value: string }[] = [
  { label: 'Cost-stress ×', value: '≥ 1×' },
  { label: 'Turnover cap', value: '≤ 0.25' },
  { label: 'Capacity floor', value: '≥ $250k' },
  { label: 'DSR cutoff', value: '≥ 0.95' },
  { label: 'Uncensored PBO', value: '≤ 0.50' },
  { label: 'IC / FDR', value: '≥ 0.10' },
  { label: 'Holdout floor', value: `≥ ${HOLDOUT_FLOOR} bars` },
  { label: 'Embargo floor', value: `≥ ${EMBARGO_FLOOR} bar` },
];

export interface NewTrainingProps {
  onCreated: (id: string) => void;
  onCancel: () => void;
}

/** Parse an optional positive-integer budget field: blank → undefined; invalid → the sentinel `NaN`. */
function optInt(raw: string): number | undefined {
  const s = raw.trim();
  if (s === '') return undefined;
  const n = Number(s);
  return Number.isInteger(n) && n >= 0 ? n : NaN;
}

/**
 * New training run (trigger) form — window/resolution/seed + the QE-458 **whitelisted steer knobs** (QE-459):
 * an indicator picker (catalogue subset), search budget (generations/population), and windows/folds — all
 * submitted as the whitelisted `TrainParams` fields → `POST /api/runs {type:"train"}`. The **blocklisted**
 * gate-decision thresholds render as fixed disabled guardrail chips (there is no control that can set them),
 * and evolved-pool inclusion is a disabled "not yet supported" affordance (QE-458 rejects it server-side), so
 * the form can never issue an always-`400` request. A projected distinct-trial `N` teaches that widening the
 * subset / raising the budget *raises* the deflation bar — steering buys no free pass; archive coverage is
 * honestly framed as recorded **after** the run (the Vintage Inspector surfaces it), never fabricated pre-run.
 *
 * The training universe and store are resolved server-side from config (no `--universe` flag on `qe train`),
 * so this form does not select instruments. Client-side validation only *removes* affordances; the server's
 * `validate_train` stays the single enforcement point, and a server `400` is surfaced inline.
 */
export function NewTraining({ onCreated, onCancel }: NewTrainingProps) {
  const [start, setStart] = useState('');
  const [end, setEnd] = useState('');
  const [resolution, setResolution] = useState('1h');
  const [seed, setSeed] = useState('');
  const [generations, setGenerations] = useState('');
  const [population, setPopulation] = useState('');
  const [holdout, setHoldout] = useState('');
  const [embargo, setEmbargo] = useState('');
  const [windows, setWindows] = useState('');
  const [folds, setFolds] = useState('');
  // Indicator subset — default to the whole catalogue (all selected ⇒ omit ⇒ engine-default full catalogue).
  const [indicators, setIndicators] = useState<string[]>([...CATALOGUE_INDICATORS]);

  const [fieldError, setFieldError] = useState<string | null>(null);
  const [serverError, setServerError] = useState<string | null>(null);
  const [submitting, setSubmitting] = useState(false);

  const toggleIndicator = (id: string) =>
    setIndicators((prev) => (prev.includes(id) ? prev.filter((x) => x !== id) : [...prev, id]));

  // Projected distinct-trial N — a PURE client-side function of subset cardinality + budget, mirroring
  // QE-458's `effective_trials_with_features(cells, gens, windows, feature_space)` = 45·gens·windows·|subset|.
  // Indicative (labelled as such); it grows monotonically as the subset widens or the budget rises, so the
  // operator sees the deflation bar climb with scope. Archive coverage is a runtime OUTPUT — not previewable.
  const subsetSize = indicators.length;
  const genForN = optInt(generations);
  const winForN = optInt(windows);
  const projGen = genForN && !Number.isNaN(genForN) ? genForN : INDICATIVE_GENERATIONS;
  const projWin = winForN && !Number.isNaN(winForN) ? winForN : INDICATIVE_WINDOWS;
  const projectedN = DESCRIPTOR_CELLS * projGen * projWin * Math.max(1, subsetSize);
  const budgetIsIndicative = genForN === undefined || winForN === undefined;

  const validate = (): string | null => {
    if (!start) return 'Choose a training-window start date.';
    if (!end) return 'Choose a training-window end date.';
    if (start >= end) return 'The window start must be before the end.';
    if (!resolution) return 'Choose a bar resolution.';
    if (indicators.length === 0) return 'Select at least one catalogue indicator for the search.';
    for (const [label, raw] of [
      ['Seed', seed],
      ['Generations', generations],
      ['Population', population],
      ['Holdout', holdout],
      ['Embargo', embargo],
      ['Windows', windows],
      ['Folds', folds],
    ] as const) {
      if (Number.isNaN(optInt(raw))) return `${label} must be a non-negative whole number.`;
    }
    // Floors mirror the server (`validate_train`) so the client removes the affordance before the POST.
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

  const submit = async () => {
    setServerError(null);
    const err = validate();
    if (err) {
      setFieldError(err);
      return;
    }
    setFieldError(null);
    setSubmitting(true);
    const params: TrainParams = { start, end, resolution };
    const seedN = optInt(seed);
    if (seedN !== undefined) params.seed = seedN;
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
    // (mirrors NewCampaign's window-lattice logic). Never send `evolved_pool`/`evolved_formulas` — the form
    // has no enabled control for them (QE-458 rejects them as not-yet-supported).
    if (indicators.length !== CATALOGUE_INDICATORS.length) {
      params.indicator_subset = CATALOGUE_INDICATORS.filter((id) => indicators.includes(id));
    }
    try {
      const id = await createTrainRun(params);
      onCreated(id);
    } catch (e) {
      setServerError(e instanceof ApiError ? e.message : 'Failed to start the training run.');
      setSubmitting(false);
    }
  };

  return (
    <div className="qe-nt">
      <div className="qe-nt__hd">
        <Button variant="ghost" size="sm" onClick={onCancel} iconLeft={<Icon name="arrow-left" size={15} />}>
          All training runs
        </Button>
        <h2 style={{ marginTop: 8 }}>New training run</h2>
      </div>

      <Card title="Training window">
        <div style={{ display: 'flex', flexDirection: 'column', gap: 16 }}>
          <div className="qe-nt__grid">
            <Input label="Start" type="date" value={start} onChange={(e) => setStart(e.target.value)} />
            <Input label="End" type="date" value={end} onChange={(e) => setEnd(e.target.value)} />
            <div className="qe-nt__row">
              <label className="qe-nt__lbl" htmlFor="qe-nt-res">
                Resolution
              </label>
              <Select
                id="qe-nt-res"
                aria-label="Resolution"
                value={resolution}
                onChange={(e) => setResolution(e.target.value)}
                options={RESOLUTIONS}
              />
            </div>
            <Input
              label="Seed"
              mono
              placeholder="config default"
              value={seed}
              onChange={(e) => setSeed(e.target.value)}
            />
          </div>
          <span className="qe-nt__hint">
            The training universe and market-data store are resolved from server config.
          </span>
        </div>
      </Card>

      <Card title="Indicator subset (steer)">
        <div style={{ display: 'flex', flexDirection: 'column', gap: 16 }}>
          <div className="qe-nt__row">
            <span className="qe-nt__lbl">Catalogue indicators</span>
            <div className="qe-nt__chips" role="group" aria-label="Catalogue indicators">
              {CATALOGUE_INDICATORS.map((id) => (
                <button
                  key={id}
                  type="button"
                  className="qe-nt__chip"
                  aria-pressed={indicators.includes(id)}
                  onClick={() => toggleIndicator(id)}
                >
                  {id}
                </button>
              ))}
            </div>
            <span className="qe-nt__hint">
              Which catalogue indicators the search may reference. The full catalogue defers to the engine
              default; a narrower subset is a <em>smaller</em> hypothesis space. A wider subset is counted in
              the deflation basis <code>N</code> below — steering more raises the bar, it does not relax it.
            </span>
          </div>
          <div className="qe-nt__row">
            <span className="qe-nt__lbl">Evolved-pool formulas</span>
            <div className="qe-nt__chips" role="group" aria-label="Evolved-pool formulas">
              <button
                type="button"
                className="qe-nt__chip qe-nt__chip--fixed"
                disabled
                aria-disabled="true"
              >
                not yet supported
              </button>
            </div>
            <span className="qe-nt__hint">
              Including already-sealed evolved-pool formulas as indicators is <strong>not yet supported on the
              live train search</strong> (a QE-402-safe feature-space extension is a follow-up); the server
              rejects it. This control is disabled so the form never issues a request the engine would reject.
            </span>
          </div>
        </div>
      </Card>

      <Card title="Search budget & windows (steer)">
        <div style={{ display: 'flex', flexDirection: 'column', gap: 16 }}>
          <div className="qe-nt__grid">
            <Input
              label="Generations"
              mono
              placeholder="default"
              value={generations}
              onChange={(e) => setGenerations(e.target.value)}
            />
            <Input
              label="Population"
              mono
              placeholder="default"
              value={population}
              onChange={(e) => setPopulation(e.target.value)}
            />
            <Input
              label={`WFO windows (≥ ${MIN_WINDOWS})`}
              mono
              placeholder="default"
              value={windows}
              onChange={(e) => setWindows(e.target.value)}
            />
            <Input
              label={`CV folds (≥ ${MIN_FOLDS})`}
              mono
              placeholder="default"
              value={folds}
              onChange={(e) => setFolds(e.target.value)}
            />
            <Input
              label={`Holdout bars (≥ ${HOLDOUT_FLOOR})`}
              mono
              placeholder="default"
              value={holdout}
              onChange={(e) => setHoldout(e.target.value)}
            />
            <Input
              label={`Embargo bars (≥ ${EMBARGO_FLOOR})`}
              mono
              placeholder="default"
              value={embargo}
              onChange={(e) => setEmbargo(e.target.value)}
            />
          </div>
          <span className="qe-nt__hint">
            Leave blank to use the engine defaults (a small, fast fixture budget). More generations / longer
            windows raise <code>T&#8202;eff</code> and make the search <em>harder</em> to pass, never easier.
          </span>
        </div>
      </Card>

      <Card title="Deflation-scaling feedback">
        <div className="qe-nt__feedback">
          <div className="qe-nt__stat">
            <span className="qe-nt__statN" aria-label="Projected distinct-trial N">
              {projectedN.toLocaleString()}
            </span>
            <span className="qe-nt__statLbl">
              projected distinct-trial <code>N</code> — the deflation bar rises with scope
            </span>
          </div>
          <span className="qe-nt__hint">
            Projected from the selected subset ({subsetSize} indicator{subsetSize === 1 ? '' : 's'}) ×{' '}
            {budgetIsIndicative ? 'the indicative default budget' : 'your budget'} (
            <code>
              {DESCRIPTOR_CELLS}·{projGen} gen·{projWin} win·{subsetSize}
            </code>
            ). Indicative, not the sealed basis: widening the subset or raising the budget only <em>raises</em>{' '}
            <code>N</code> and the <code>E[max&nbsp;SR]</code> bar — steering buys no free pass.
          </span>
          <span className="qe-nt__hint">
            <strong>Archive coverage</strong> (occupied niches, pre/post steer) is a runtime search output —
            it is <em>recorded after the run</em> and surfaced in the Vintage Inspector, not previewed here. A
            steer that collapses the quality-diversity archive is caught by the engine, never hidden.
          </span>
        </div>
      </Card>

      <Card title="Compiled floors (not steerable)">
        <div className="qe-nt__row">
          <div className="qe-nt__chips" role="group" aria-label="Compiled gate floors">
            {GUARDRAIL_FLOORS.map((g) => (
              <button
                key={g.label}
                type="button"
                className="qe-nt__chip qe-nt__chip--fixed"
                disabled
                aria-disabled="true"
              >
                {g.label} {g.value}
              </button>
            ))}
          </div>
          <span className="qe-nt__hint">
            These thresholds ride the G1 gate&rsquo;s own decision and are <strong>fixed compiled floors</strong>{' '}
            the research path cannot relax. There is no form control that can set them — a request that tries is
            rejected server-side. Steering changes <em>what</em> the search explores and <em>how hard</em>,
            never <em>what passes</em>.
          </span>
        </div>
      </Card>

      {fieldError && (
        <Callout variant="warn" title="Check the form">
          {fieldError}
        </Callout>
      )}
      {serverError && (
        <Callout variant="danger" title="The server rejected the request">
          {serverError}
        </Callout>
      )}

      <div className="qe-nt__actions">
        <Button variant="secondary" onClick={onCancel} disabled={submitting}>
          Cancel
        </Button>
        <Button
          variant="primary"
          loading={submitting}
          onClick={submit}
          iconLeft={<Icon name="play" size={15} />}
        >
          Start training
        </Button>
      </div>
    </div>
  );
}
