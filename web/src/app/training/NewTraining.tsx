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
`;

injectCss('qe-nt-css', CSS);

const RESOLUTIONS = ['1m', '5m', '1h', '4h', '1d'];

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
 * New training run (trigger) form — window/resolution/seed + optional MAP-Elites budget →
 * `POST /api/runs {type:"train"}` (QE-261). The training universe and store are resolved server-side
 * from config (there is no `--universe` flag on `qe train`), so this form does not select instruments.
 * Client-side validation surfaces missing/invalid fields; a server 400 is surfaced inline.
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

  const [fieldError, setFieldError] = useState<string | null>(null);
  const [serverError, setServerError] = useState<string | null>(null);
  const [submitting, setSubmitting] = useState(false);

  const validate = (): string | null => {
    if (!start) return 'Choose a training-window start date.';
    if (!end) return 'Choose a training-window end date.';
    if (start >= end) return 'The window start must be before the end.';
    if (!resolution) return 'Choose a bar resolution.';
    for (const [label, raw] of [
      ['Seed', seed],
      ['Generations', generations],
      ['Population', population],
      ['Holdout', holdout],
      ['Embargo', embargo],
    ] as const) {
      if (Number.isNaN(optInt(raw))) return `${label} must be a non-negative whole number.`;
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

      <Card title="Search budget (optional)">
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
              label="Holdout bars"
              mono
              placeholder="default"
              value={holdout}
              onChange={(e) => setHoldout(e.target.value)}
            />
            <Input
              label="Embargo bars"
              mono
              placeholder="default"
              value={embargo}
              onChange={(e) => setEmbargo(e.target.value)}
            />
          </div>
          <span className="qe-nt__hint">
            Leave blank to use the engine defaults (a small, fast fixture budget).
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
