import { useEffect, useRef, useState } from 'react';
import { Badge, Button, Callout, Card, DataTable, Icon, Input, Pnl, Tabs, Tag } from '../../design';
import type { Column } from '../../design';
import { injectCss } from '../../design/injectCss';
import {
  ApiError,
  createRun,
  getRun,
  getRunResult,
  type BacktestResult as ResultContract,
  type RunMeta,
  type Trade,
} from '../../api/runs';

/* Layout CSS ported verbatim from the Claude Design "Quant Engine Design System"
   (ui_kits/strategy-research/BacktestResearch.jsx), then made data-driven. */
const CSS = `
.qe-bt { padding: 18px; display: grid; grid-template-columns: 1fr 300px; gap: 16px; align-items: start; }
.qe-bt__col { display: flex; flex-direction: column; gap: 16px; min-width: 0; }
.qe-bt__hd { display: flex; align-items: center; justify-content: space-between; gap: 16px; padding: 16px 18px; background: var(--surface-card); border: 1px solid var(--border-subtle); border-radius: var(--radius-lg); }
.qe-bt__title { display: flex; align-items: center; gap: 10px; flex-wrap: wrap; }
.qe-bt__title h2 { font-size: 20px; }
.qe-bt__params { display: flex; gap: 6px; flex-wrap: wrap; margin-top: 8px; }
.qe-metrics { display: grid; grid-template-columns: repeat(6, 1fr); gap: 1px; background: var(--border-subtle); border: 1px solid var(--border-subtle); border-radius: var(--radius-lg); overflow: hidden; }
.qe-metric { background: var(--surface-card); padding: 14px; display: flex; flex-direction: column; gap: 4px; }
.qe-metric .k { font: 500 10px var(--font-mono); text-transform: uppercase; letter-spacing: .08em; color: var(--text-muted); }
.qe-metric .v { font-family: var(--font-mono); font-variant-numeric: tabular-nums; font-size: 20px; font-weight: 600; color: var(--text-primary); }
.qe-heat { display: grid; grid-template-columns: 40px repeat(12, 1fr); gap: 3px; font-family: var(--font-mono); }
.qe-heat__lbl { font-size: 10px; color: var(--text-muted); display: flex; align-items: center; }
.qe-heat__cell { aspect-ratio: 1.4; border-radius: var(--radius-xs); display: flex; align-items: center; justify-content: center; font-size: 9px; color: rgba(255,255,255,0.85); }
.qe-chart2 svg { display: block; width: 100%; }
.qe-side-lbl { font: 500 10px var(--font-mono); text-transform: uppercase; letter-spacing: .08em; color: var(--text-muted); margin-bottom: 8px; }
.qe-run { display: flex; flex-direction: column; gap: 10px; }
.qe-run__bar { height: 6px; background: var(--surface-inset); border-radius: var(--radius-full); overflow: hidden; }
.qe-run__fill { height: 100%; background: var(--accent); transition: width 0.1s linear; }
.qe-bt__back { margin-bottom: 4px; }
`;

injectCss('qe-bt-css', CSS);

const MONTHS = ['J', 'F', 'M', 'A', 'M', 'J', 'J', 'A', 'S', 'O', 'N', 'D'];

/** Poll cadence while a run is queued/running (ms). */
const POLL_MS = 2000;

/** Consecutive poll failures tolerated before giving up with a fatal error (resilience). */
const MAX_POLL_FAILURES = 4;

const MINUS = '−'; // − typographic minus

function pct(v: number, signed: boolean): string {
  const p = v * 100;
  if (v < 0) return `${MINUS}${Math.abs(p).toFixed(1)}%`;
  return signed ? `+${p.toFixed(1)}%` : `${p.toFixed(1)}%`;
}

function num(v: number): string {
  return v.toFixed(2);
}

