import { useState } from 'react';
import { Button, Callout, Card, Icon, Input, Select } from '../../design';
import { injectCss } from '../../design/injectCss';
import { ApiError, createIngestRun, type IngestParams } from '../../api/runs';
import { RESOLUTIONS } from '../training/steer';

const CSS = `
.qe-ni { max-width: 880px; margin: 0 auto; padding: 24px; display: flex; flex-direction: column; gap: 16px; }
.qe-ni__hd h2 { font-family: var(--font-display); font-size: var(--fs-lg); font-weight: 600; }
.qe-ni__hint { font-size: var(--fs-caption); color: var(--text-muted); margin-top: 2px; }
.qe-ni__grid { display: grid; grid-template-columns: 1fr 1fr; gap: 14px; }
.qe-ni__row { display: flex; flex-direction: column; gap: 6px; }
.qe-ni__lbl { font-family: var(--font-sans); font-size: var(--fs-sm); font-weight: var(--fw-medium); color: var(--text-secondary); }
.qe-ni__toggle { display: flex; align-items: center; gap: 10px; padding: 12px 14px; background: var(--surface-inset); border: 1px solid var(--border-subtle); border-radius: var(--radius-md); }
.qe-ni__toggle input { width: 16px; height: 16px; accent-color: var(--violet-400); }
.qe-ni__toggle .t { display: flex; flex-direction: column; gap: 2px; }
.qe-ni__toggle .t .k { font-family: var(--font-sans); font-size: var(--fs-sm); font-weight: var(--fw-medium); color: var(--text-secondary); }
.qe-ni__toggle .t .d { font-size: var(--fs-caption); color: var(--text-muted); }
.qe-ni__actions { display: flex; justify-content: space-between; gap: 10px; }
.qe-ni__actions .sp { flex: 1; }
.qe-ni__preview { font: 500 12px var(--font-mono); color: var(--text-secondary); background: var(--surface-inset); border: 1px solid var(--border-subtle); border-radius: var(--radius-md); padding: 12px 14px; overflow-x: auto; white-space: pre; }
`;

injectCss('qe-ni-css', CSS);

export interface NewIngestProps {
  onCreated: (id: string) => void;
  onCancel: () => void;
}

/** Split a free-text instrument list (comma / whitespace separated) into canonical upper-case symbols. */
function parseInstruments(raw: string): string[] {
  return raw
    .split(/[\s,]+/)
    .map((s) => s.trim().toUpperCase())
    .filter((s) => s.length > 0);
}

/**
 * New **ingest** run — the QE-465 ingest-trigger screen. Selects instruments (or a **fetch-all** toggle),
 * a date range, and a resolution, then submits a single **`POST /api/ingest`** (QE-464) whose body is the
 * {@link IngestParams} object directly. After launch the caller opens the standard run monitor.
 *
 * The `synthetic` flag is deliberately not an operator control here — this is the *real*-ingest trigger,
 * so the body always sends `synthetic:false` (no store the SPA populates is ambiguously synthetic; design
 * §8.2). Client validation only *removes* affordances (empty-and-not-fetch-all ⇒ inline warn, start ≥ end
 * ⇒ warn); the server's `validate_ingest` stays the single enforcement point and a `400` is surfaced inline.
 */
