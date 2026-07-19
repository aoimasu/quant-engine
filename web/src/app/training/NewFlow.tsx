import { useState } from 'react';
import { Button, Callout, Card, Icon, Input, Select } from '../../design';
import { injectCss } from '../../design/injectCss';
import { ApiError, createFlowRun, type FlowParams } from '../../api/runs';
import { CATALOGUE_INDICATORS, optInt, RESOLUTIONS, useSteerControls } from './steer';
import {
  CompiledFloorsCard,
  DeflationFeedbackCard,
  IndicatorSubsetCard,
  SearchBudgetCard,
} from './steerCards';

const CSS = `
.qe-nf { max-width: 880px; margin: 0 auto; padding: 24px; display: flex; flex-direction: column; gap: 16px; }
.qe-nf__hd h2 { font-family: var(--font-display); font-size: var(--fs-lg); font-weight: 600; }
.qe-nf__grid { display: grid; grid-template-columns: 1fr 1fr; gap: 14px; }
.qe-nf__row { display: flex; flex-direction: column; gap: 6px; }
.qe-nf__lbl { font-family: var(--font-sans); font-size: var(--fs-sm); font-weight: var(--fw-medium); color: var(--text-secondary); }
.qe-nf__hint { font-size: var(--fs-caption); color: var(--text-muted); margin-top: 2px; }
.qe-nf__actions { display: flex; justify-content: space-between; gap: 10px; }
.qe-nf__actions .sp { flex: 1; }
.qe-nf__steps { display: flex; align-items: center; gap: 8px; margin-top: 10px; }
.qe-nf__step { display: flex; align-items: center; gap: 8px; font: 600 11px var(--font-mono); text-transform: uppercase; letter-spacing: .06em; color: var(--text-muted); }
.qe-nf__step .n { display: inline-flex; align-items: center; justify-content: center; width: 20px; height: 20px; border-radius: var(--radius-pill); border: 1px solid var(--border-default); font-size: 11px; }
.qe-nf__step--active { color: var(--violet-200); }
.qe-nf__step--active .n { border-color: var(--violet-400); background: var(--accent-fill-soft); color: var(--violet-200); }
.qe-nf__step--done { color: var(--text-secondary); }
.qe-nf__sep { flex: 0 0 20px; height: 1px; background: var(--border-subtle); }
.qe-nf__preview { font: 500 12px var(--font-mono); color: var(--text-secondary); background: var(--surface-inset); border: 1px solid var(--border-subtle); border-radius: var(--radius-md); padding: 12px 14px; overflow-x: auto; white-space: pre; }
.qe-nf__sum { display: grid; grid-template-columns: 1fr 1fr; gap: 10px; }
.qe-nf__sum .row { display: flex; flex-direction: column; gap: 2px; }
.qe-nf__sum .row .k { font: 500 10px var(--font-mono); text-transform: uppercase; letter-spacing: .06em; color: var(--text-muted); }
.qe-nf__sum .row .v { font-family: var(--font-mono); font-size: 12px; color: var(--text-secondary); }
`;

injectCss('qe-nf-css', CSS);

const STEPS = ['Configure', 'Review', 'Launch'] as const;

export interface NewFlowProps {
  onCreated: (id: string) => void;
  onCancel: () => void;
}

/**
 * New composite-**flow** run — the single stepped page (configure → review → launch) that configures the
 * QE-459 steer controls + the training window + the **required** flow seed + the frozen-holdout size/embargo
 * **once**, then submits a single `POST /api/runs {type:"flow"}` (QE-460). The backtest window is *not*
 * operator-chosen — it is the server-frozen holdout the train phase carves, so there is no separate backtest
 * window field. The steer controls are the shared {@link useSteerControls} primitives (identical to the
 * `type:"train"` form), so the flow can never steer past a floor a train could not; the blocklisted
 * gate-decision thresholds render as disabled guardrail chips and are never submitted. Client validation only
 * *removes* affordances — the server's `validate_flow` (which reuses `validate_train` verbatim) is the single
 * enforcement point, and a `400` is surfaced inline.
 */
