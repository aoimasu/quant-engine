import { Card, Input } from '../../design';
import { injectCss } from '../../design/injectCss';
import {
  CATALOGUE_INDICATORS,
  DESCRIPTOR_CELLS,
  EMBARGO_FLOOR,
  GUARDRAIL_FLOORS,
  HOLDOUT_FLOOR,
  MIN_FOLDS,
  MIN_WINDOWS,
  type SteerControls,
} from './steer';

/*
 * The presentational QE-459 **steering-control cards** — the indicator picker, the search-budget/windows
 * inputs, the deflation-scaling feedback, and the disabled compiled-floor guardrail chips. Shared by both the
 * `type:"train"` and `type:"flow"` forms (state lives in {@link useSteerControls}); they render the identical
 * accessible controls so the two paths can never diverge on what is steerable.
 */

const CSS = `
.qe-steer__grid { display: grid; grid-template-columns: 1fr 1fr; gap: 14px; }
.qe-steer__row { display: flex; flex-direction: column; gap: 6px; }
.qe-steer__lbl { font-family: var(--font-sans); font-size: var(--fs-sm); font-weight: var(--fw-medium); color: var(--text-secondary); }
.qe-steer__hint { font-size: var(--fs-caption); color: var(--text-muted); margin-top: 2px; }
.qe-steer__chips { display: flex; flex-wrap: wrap; gap: 6px; }
.qe-steer__chip { font: 500 11px var(--font-mono); padding: 5px 10px; border-radius: var(--radius-pill); border: 1px solid var(--border-default); color: var(--text-tertiary); background: transparent; cursor: pointer; }
.qe-steer__chip[aria-pressed="true"] { background: var(--accent-fill-soft); border-color: var(--violet-400); color: var(--violet-200); }
/* Fixed, compiled-floor guardrail chips — deliberately non-interactive (mirrors the evolve caps chips). */
.qe-steer__chip--fixed { cursor: not-allowed; opacity: 0.85; border-style: dashed; color: var(--text-muted); }
.qe-steer__chip--fixed:disabled { cursor: not-allowed; }
.qe-steer__feedback { display: flex; flex-direction: column; gap: 8px; }
.qe-steer__stat { display: flex; align-items: baseline; gap: 8px; }
.qe-steer__statN { font: 600 22px var(--font-mono); color: var(--violet-200); }
.qe-steer__statLbl { font-size: var(--fs-sm); color: var(--text-secondary); }
`;

injectCss('qe-steer-css', CSS);

/** The catalogue indicator picker + the disabled "evolved-pool not yet supported" affordance (QE-458/459). */
export function IndicatorSubsetCard({ steer }: { steer: SteerControls }) {
  return (
    <Card title="Indicator subset (steer)">
      <div style={{ display: 'flex', flexDirection: 'column', gap: 16 }}>
        <div className="qe-steer__row">
          <span className="qe-steer__lbl">Catalogue indicators</span>
          <div className="qe-steer__chips" role="group" aria-label="Catalogue indicators">
            {CATALOGUE_INDICATORS.map((id) => (
              <button
                key={id}
                type="button"
                className="qe-steer__chip"
                aria-pressed={steer.indicators.includes(id)}
                onClick={() => steer.toggleIndicator(id)}
              >
                {id}
              </button>
            ))}
          </div>
          <span className="qe-steer__hint">
            Which catalogue indicators the search may reference. The full catalogue defers to the engine
            default; a narrower subset is a <em>smaller</em> hypothesis space. A wider subset is counted in the
            deflation basis <code>N</code> below — steering more raises the bar, it does not relax it.
          </span>
        </div>
        <div className="qe-steer__row">
          <span className="qe-steer__lbl">Evolved-pool formulas</span>
          <div className="qe-steer__chips" role="group" aria-label="Evolved-pool formulas">
            <button type="button" className="qe-steer__chip qe-steer__chip--fixed" disabled aria-disabled="true">
              not yet supported
            </button>
          </div>
          <span className="qe-steer__hint">
            Including already-sealed evolved-pool formulas as indicators is <strong>not yet supported on the
            live search</strong> (a QE-402-safe feature-space extension is a follow-up); the server rejects it.
            This control is disabled so the form never issues a request the engine would reject.
          </span>
        </div>
      </div>
    </Card>
  );
}