export function NewIngest({ onCreated, onCancel }: NewIngestProps) {
  const [instrumentsRaw, setInstrumentsRaw] = useState('');
  const [fetchAll, setFetchAll] = useState(false);
  const [start, setStart] = useState('');
  const [end, setEnd] = useState('');
  const [resolution, setResolution] = useState('1h');

  const [fieldError, setFieldError] = useState<string | null>(null);
  const [serverError, setServerError] = useState<string | null>(null);
  const [submitting, setSubmitting] = useState(false);

  const instruments = parseInstruments(instrumentsRaw);

  /** Build the exact `IngestParams` body that will be POSTed to `/api/ingest`. */
  const buildParams = (): IngestParams => ({
    // Fetch-all resolves the universe server-side, so the explicit list is dropped (mirrors validate_ingest).
    instruments: fetchAll ? [] : instruments,
    fetch_all: fetchAll,
    start,
    end,
    resolution,
    synthetic: false,
  });

  const validate = (): string | null => {
    if (!start) return 'Choose an ingest-window start date.';
    if (!end) return 'Choose an ingest-window end date.';
    if (start >= end) return 'The window start must be before the end.';
    if (!resolution) return 'Choose a bar resolution.';
    // Mirror validate_ingest's either/or: a named instrument list OR fetch-all — never neither.
    if (!fetchAll && instruments.length === 0) {
      return 'Name at least one instrument, or enable fetch-all.';
    }
    return null;
  };

  const launch = async () => {
    setServerError(null);
    const err = validate();
    if (err) {
      setFieldError(err);
      return;
    }
    setFieldError(null);
    setSubmitting(true);
    try {
      const id = await createIngestRun(buildParams());
      onCreated(id);
    } catch (e) {
      setServerError(e instanceof ApiError ? e.message : 'Failed to start the ingest run.');
      setSubmitting(false);
    }
  };

  return (
    <div className="qe-ni">
      <div className="qe-ni__hd">
        <Button variant="ghost" size="sm" onClick={onCancel} iconLeft={<Icon name="arrow-left" size={15} />}>
          Market data
        </Button>
        <h2 style={{ marginTop: 8 }}>Ingest market data</h2>
        <div className="qe-ni__hint">
          Populate the local market-data store from the real historical decoder — every ingested bar is
          tagged <strong>real</strong> provenance (QE-464). Launches a supervised run you can monitor and
          cancel.
        </div>
      </div>

      <Card title="Instruments">
        <div style={{ display: 'flex', flexDirection: 'column', gap: 14 }}>
          <div className="qe-ni__row">
            <label className="qe-ni__lbl" htmlFor="qe-ni-instruments">
              Instruments
            </label>
            <Input
              id="qe-ni-instruments"
              aria-label="Instruments"
              mono
              placeholder="BTCUSDT ETHUSDT"
              value={instrumentsRaw}
              disabled={fetchAll}
              onChange={(e) => setInstrumentsRaw(e.target.value)}
            />
            <span className="qe-ni__hint">
              Comma- or space-separated symbols. Ignored when fetch-all is on.
            </span>
          </div>

          <label className="qe-ni__toggle">
            <input
              type="checkbox"
              aria-label="Fetch all instruments"
              checked={fetchAll}
              onChange={(e) => setFetchAll(e.target.checked)}
            />
            <span className="t">
              <span className="k">Fetch all instruments</span>
              <span className="d">
                Resolve the full instrument set from the point-in-time universe (survivorship-safe).
              </span>
            </span>
          </label>
        </div>
      </Card>

      <Card title="Window & resolution">
        <div className="qe-ni__grid">
          <Input label="Start" type="date" value={start} onChange={(e) => setStart(e.target.value)} />
          <Input label="End" type="date" value={end} onChange={(e) => setEnd(e.target.value)} />
          <div className="qe-ni__row">
            <label className="qe-ni__lbl" htmlFor="qe-ni-res">
              Resolution
            </label>
            <Select
              id="qe-ni-res"
              aria-label="Resolution"
              value={resolution}
              onChange={(e) => setResolution(e.target.value)}
              options={RESOLUTIONS}
            />
          </div>
        </div>
      </Card>

      <Card title="Request">
        <pre className="qe-ni__preview" aria-label="Ingest request body">
          {JSON.stringify(buildParams(), null, 2)}
        </pre>
        <span className="qe-ni__hint">
          Posted directly to <code>POST /api/ingest</code>. The server’s <code>validate_ingest</code> is the
          enforcement point — a bad request is surfaced below.
        </span>
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

      <div className="qe-ni__actions">
        <Button variant="secondary" onClick={onCancel} disabled={submitting}>
          Cancel
        </Button>
        <span className="sp" />
        <Button
          variant="primary"
          loading={submitting}
          onClick={launch}
          iconLeft={<Icon name="arrow-right" size={15} />}
        >
          Launch ingest
        </Button>
      </div>
    </div>
  );
}