function heatColor(v: number): string {
  if (v >= 0) {
    const a = Math.min(1, v / 12) * 0.75 + 0.12;
    return `rgba(52,211,153,${a})`;
  }
  const a = Math.min(1, -v / 8) * 0.75 + 0.12;
  return `rgba(255,93,108,${a})`;
}

interface AreaChartProps {
  data: number[];
  height?: number;
  stroke?: string;
  fillId?: string;
  negative?: boolean;
}

/** Inline-SVG area chart (no charting lib / no CDN — CSP-safe). Ported from the kit. */
function AreaChart({
  data,
  height = 150,
  stroke = 'var(--violet-400)',
  fillId = 'qe-bt-eq',
  negative = false,
}: AreaChartProps) {
  const width = 640;
  if (data.length < 2) {
    return (
      <svg viewBox={`0 0 ${width} ${height}`} preserveAspectRatio="none" style={{ height }} className="qe-chart2" />
    );
  }
  const min = Math.min(...data);
  const max = Math.max(...data);
  const range = max - min || 1;
  const pad = 6;
  const stepX = (width - pad * 2) / (data.length - 1);
  const y = (v: number) => pad + (height - pad * 2) * (1 - (v - min) / range);
  const pts = data.map((v, i): [number, number] => [pad + i * stepX, y(v)]);
  const line = pts.map((p, i) => `${i ? 'L' : 'M'}${p[0].toFixed(1)} ${p[1].toFixed(1)}`).join(' ');
  const area = `${line} L${pts[pts.length - 1][0].toFixed(1)} ${height} L${pts[0][0].toFixed(1)} ${height} Z`;
  return (
    <svg viewBox={`0 0 ${width} ${height}`} preserveAspectRatio="none" style={{ height }} className="qe-chart2">
      <defs>
        <linearGradient id={fillId} x1="0" y1="0" x2="0" y2="1">
          <stop offset="0" stopColor={stroke} stopOpacity={negative ? 0.02 : 0.28} />
          <stop offset="1" stopColor={stroke} stopOpacity={negative ? 0.28 : 0} />
        </linearGradient>
      </defs>
      <path d={area} fill={`url(#${fillId})`} />
      <path d={line} fill="none" stroke={stroke} strokeWidth="2" strokeLinejoin="round" />
    </svg>
  );
}

function statusBadge(status: RunMeta['status']) {
  switch (status) {
    case 'succeeded':
      return (
        <Badge variant="up" dot>
          SUCCEEDED
        </Badge>
      );
    case 'running':
      return (
        <Badge variant="info" dot>
          RUNNING
        </Badge>
      );
    case 'queued':
      return (
        <Badge variant="neutral" dot>
          QUEUED
        </Badge>
      );
    case 'failed':
      return (
        <Badge variant="down" dot>
          FAILED
        </Badge>
      );
  }
}

export interface BacktestResultProps {
  runId: string;
  /** Go back to the runs list. */
  onBack: () => void;
  /** Navigate to a freshly cloned run (Re-run). */
  onReRun: (newId: string) => void;
  /** Poll cadence while queued/running (ms). Overridable for tests; defaults to {@link POLL_MS}. */
  pollMs?: number;
}

/**
 * Backtest result — data-driven from `GET /api/runs/:id/result` (§8.1). While the run is
 * queued/running, polls `GET /api/runs/:id` and shows the progress card; on success it renders the
 * full contract; on failure it surfaces the error. Genome params are read-only (D1). "Re-run" clones
 * the run's params into a new POST.
 */