export function NewFlow({ onCreated, onCancel }: NewFlowProps) {
  const [step, setStep] = useState(0); // 0 = configure, 1 = review (launch is the terminal action)
  const [start, setStart] = useState('');
  const [end, setEnd] = useState('');
  const [resolution, setResolution] = useState('1h');
  const [seed, setSeed] = useState('');
  const steer = useSteerControls();

  const [fieldError, setFieldError] = useState<string | null>(null);
  const [serverError, setServerError] = useState<string | null>(null);
  const [submitting, setSubmitting] = useState(false);

  const validate = (): string | null => {
    if (!start) return 'Choose a training-window start date.';
    if (!end) return 'Choose a training-window end date.';
    if (start >= end) return 'The window start must be before the end.';
    if (!resolution) return 'Choose a bar resolution.';
    // The flow seed is REQUIRED (a flow verdict must stay byte-reproducible off the recorded seed).
    const seedN = optInt(seed);
    if (seedN === undefined) return 'A flow requires a seed (it must be byte-reproducible).';
    if (Number.isNaN(seedN)) return 'Seed must be a non-negative whole number.';
    return steer.validate();
  };

  /** Build the exact `FlowParams` body that will be posted (whitelisted fields only). */
  const buildParams = (): FlowParams => {
    const seedN = optInt(seed);
    const params: FlowParams = {
      seed: seedN as number, // validated present + numeric before this is reached
      start,
      end,
      resolution,
    };
    steer.applyTo(params as unknown as Record<string, unknown>);
    return params;
  };

  const toReview = () => {
    const err = validate();
    if (err) {
      setFieldError(err);
      return;
    }
    setFieldError(null);
    setStep(1);
  };

  const launch = async () => {
    setServerError(null);
    // Re-validate defensively (the configure step is the gate, but never trust a stale step).
    const err = validate();
    if (err) {
      setFieldError(err);
      setStep(0);
      return;
    }
    setSubmitting(true);
    try {
      const id = await createFlowRun(buildParams());
      onCreated(id);
    } catch (e) {
      setServerError(e instanceof ApiError ? e.message : 'Failed to start the flow run.');
      setSubmitting(false);
    }
  };

  const stepper = (
    <div className="qe-nf__steps" aria-label="Flow steps">
      {STEPS.map((label, i) => {
        // Configure/Review track `step`; Launch is active only while submitting.
        const active = i === 2 ? submitting : i === step;
        const done = i === 2 ? false : i < step;
        return (
          <span
            key={label}
            className={`qe-nf__step${active ? ' qe-nf__step--active' : ''}${done ? ' qe-nf__step--done' : ''}`}
          >
            {i > 0 && <span className="qe-nf__sep" aria-hidden="true" />}
            <span className="n">{i + 1}</span>
            {label}
          </span>
        );
      })}
    </div>
  );

  return (
    <div className="qe-nf">
      <div className="qe-nf__hd">
        <Button variant="ghost" size="sm" onClick={onCancel} iconLeft={<Icon name="arrow-left" size={15} />}>
          All training runs
        </Button>
        <h2 style={{ marginTop: 8 }}>New flow run</h2>
        <div className="qe-nf__hint">
          One supervised <strong>train → backtest</strong> over a server-frozen, regime-stratified holdout —
          configured once, launched as a single run (QE-460).
        </div>
        {stepper}
      </div>

      {step === 0 && (
        <>
          <Card title="Training window & flow seed">
            <div style={{ display: 'flex', flexDirection: 'column', gap: 16 }}>
              <div className="qe-nf__grid">
                <Input label="Start" type="date" value={start} onChange={(e) => setStart(e.target.value)} />
                <Input label="End" type="date" value={end} onChange={(e) => setEnd(e.target.value)} />
                <div className="qe-nf__row">
                  <label className="qe-nf__lbl" htmlFor="qe-nf-res">
                    Resolution
                  </label>
                  <Select
                    id="qe-nf-res"
                    aria-label="Resolution"
                    value={resolution}
                    onChange={(e) => setResolution(e.target.value)}
                    options={RESOLUTIONS}
                  />
                </div>
                <Input
                  label="Seed (required)"
                  mono
                  placeholder="required"
                  value={seed}
                  onChange={(e) => setSeed(e.target.value)}
                />
              </div>
              <span className="qe-nf__hint">
                The training universe and market-data store are resolved from server config. The backtest runs
                against the <strong>server-frozen holdout</strong> the train phase carves — it is not chosen
                here. The seed is required so the flow verdict is byte-reproducible.
              </span>
            </div>
          </Card>

          <IndicatorSubsetCard steer={steer} />
          <SearchBudgetCard steer={steer} />
          <DeflationFeedbackCard steer={steer} />
          <CompiledFloorsCard />

          {fieldError && (
            <Callout variant="warn" title="Check the form">
              {fieldError}
            </Callout>
          )}

          <div className="qe-nf__actions">
            <Button variant="secondary" onClick={onCancel} disabled={submitting}>
              Cancel
            </Button>
            <span className="sp" />
            <Button
              variant="primary"
              onClick={toReview}
              iconLeft={<Icon name="arrow-right" size={15} />}
            >
              Next: review
            </Button>
          </div>
        </>
      )}

      {step === 1 && (
        <>
          <Card title="Review — the single flow request">
            <div style={{ display: 'flex', flexDirection: 'column', gap: 14 }}>
              <div className="qe-nf__sum">
                <div className="row">
                  <span className="k">Window</span>
                  <span className="v">
                    {start} → {end} · {resolution}
                  </span>
                </div>
                <div className="row">
                  <span className="k">Seed</span>
                  <span className="v">{seed}</span>
                </div>
                <div className="row">
                  <span className="k">Indicator subset</span>
                  <span className="v">
                    {steer.indicators.length === CATALOGUE_INDICATORS.length
                      ? 'full catalogue (engine default)'
                      : `${steer.indicators.length} selected`}
                  </span>
                </div>
                <div className="row">
                  <span className="k">Projected distinct-trial N</span>
                  <span className="v">{steer.projectedN.toLocaleString()}</span>
                </div>
              </div>
              <span className="qe-nf__hint">
                This posts a <strong>single</strong> <code>type:"flow"</code> create-run. The server sequences
                train → backtest as one supervised, atomic run and carves the frozen holdout once. Only the
                whitelisted steer fields below are sent — no blocklisted gate-decision threshold and no
                separate backtest window.
              </span>
              <pre className="qe-nf__preview" aria-label="Flow request body">
                {JSON.stringify({ type: 'flow', params: buildParams() }, null, 2)}
              </pre>
            </div>
          </Card>

          {serverError && (
            <Callout variant="danger" title="The server rejected the request">
              {serverError}
            </Callout>
          )}

          <div className="qe-nf__actions">
            <Button
              variant="secondary"
              onClick={() => setStep(0)}
              disabled={submitting}
              iconLeft={<Icon name="arrow-left" size={15} />}
            >
              Back
            </Button>
            <span className="sp" />
            <Button
              variant="primary"
              loading={submitting}
              onClick={launch}
              iconLeft={<Icon name="play" size={15} />}
            >
              Launch flow
            </Button>
          </div>
        </>
      )}
    </div>
  );
}
