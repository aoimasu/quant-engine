import { useState } from 'react';
import { Button, Callout, Card, Icon, Input, Select } from '../../design';
import { injectCss } from '../../design/injectCss';
import { ApiError, createEvolveRun, type EvolveMode, type EvolveParams } from '../../api/runs';

const CSS = `
.qe-nc { max-width: 880px; margin: 0 auto; padding: 24px; display: flex; flex-direction: column; gap: 16px; }
.qe-nc__hd h2 { font-family: var(--font-display); font-size: var(--fs-lg); font-weight: 600; }
.qe-nc__grid { display: grid; grid-template-columns: 1fr 1fr; gap: 14px; }
.qe-nc__row { display: flex; flex-direction: column; gap: 6px; }
.qe-nc__lbl { font-family: var(--font-sans); font-size: var(--fs-sm); font-weight: var(--fw-medium); color: var(--text-secondary); }
.qe-nc__hint { font-size: var(--fs-caption); color: var(--text-muted); margin-top: 2px; }
.qe-nc__actions { display: flex; justify-content: flex-end; gap: 10px; }
.qe-nc__seg { display: inline-flex; gap: 4px; padding: 3px; background: var(--surface-inset); border: 1px solid var(--border-default); border-radius: var(--radius-md); width: max-content; }
.qe-nc__seg button { font: 500 12px var(--font-mono); padding: 6px 14px; border: none; border-radius: var(--radius-sm); background: transparent; color: var(--text-tertiary); cursor: pointer; }
.qe-nc__seg button[aria-pressed="true"] { background: var(--accent-fill-soft); color: var(--violet-200); }
.qe-nc__chips { display: flex; flex-wrap: wrap; gap: 6px; }
.qe-nc__chip { font: 500 11px var(--font-mono); padding: 5px 10px; border-radius: var(--radius-pill); border: 1px solid var(--border-default); color: var(--text-tertiary); background: transparent; cursor: pointer; }
.qe-nc__chip[aria-pressed="true"] { background: var(--accent-fill-soft); border-color: var(--violet-400); color: var(--violet-200); }
`;

injectCss('qe-nc-css', CSS);

const RESOLUTIONS = ['1m', '5m', '1h', '4h', '1d'];

/** The fixed window-length lattice — the ONLY admissible windows (mirrors `EVOLVE_WINDOW_LATTICE`). */
const WINDOW_LATTICE = [5, 10, 20, 50, 100] as const;

/** The server caps (`validate_evolve`): a client-side violation must block submit, exactly like the server. */
const CAP_DEPTH = 4;
const CAP_NODES = 16;
const CAP_LOOKBACK = 200;
const CAP_K = 16;

export interface NewCampaignProps {
  onCreated: (id: string) => void;
  onCancel: () => void;
}

/** Parse an optional positive-integer field: blank → undefined; invalid → the sentinel `NaN`. */
function optInt(raw: string): number | undefined {
  const s = raw.trim();
  if (s === '') return undefined;
  const n = Number(s);
  return Number.isInteger(n) && n >= 0 ? n : NaN;
}

/**
 * New evolve campaign (trigger) form — mirrors {@link NewTraining}. Configures window/resolution + the
 * **required** seed, the sandbox/production mode, and the capped GP params → `createEvolveRun` →
 * `POST /api/runs {type:"evolve"}` (QE-452).
 *
 * Client-side validation mirrors the server's `validate_evolve`: **seed is REQUIRED** (the form will not
 * submit without it), the caps (`depth≤4`, `nodes≤16`, `lookback≤200`, `K≤16`) block submit, and windows
 * are constrained to the fixed lattice via toggle-chips (no free entry). A server `400` — including a
 * production launch refused by the compiled prereq const (QE-454) — is surfaced inline, never hidden.
 */