export function BacktestResult({ runId, onBack, onReRun, pollMs = POLL_MS }: BacktestResultProps) {
  const [meta, setMeta] = useState<RunMeta | null>(null);
  const [result, setResult] = useState<ResultContract | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [retrying, setRetrying] = useState(false);
  const [tab, setTab] = useState<'overview' | 'trades' | 'config'>('overview');
  const [rerunning, setRerunning] = useState(false);
  const [rerunError, setRerunError] = useState<string | null>(null);
  const resultFetched = useRef(false);

  useEffect(() => {
    // Reset when the run changes.
    resultFetched.current = false;
    setMeta(null);
    setResult(null);
    setError(null);
    setRetrying(false);
    setTab('overview');

    let cancelled = false;
    let timer: ReturnType<typeof setTimeout> | undefined;
    // Consecutive poll failures (network/fetch). Reset only on a *fully* clean tick, so a run stuck
    // erroring — e.g. a succeeded run whose result endpoint keeps failing — still hits the cap.
    let failures = 0;

    const tick = async () => {
      try {
        const m = await getRun(runId);
        if (cancelled) return;
        setMeta(m);
        if (m.status === 'succeeded') {
          if (!resultFetched.current) {
            const r = await getRunResult(runId);
            if (cancelled) return;
            resultFetched.current = true;
            setResult(r);
          }
          failures = 0;
          setRetrying(false);
          return; // terminal — stop polling
        }
        if (m.status === 'failed') {
          setError(m.error ?? 'The backtest run failed.');
          return; // terminal
        }
        // queued | running — a clean poll: reset the failure streak and keep polling.
        failures = 0;
        setRetrying(false);
        timer = setTimeout(tick, pollMs);
      } catch (e) {
        if (cancelled) return;
        // Resilience: a transient fetch error must not freeze the progress view. Keep polling up to a
        // bounded streak, surfacing a non-fatal "retrying" note; only give up (fatal) past the cap.
        failures += 1;
        if (failures > MAX_POLL_FAILURES) {
          setError(e instanceof Error ? e.message : 'Failed to load the run.');
          return;
        }
        setRetrying(true);
        timer = setTimeout(tick, pollMs);
      }
    };

    void tick();
    return () => {
      cancelled = true;
      if (timer) clearTimeout(timer);
    };
  }, [runId, pollMs]);

  const reRun = async () => {
    if (!meta || rerunning) return;
    setRerunning(true);
    setRerunError(null);
    try {
      const newId = await createRun(meta.params);
      onReRun(newId);
    } catch (e) {
      setRerunError(e instanceof ApiError ? e.message : 'Failed to start the re-run.');
      setRerunning(false);
    }
  };

  const back = (
    <div className="qe-bt__back">
      <Button variant="ghost" size="sm" onClick={onBack} iconLeft={<Icon name="arrow-left" size={15} />}>
        All backtests
      </Button>
    </div>
  );

  const running = meta != null && (meta.status === 'running' || meta.status === 'queued');
  const title = result?.strategy.name ?? meta?.params.vintage ?? runId;

  const tradeCols: Column<Trade & Record<string, unknown>>[] = [
    {
      key: 'id',
      header: 'Trade',
      render: (v) => (
        <span style={{ fontFamily: 'var(--font-mono)', color: 'var(--text-tertiary)' }}>{String(v)}</span>
      ),
    },
    {
      key: 'symbol',
      header: 'Symbol',
      render: (v) => <span style={{ fontFamily: 'var(--font-mono)', fontWeight: 600 }}>{String(v)}</span>,
    },
    {
      key: 'side',
      header: 'Side',
      render: (v) => <Badge variant={v === 'LONG' ? 'up' : 'down'}>{String(v)}</Badge>,
    },
    { key: 'entry', header: 'Entry', align: 'num' },
    { key: 'exit', header: 'Exit', align: 'num' },
    { key: 'hold', header: 'Hold', align: 'num' },
    {
      key: 'return_pct',
      header: 'Return',
      align: 'num',
      render: (v) => <Pnl value={Number(v)} format="percent" />,
    },
    {
      key: 'result',
      header: 'Result',
      render: (v) => (
        <Badge variant={v === 'WIN' ? 'up' : 'down'} dot>
          {String(v)}
        </Badge>
      ),
    },
  ];

  return (
    <div style={{ maxWidth: 'var(--content-max)', margin: '0 auto' }}>
      <div style={{ padding: '18px 18px 0' }}>{back}</div>
      <div className="qe-bt">
        <div className="qe-bt__col">
          <div className="qe-bt__hd">
            <div>
              <div className="qe-bt__title">
                <h2>{title}</h2>
                {meta && statusBadge(meta.status)}
                {result && <Badge variant="neutral">{result.strategy.tags.join(' · ')}</Badge>}
              </div>
              {result && (
                <div className="qe-bt__params" aria-label="Strategy parameters (read-only)">
                  {Object.entries(result.strategy.params).map(([k, v]) => (
                    <Tag key={k} mono>
                      {k}={String(v)}
                    </Tag>
                  ))}
                </div>
              )}
            </div>
            <Button
              variant="primary"
              loading={rerunning}
              disabled={!meta}
              onClick={reRun}
              iconLeft={<Icon name="play" size={15} />}
            >
              {rerunning ? 'Starting…' : 'Re-run backtest'}
            </Button>
          </div>

          {rerunError && (
            <Callout variant="danger" title="Re-run failed">
              {rerunError}
            </Callout>
          )}

          {retrying && !error && (
            <Callout variant="warn" title="Connection issue">
              Couldn’t reach the server — retrying…
            </Callout>
          )}

          {running && meta && (
            <Card>
              <div className="qe-run">
                <div
                  style={{
                    display: 'flex',
                    justifyContent: 'space-between',
                    fontFamily: 'var(--font-mono)',
                    fontSize: 12,
                    color: 'var(--text-secondary)',
                  }}
                >
                  <span>{meta.progress.msg || `${meta.status}…`}</span>
                  <span>{`${meta.progress.pct}%`}</span>
                </div>
                <div className="qe-run__bar">
                  <div
                    className="qe-run__fill"
                    role="progressbar"
                    aria-valuenow={meta.progress.pct}
                    aria-valuemin={0}
                    aria-valuemax={100}
                    style={{ width: `${meta.progress.pct}%` }}
                  />
                </div>
              </div>
            </Card>
          )}

          {error && (
            <Callout variant="danger" title="Backtest failed">
              {error}
            </Callout>
          )}

          {result && (
            <>
              <div className="qe-metrics">
                {(
                  [
                    ['CAGR', pct(result.metrics.cagr, true)],
                    ['Sharpe', num(result.metrics.sharpe)],
                    ['Sortino', num(result.metrics.sortino)],
                    ['Max DD', pct(result.metrics.max_dd, true)],
                    ['Win rate', pct(result.metrics.win_rate, false)],
                    ['Profit factor', num(result.metrics.profit_factor)],
                  ] as const
                ).map(([k, v]) => (
                  <div className="qe-metric" key={k}>
                    <span className="k">{k}</span>
                    <span className="v" style={v.startsWith(MINUS) ? { color: 'var(--down-500)' } : undefined}>
                      {v}
                    </span>
                  </div>
                ))}
              </div>

              <Card>
                <div style={{ padding: '10px 16px 0' }}>
                  <Tabs
                    tabs={(['overview', 'trades', 'config'] as const).map((v) => ({
                      value: v,
                      label: v[0].toUpperCase() + v.slice(1),
                    }))}
                    value={tab}
                    onChange={(v) => setTab(v as typeof tab)}
                  />
                </div>
                <div style={{ padding: 16 }}>
                  {tab === 'overview' && (
                    <div style={{ display: 'flex', flexDirection: 'column', gap: 18 }}>
                      <div>
                        <div className="qe-side-lbl">Equity curve (log)</div>
                        <AreaChart data={result.equity_curve} height={150} />
                      </div>
                      <div>
                        <div className="qe-side-lbl">Drawdown</div>
                        <AreaChart
                          data={result.drawdown}
                          height={90}
                          stroke="var(--down-500)"
                          fillId="qe-bt-dd"
                          negative
                        />
                      </div>
                      <div>
                        <div className="qe-side-lbl">Monthly returns %</div>
                        <div style={{ display: 'flex', flexDirection: 'column', gap: 3 }}>
                          <div className="qe-heat">
                            <span />
                            {MONTHS.map((m, i) => (
                              <div key={i} className="qe-heat__lbl" style={{ justifyContent: 'center' }}>
                                {m}
                              </div>
                            ))}
                          </div>
                          {result.monthly_returns.map((row) => (
                            <div className="qe-heat" key={row.year}>
                              <span className="qe-heat__lbl">{row.year}</span>
                              {row.months.map((v, ci) => (
                                <div
                                  key={ci}
                                  className="qe-heat__cell"
                                  style={{ background: heatColor(v) }}
                                  title={`${v}%`}
                                >
                                  {v > 0 ? '+' : ''}
                                  {v}
                                </div>
                              ))}
                            </div>
                          ))}
                        </div>
                      </div>
                    </div>
                  )}
                  {tab === 'trades' && (
                    <div style={{ margin: -16 }}>
                      <DataTable columns={tradeCols} rows={result.trades as (Trade & Record<string, unknown>)[]} keyField="id" />
                    </div>
                  )}
                  {tab === 'config' && (
                    <div style={{ display: 'grid', gridTemplateColumns: '1fr 1fr', gap: 14 }}>
                      <Input label="Start" defaultValue={result.window.start} readOnly disabled />
                      <Input label="End" defaultValue={result.window.end} readOnly disabled />
                      <Input label="Resolution" defaultValue={result.window.resolution} readOnly disabled />
                      <Input label="Universe size" mono defaultValue={String(result.universe.count)} readOnly disabled />
                      <Input
                        label="Taker fee (bps)"
                        mono
                        defaultValue={String(result.costs.taker_fee_bps)}
                        readOnly
                        disabled
                      />
                      <Input label="Slippage model" defaultValue={result.costs.slippage_model} readOnly disabled />
                    </div>
                  )}
                </div>
              </Card>
            </>
          )}
        </div>

        <div className="qe-bt__col">
          <Card title="Run configuration">
            <div style={{ display: 'flex', flexDirection: 'column', gap: 12 }}>
              <div>
                <div className="qe-side-lbl">Universe</div>
                <div style={{ display: 'flex', gap: 6, flexWrap: 'wrap' }}>
                  {(result?.universe.symbols ?? meta?.params.universe ?? []).slice(0, 6).map((s) => (
                    <Tag key={s} mono>
                      {s}
                    </Tag>
                  ))}
                  {(() => {
                    const syms = result?.universe.symbols ?? meta?.params.universe ?? [];
                    return syms.length > 6 ? <Tag mono>+{syms.length - 6}</Tag> : null;
                  })()}
                </div>
              </div>
              <Input label="Resolution" defaultValue={result?.window.resolution ?? meta?.params.resolution ?? ''} readOnly disabled />
              <Input label="Start" defaultValue={result?.window.start ?? meta?.params.start ?? ''} readOnly disabled />
              <Input label="End" defaultValue={result?.window.end ?? meta?.params.end ?? ''} readOnly disabled />
            </div>
          </Card>
          <Callout variant="accent" title="Point-in-time data">
            Universe reconstructed with delisted symbols to avoid survivorship bias.
          </Callout>
          <Card title="Costs & slippage">
            <div style={{ display: 'flex', flexDirection: 'column', gap: 12 }}>
              <Input
                label="Taker fee (bps)"
                mono
                defaultValue={String(result?.costs.taker_fee_bps ?? meta?.params.taker_fee_bps ?? '')}
                readOnly
                disabled
              />
              <Input
                label="Slippage model"
                defaultValue={result?.costs.slippage_model ?? meta?.params.slippage_model ?? ''}
                readOnly
                disabled
              />
            </div>
          </Card>
        </div>
      </div>
    </div>
  );
}
