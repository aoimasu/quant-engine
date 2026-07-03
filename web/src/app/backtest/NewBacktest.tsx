import { useEffect, useMemo, useState } from 'react';
import { Badge, Button, Callout, Card, Icon, Input, Select, Tag } from '../../design';
import { injectCss } from '../../design/injectCss';
import {
  ApiError,
  createRun,
  getCoverage,
  listVintages,
  type BacktestParams,
  type CoverageRow,
  type VintageListItem,
} from '../../api/runs';

const CSS = `
.qe-new { max-width: 880px; margin: 0 auto; padding: 24px; display: flex; flex-direction: column; gap: 16px; }
.qe-new__hd h2 { font-family: var(--font-display); font-size: var(--fs-lg); font-weight: 600; }
.qe-new__grid { display: grid; grid-template-columns: 1fr 1fr; gap: 14px; }
.qe-new__row { display: flex; flex-direction: column; gap: 6px; }
.qe-new__lbl { font-family: var(--font-sans); font-size: var(--fs-sm); font-weight: var(--fw-medium); color: var(--text-secondary); }
.qe-new__uni { display: flex; gap: 6px; flex-wrap: wrap; }
.qe-new__chip { cursor: pointer; }
.qe-new__chip--off { opacity: 0.45; }
.qe-new__hint { font-size: var(--fs-caption); color: var(--text-muted); margin-top: 2px; }
.qe-new__actions { display: flex; justify-content: flex-end; gap: 10px; }
`;

injectCss('qe-new-css', CSS);

const DEFAULT_RESOLUTIONS = ['1m', '5m', '1h', '4h', '1d'];
const SLIPPAGE_MODELS = ['square-root-impact', 'linear-impact', 'fixed-bps', 'none'];

export interface NewBacktestProps {
  onCreated: (id: string) => void;
  onCancel: () => void;
  /** A vintage id to preselect (QE-261 training → backtest deep-link). */
  initialVintage?: string;
}

/**
 * New backtest (trigger) form — vintage/window/resolution/universe/costs → `POST /api/runs`.
 * Client-side validation surfaces missing fields; a server 400 is surfaced inline. Genome params are
 * NOT editable here (D1) — the user backtests a sealed vintage, not hand-typed genome params. An
 * `initialVintage` (a QE-261 deep-link) is preselected instead of defaulting to the first vintage.
 */