export function NewCampaign({ onCreated, onCancel }: NewCampaignProps) {
  const [seed, setSeed] = useState('');
  const [mode, setMode] = useState<EvolveMode>('sandbox');
  const [start, setStart] = useState('');
  const [end, setEnd] = useState('');
  const [resolution, setResolution] = useState('1h');
  const [generations, setGenerations] = useState('');
  const [offspring, setOffspring] = useState('');
  const [depth, setDepth] = useState('');
  const [nodes, setNodes] = useState('');
  const [lookback, setLookback] = useState('');
  const [k, setK] = useState('');
  // Windows are picked from the fixed lattice; default to the whole lattice (blank ⇒ engine default).
  const [windows, setWindows] = useState<number[]>([...WINDOW_LATTICE]);

  const [fieldError, setFieldError] = useState<string | null>(null);
  const [serverError, setServerError] = useState<string | null>(null);
  const [submitting, setSubmitting] = useState(false);

  const toggleWindow = (w: number) =>
    setWindows((prev) => (prev.includes(w) ? prev.filter((x) => x !== w) : [...prev, w].sort((a, b) => a - b)));

  const validate = (): string | null => {
    // seed is REQUIRED — the single most important client-side gate (mirrors the serde-required field).
    const seedN = optInt(seed);
    if (seedN === undefined) return 'A seed is required — an evolve campaign must be byte-reproducible.';
    if (Number.isNaN(seedN)) return 'Seed must be a non-negative whole number.';
    if (!start) return 'Choose a campaign-window start date.';
    if (!end) return 'Choose a campaign-window end date.';
    if (start >= end) return 'The window start must be before the end.';
    if (!resolution) return 'Choose a bar resolution.';
    for (const [label, raw, cap] of [
      ['Depth', depth, CAP_DEPTH],
      ['Nodes', nodes, CAP_NODES],
      ['Lookback', lookback, CAP_LOOKBACK],
      ['K', k, CAP_K],
    ] as const) {
      const n = optInt(raw);
      if (Number.isNaN(n)) return `${label} must be a non-negative whole number.`;
      if (n !== undefined && n > cap) return `${label} must be ≤ ${cap} (the engine cap).`;
    }
    for (const [label, raw] of [
      ['Generations', generations],
      ['Offspring', offspring],
    ] as const) {
      if (Number.isNaN(optInt(raw))) return `${label} must be a non-negative whole number.`;
    }
    if (windows.length === 0) return 'Select at least one window from the lattice.';
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
    // seed is guaranteed present by validate(); the `!` is safe here.
    const params: EvolveParams = { seed: optInt(seed)!, mode, start, end, resolution };
    const gen = optInt(generations);
    if (gen !== undefined) params.generations = gen;
    const off = optInt(offspring);
    if (off !== undefined) params.offspring = off;
    const d = optInt(depth);
    if (d !== undefined) params.depth = d;
    const nd = optInt(nodes);
    if (nd !== undefined) params.nodes = nd;
    const lb = optInt(lookback);
    if (lb !== undefined) params.lookback = lb;
    const kk = optInt(k);
    if (kk !== undefined) params.k = kk;
    // Only send `windows` when it is a strict subset of the lattice; the full lattice ⇒ engine default.
    if (windows.length !== WINDOW_LATTICE.length) params.windows = windows;
    try {
      const id = await createEvolveRun(params);
      onCreated(id);
    } catch (e) {
      setServerError(e instanceof ApiError ? e.message : 'Failed to launch the evolve campaign.');
      setSubmitting(false);
    }
  };

  return (
    <div className="qe-nc">
      <div className="qe-nc__hd">
        <Button variant="ghost" size="sm" onClick={onCancel} iconLeft={<Icon name="arrow-left" size={15} />}>
          All campaigns
        </Button>
        <h2 style={{ marginTop: 8 }}>New evolve campaign</h2>
      </div>

      <Card title="Campaign">
        <div style={{ display: 'flex', flexDirection: 'column', gap: 16 }}>
          <div className="qe-nc__grid">
            <Input
              label="Seed (required)"
              mono
              placeholder="e.g. 20260718"
              value={seed}
              onChange={(e) => setSeed(e.target.value)}
            />
            <div className="qe-nc__row">
              <span className="qe-nc__lbl">Mode</span>
              <div className="qe-nc__seg" role="group" aria-label="Mode">
                <button type="button" aria-pressed={mode === 'sandbox'} onClick={() => setMode('sandbox')}>
                  Sandbox
                </button>
                <button
                  type="button"
                  aria-pressed={mode === 'production'}
                  onClick={() => setMode('production')}
                >
                  Production
                </button>
              </div>
            </div>
            <Input label="Start" type="date" value={start} onChange={(e) => setStart(e.target.value)} />
            <Input label="End" type="date" value={end} onChange={(e) => setEnd(e.target.value)} />
            <div className="qe-nc__row">
              <label className="qe-nc__lbl" htmlFor="qe-nc-res">
                Resolution
              </label>
              <Select
                id="qe-nc-res"
                aria-label="Resolution"
                value={resolution}
                onChange={(e) => setResolution(e.target.value)}
                options={RESOLUTIONS}
              />
            </div>
          </div>
          {mode === 'production' && (
            <Callout variant="warn" title="Production is gated">
              A production campaign only launches once the compiled prerequisite gate (QE-439/434/436/432/430)
              is satisfied. Until QE-454 lands, the server will refuse it with a clear error — surfaced below,
              not hidden. Sandbox is RESEARCH and cannot reach a production vintage.
            </Callout>
          )}
        </div>
      </Card>

      <Card title="Search caps (guardrails)">
        <div style={{ display: 'flex', flexDirection: 'column', gap: 16 }}>
          <div className="qe-nc__grid">
            <Input
              label={`Depth (≤ ${CAP_DEPTH})`}
              mono
              placeholder="default"
              value={depth}
              onChange={(e) => setDepth(e.target.value)}
            />
            <Input
              label={`Nodes (≤ ${CAP_NODES})`}
              mono
              placeholder="default"
              value={nodes}
              onChange={(e) => setNodes(e.target.value)}
            />
            <Input
              label={`Lookback (≤ ${CAP_LOOKBACK})`}
              mono
              placeholder="default"
              value={lookback}
              onChange={(e) => setLookback(e.target.value)}
            />
            <Input
              label={`Pool size K (≤ ${CAP_K})`}
              mono
              placeholder="default"
              value={k}
              onChange={(e) => setK(e.target.value)}
            />
            <Input
              label="Generations"
              mono
              placeholder="default"
              value={generations}
              onChange={(e) => setGenerations(e.target.value)}
            />
            <Input
              label="Offspring / gen"
              mono
              placeholder="default"
              value={offspring}
              onChange={(e) => setOffspring(e.target.value)}
            />
          </div>
          <div className="qe-nc__row">
            <span className="qe-nc__lbl">Window lattice</span>
            <div className="qe-nc__chips" role="group" aria-label="Window lattice">
              {WINDOW_LATTICE.map((w) => (
                <button
                  key={w}
                  type="button"
                  className="qe-nc__chip"
                  aria-pressed={windows.includes(w)}
                  onClick={() => toggleWindow(w)}
                >
                  {w}
                </button>
              ))}
            </div>
            <span className="qe-nc__hint">
              Windows are fixed to the lattice {'{5,10,20,50,100}'}; there is no free window entry. The full
              lattice defers to the engine default.
            </span>
          </div>
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

      <div className="qe-nc__actions">
        <Button variant="secondary" onClick={onCancel} disabled={submitting}>
          Cancel
        </Button>
        <Button
          variant="primary"
          loading={submitting}
          onClick={submit}
          iconLeft={<Icon name="play" size={15} />}
        >
          Launch campaign
        </Button>
      </div>
    </div>
  );
}