/** The search budget + WFO windows / CV folds / holdout / embargo inputs (QE-458/459 whitelist). */
export function SearchBudgetCard({ steer }: { steer: SteerControls }) {
  return (
    <Card title="Search budget & windows (steer)">
      <div style={{ display: 'flex', flexDirection: 'column', gap: 16 }}>
        <div className="qe-steer__grid">
          <Input
            label="Generations"
            mono
            placeholder="default"
            value={steer.generations}
            onChange={(e) => steer.setGenerations(e.target.value)}
          />
          <Input
            label="Population"
            mono
            placeholder="default"
            value={steer.population}
            onChange={(e) => steer.setPopulation(e.target.value)}
          />
          <Input
            label={`WFO windows (≥ ${MIN_WINDOWS})`}
            mono
            placeholder="default"
            value={steer.windows}
            onChange={(e) => steer.setWindows(e.target.value)}
          />
          <Input
            label={`CV folds (≥ ${MIN_FOLDS})`}
            mono
            placeholder="default"
            value={steer.folds}
            onChange={(e) => steer.setFolds(e.target.value)}
          />
          <Input
            label={`Holdout bars (≥ ${HOLDOUT_FLOOR})`}
            mono
            placeholder="default"
            value={steer.holdout}
            onChange={(e) => steer.setHoldout(e.target.value)}
          />
          <Input
            label={`Embargo bars (≥ ${EMBARGO_FLOOR})`}
            mono
            placeholder="default"
            value={steer.embargo}
            onChange={(e) => steer.setEmbargo(e.target.value)}
          />
        </div>
        <span className="qe-steer__hint">
          Leave blank to use the engine defaults (a small, fast fixture budget). More generations / longer
          windows raise <code>T&#8202;eff</code> and make the search <em>harder</em> to pass, never easier.
        </span>
      </div>
    </Card>
  );
}

/** The projected distinct-trial `N` deflation-scaling feedback (honest, never a fabricated pre-run figure). */
export function DeflationFeedbackCard({ steer }: { steer: SteerControls }) {
  return (
    <Card title="Deflation-scaling feedback">
      <div className="qe-steer__feedback">
        <div className="qe-steer__stat">
          <span className="qe-steer__statN" aria-label="Projected distinct-trial N">
            {steer.projectedN.toLocaleString()}
          </span>
          <span className="qe-steer__statLbl">
            projected distinct-trial <code>N</code> — the deflation bar rises with scope
          </span>
        </div>
        <span className="qe-steer__hint">
          Projected from the selected subset ({steer.subsetSize} indicator
          {steer.subsetSize === 1 ? '' : 's'}) ×{' '}
          {steer.budgetIsIndicative ? 'the indicative default budget' : 'your budget'} (
          <code>
            {DESCRIPTOR_CELLS}·{steer.projGen} gen·{steer.projWin} win·{steer.subsetSize}
          </code>
          ). Indicative, not the sealed basis: widening the subset or raising the budget only <em>raises</em>{' '}
          <code>N</code> and the <code>E[max&nbsp;SR]</code> bar — steering buys no free pass.
        </span>
        <span className="qe-steer__hint">
          <strong>Archive coverage</strong> (occupied niches, pre/post steer) is a runtime search output — it
          is <em>recorded after the run</em> and surfaced in the Vintage Inspector, not previewed here. A steer
          that collapses the quality-diversity archive is caught by the engine, never hidden.
        </span>
      </div>
    </Card>
  );
}

/** The fixed, disabled compiled-floor guardrail chips (QE-458 blocklist) — no control can set them. */
export function CompiledFloorsCard() {
  return (
    <Card title="Compiled floors (not steerable)">
      <div className="qe-steer__row">
        <div className="qe-steer__chips" role="group" aria-label="Compiled gate floors">
          {GUARDRAIL_FLOORS.map((g) => (
            <button
              key={g.label}
              type="button"
              className="qe-steer__chip qe-steer__chip--fixed"
              disabled
              aria-disabled="true"
            >
              {g.label} {g.value}
            </button>
          ))}
        </div>
        <span className="qe-steer__hint">
          These thresholds ride the G1 gate&rsquo;s own decision and are <strong>fixed compiled floors</strong>{' '}
          the research path cannot relax. There is no form control that can set them — a request that tries is
          rejected server-side. Steering changes <em>what</em> the search explores and <em>how hard</em>, never{' '}
          <em>what passes</em>.
        </span>
      </div>
    </Card>
  );
}