export function NewBacktest({ onCreated, onCancel, initialVintage }: NewBacktestProps) {
  const [vintages, setVintages] = useState<VintageListItem[]>([]);
  const [coverage, setCoverage] = useState<CoverageRow[]>([]);
  const [loadError, setLoadError] = useState<string | null>(null);

  const [vintage, setVintage] = useState(initialVintage ?? '');
  const [start, setStart] = useState('');
  const [end, setEnd] = useState('');
  const [resolution, setResolution] = useState('1h');
  const [selected, setSelected] = useState<Set<string>>(new Set());
  const [takerFee, setTakerFee] = useState('2.0');
  const [slippage, setSlippage] = useState(SLIPPAGE_MODELS[0]);

  const [fieldError, setFieldError] = useState<string | null>(null);
  const [serverError, setServerError] = useState<string | null>(null);
  const [submitting, setSubmitting] = useState(false);

  useEffect(() => {
    let cancelled = false;
    Promise.all([listVintages(), getCoverage()])
      .then(([vs, cov]) => {
        if (cancelled) return;
        setVintages(vs);
        setCoverage(cov);
        // Preselect the deep-linked vintage when present (and known); else default to the first.
        if (initialVintage && vs.some((v) => v.id === initialVintage)) setVintage(initialVintage);
        else if (vs.length > 0) setVintage(vs[0].id);
        // Default-select every symbol present in the store (the backtestable set).
        setSelected(new Set(cov.map((r) => r.symbol)));
      })
      .catch((e) => {
        if (!cancelled) setLoadError(e instanceof Error ? e.message : 'Failed to load form data.');
      });
    return () => {
      cancelled = true;
    };
  }, [initialVintage]);

  const symbols = useMemo(
    () => Array.from(new Set(coverage.map((r) => r.symbol))).sort(),
    [coverage],
  );
  const resolutions = useMemo(() => {
    const fromCov = Array.from(new Set(coverage.map((r) => r.resolution)));
    return fromCov.length > 0 ? fromCov : DEFAULT_RESOLUTIONS;
  }, [coverage]);

  const toggleSymbol = (sym: string) => {
    setSelected((prev) => {
      const next = new Set(prev);
      if (next.has(sym)) next.delete(sym);
      else next.add(sym);
      return next;
    });
  };

  const validate = (): string | null => {
    if (!vintage) return 'Select a vintage to backtest.';
    if (!start) return 'Choose a window start date.';
    if (!end) return 'Choose a window end date.';
    if (start >= end) return 'The window start must be before the end.';
    if (!resolution) return 'Choose a bar resolution.';
    if (selected.size === 0) return 'Select at least one universe symbol.';
    const fee = Number(takerFee);
    if (!Number.isFinite(fee) || fee < 0) return 'Taker fee must be a non-negative number.';
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
    const params: BacktestParams = {
      vintage,
      start,
      end,
      resolution,
      universe: symbols.filter((s) => selected.has(s)),
      taker_fee_bps: Number(takerFee),
      slippage_model: slippage,
    };
    try {
      const id = await createRun(params);
      onCreated(id);
    } catch (e) {
      setServerError(e instanceof ApiError ? e.message : 'Failed to start the backtest.');
      setSubmitting(false);
    }
  };

  return (
    <div className="qe-new">
      <div className="qe-new__hd">
        <Button variant="ghost" size="sm" onClick={onCancel} iconLeft={<Icon name="arrow-left" size={15} />}>
          All backtests
        </Button>
        <h2 style={{ marginTop: 8 }}>New backtest</h2>
      </div>

      {loadError && (
        <Callout variant="danger" title="Could not load form data">
          {loadError}
        </Callout>
      )}

      <Card title="Run configuration">
        <div style={{ display: 'flex', flexDirection: 'column', gap: 16 }}>
          <div className="qe-new__grid">
            <div className="qe-new__row">
              <label className="qe-new__lbl" htmlFor="qe-new-vintage">
                Vintage
              </label>
              <Select
                id="qe-new-vintage"
                aria-label="Vintage"
                value={vintage}
                onChange={(e) => setVintage(e.target.value)}
                options={vintages.map((v) => ({ value: v.id, label: v.label }))}
              >
                {vintages.length === 0 ? <option value="">No sealed vintages available</option> : undefined}
              </Select>
            </div>
            <div className="qe-new__row">
              <label className="qe-new__lbl" htmlFor="qe-new-res">
                Resolution
              </label>
              <Select
                id="qe-new-res"
                aria-label="Resolution"
                value={resolution}
                onChange={(e) => setResolution(e.target.value)}
                options={resolutions}
              />
            </div>
            <Input
              label="Start"
              type="date"
              value={start}
              onChange={(e) => setStart(e.target.value)}
            />
            <Input label="End" type="date" value={end} onChange={(e) => setEnd(e.target.value)} />
          </div>

          <div className="qe-new__row">
            <span className="qe-new__lbl">
              Universe{' '}
              <Badge variant="neutral">
                {selected.size}/{symbols.length}
              </Badge>
            </span>
            {symbols.length === 0 ? (
              <div style={{ fontSize: 'var(--fs-sm)', color: 'var(--text-muted)' }}>
                No symbols in the market-data store. Ingest data first.
              </div>
            ) : (
              <>
                <div className="qe-new__uni" role="group" aria-label="Universe symbols">
                  {symbols.map((s) => {
                    const on = selected.has(s);
                    return (
                      <Tag
                        key={s}
                        mono
                        className={`qe-new__chip ${on ? '' : 'qe-new__chip--off'}`}
                        role="checkbox"
                        aria-checked={on}
                        aria-label={s}
                        onClick={() => toggleSymbol(s)}
                      >
                        {s}
                      </Tag>
                    );
                  })}
                </div>
                {/* Multi-select kept (matches the kit's universe-as-tags idiom), but the v1 engine
                    simulates only the first selected symbol — hint so users aren't surprised. */}
                <span className="qe-new__hint">
                  v1 backtests the first selected symbol ({symbols.find((s) => selected.has(s)) ?? '—'}).
                </span>
              </>
            )}
          </div>
        </div>
      </Card>

      <Card title="Costs & slippage">
        <div className="qe-new__grid">
          <Input
            label="Taker fee (bps)"
            mono
            value={takerFee}
            onChange={(e) => setTakerFee(e.target.value)}
          />
          <div className="qe-new__row">
            <label className="qe-new__lbl" htmlFor="qe-new-slip">
              Slippage model
            </label>
            <Select
              id="qe-new-slip"
              aria-label="Slippage model"
              value={slippage}
              onChange={(e) => setSlippage(e.target.value)}
              options={SLIPPAGE_MODELS}
            />
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

      <div className="qe-new__actions">
        <Button variant="secondary" onClick={onCancel} disabled={submitting}>
          Cancel
        </Button>
        <Button
          variant="primary"
          loading={submitting}
          onClick={submit}
          iconLeft={<Icon name="play" size={15} />}
        >
          Run backtest
        </Button>
      </div>
    </div>
  );
}
