import { useState } from 'react';
import { Button, Callout, Card, Icon, Input, Select } from '../../design';
import { injectCss } from '../../design/injectCss';
import { ApiError, createTrainRun, type TrainParams } from '../../api/runs';
import { optInt, RESOLUTIONS, useSteerControls } from './steer';
import {
  CompiledFloorsCard,
  DeflationFeedbackCard,
  IndicatorSubsetCard,
  SearchBudgetCard,
} from './steerCards';

const CSS = `
.qe-nt { max-width: 880px; margin: 0 auto; padding: 24px; display: flex; flex-direction: column; gap: 16px; }
.qe-nt__hd h2 { font-family: var(--font-display); font-size: var(--fs-lg); font-weight: 600; }
.qe-nt__grid { display: grid; grid-template-columns: 1fr 1fr; gap: 14px; }
.qe-nt__row { display: flex; flex-direction: column; gap: 6px; }
.qe-nt__lbl { font-family: var(--font-sans); font-size: var(--fs-sm); font-weight: var(--fw-medium); color: var(--text-secondary); }
.qe-nt__hint { font-size: var(--fs-caption); color: var(--text-muted); margin-top: 2px; }
.qe-nt__actions { display: flex; justify-content: flex-end; gap: 10px; }
`;

injectCss('qe-nt-css', CSS);

export interface NewTrainingProps {
  onCreated: (id: string) => void;
  onCancel: () => void;
}

/**
 * New training run (trigger) form — window/resolution/seed + the QE-458 **whitelisted steer knobs** (QE-459):
 * an indicator picker (catalogue subset), search budget (generations/population), and windows/folds — all
 * submitted as the whitelisted `TrainParams` fields → `POST /api/runs {type:"train"}`. The **blocklisted**
 * gate-decision thresholds render as fixed disabled guardrail chips (there is no control that can set them),
 * and evolved-pool inclusion is a disabled "not yet supported" affordance (QE-458 rejects it server-side), so
 * the form can never issue an always-`400` request. The steer controls are the shared {@link useSteerControls}
 * primitives (also driving the `type:"flow"` form), so the train and flow paths can never diverge on what is
 * steerable. A projected distinct-trial `N` teaches that widening the subset / raising the budget *raises* the
 * deflation bar — steering buys no free pass; archive coverage is honestly framed as recorded **after** the
 * run (the Vintage Inspector surfaces it), never fabricated pre-run.
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
  const steer = useSteerControls();

  const [fieldError, setFieldError] = useState<string | null>(null);
  const [serverError, setServerError] = useState<string | null>(null);
  const [submitting, setSubmitting] = useState(false);

  const validate = (): string | null => {
    if (!start) return 'Choose a training-window start date.';
    if (!end) return 'Choose a training-window end date.';
    if (start >= end) return 'The window start must be before the end.';
    if (!resolution) return 'Choose a bar resolution.';
    if (Number.isNaN(optInt(seed))) return 'Seed must be a non-negative whole number.';
    return steer.validate();
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
    steer.applyTo(params as unknown as Record<string, unknown>);
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

      <IndicatorSubsetCard steer={steer} />
      <SearchBudgetCard steer={steer} />
      <DeflationFeedbackCard steer={steer} />
      <CompiledFloorsCard />

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
