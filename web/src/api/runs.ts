/*
 * Runs / vintages / market-data API client — the QE-259 backtest screens talk to
 * the session-gated, same-origin qe-server (QE-255 run lifecycle, QE-257 read APIs).
 *
 *   GET  /api/runs                 → RunMeta[] (newest first)
 *   POST /api/runs                 → 201 { id } | 400 { error }
 *   GET  /api/runs/:id             → RunMeta (status + progress) | 404
 *   GET  /api/runs/:id/result      → BacktestResult (§8.1) | 409 (not ready) | 404
 *   GET  /api/vintages             → VintageListItem[]
 *   GET  /api/market-data/coverage → CoverageRow[]
 */

export type RunStatus = 'queued' | 'running' | 'succeeded' | 'failed';

/** Latest progress update tailed from the subprocess stdout (spec §5.3). */
export interface Progress {
  pct: number;
  stage: string;
  msg: string;
}

/** Backtest parameters — the `params` object of a create-run request (QE-255 model). */
export interface BacktestParams {
  vintage: string;
  strategy?: string;
  start: string;
  end: string;
  resolution: string;
  universe: string[];
  taker_fee_bps: number;
  slippage_model: string;
}

/** A run's `meta.json` — the authoritative status + progress record (§6.1 / §8.2). */
export interface RunMeta {
  id: string;
  type: string;
  status: RunStatus;
  params: BacktestParams;
  progress: Progress;
  created_ms: number;
  started_ms: number | null;
  finished_ms: number | null;
  exit: number | null;
  error: string | null;
  artifacts: string[];
}

/** One trade row of the §8.1 result contract. */
export interface Trade {
  id: string;
  symbol: string;
  side: string;
  entry: string;
  exit: string;
  hold: string;
  return_pct: number;
  result: string;
}

/** One year's monthly returns (12 floats, %) for the heatmap. */
export interface MonthlyReturns {
  year: number;
  months: number[];
}

/** The full backtest result contract (`result.json`, spec §8.1). */
export interface BacktestResult {
  strategy: {
    name: string;
    status: string;
    tags: string[];
    /** Read-only genome params → header tags (decision D1). */
    params: Record<string, unknown>;
  };
  window: { start: string; end: string; resolution: string };
  universe: { symbols: string[]; count: number };
  costs: { taker_fee_bps: number; slippage_model: string };
  metrics: {
    cagr: number;
    sharpe: number;
    sortino: number;
    max_dd: number;
    win_rate: number;
    profit_factor: number;
  };
  equity_curve: number[];
  drawdown: number[];
  monthly_returns: MonthlyReturns[];
  trades: Trade[];
}

/** Per-vintage summary carried in a {@link VintageListItem} (QE-257). */
export interface VintageSummary {
  chromosomes: number;
  content_hash: string;
  worst_case_loss: number | null;
  format_version: number;
}

/** One selectable vintage from `GET /api/vintages`. */
export interface VintageListItem {
  id: string;
  label: string;
  summary: VintageSummary;
}

/** One market-data coverage row from `GET /api/market-data/coverage` (QE-257). */
export interface CoverageRow {
  symbol: string;
  resolution: string;
  /** Earliest stored bar open_time, epoch-ms (inclusive). */
  from: number;
  /** Latest stored bar open_time, epoch-ms (inclusive). */
  to: number;
  bars: number;
}

/** An API error carrying the HTTP status and the server's `{ error }` message when present. */
export class ApiError extends Error {
  constructor(
    public status: number,
    message: string,
  ) {
    super(message);
    this.name = 'ApiError';
  }
}

const JSON_HEADERS = { Accept: 'application/json' } as const;

async function errorMessage(res: Response): Promise<string> {
  try {
    const body = (await res.json()) as { error?: unknown };
    if (body && typeof body.error === 'string') return body.error;
  } catch {
    // non-JSON body — fall through to the generic message
  }
  return `request failed: ${res.status}`;
}

async function getJson<T>(url: string): Promise<T> {
  const res = await fetch(url, { credentials: 'same-origin', headers: JSON_HEADERS });
  if (!res.ok) throw new ApiError(res.status, await errorMessage(res));
  return (await res.json()) as T;
}

/** List all runs, newest first. */
export function listRuns(): Promise<RunMeta[]> {
  return getJson<RunMeta[]>('/api/runs');
}

/** Fetch one run's meta (status + progress). */
export function getRun(id: string): Promise<RunMeta> {
  return getJson<RunMeta>(`/api/runs/${encodeURIComponent(id)}`);
}

/** Fetch a succeeded run's full result contract. */
export function getRunResult(id: string): Promise<BacktestResult> {
  return getJson<BacktestResult>(`/api/runs/${encodeURIComponent(id)}/result`);
}

/** List the sealed vintages available to backtest. */
export function listVintages(): Promise<VintageListItem[]> {
  return getJson<VintageListItem[]>('/api/vintages');
}

/** Read-only market-data coverage (symbols × ranges present in the store). */
export function getCoverage(): Promise<CoverageRow[]> {
  return getJson<CoverageRow[]>('/api/market-data/coverage');
}

/** Create + spawn a backtest run. Resolves to the new run id; throws {@link ApiError} on a 400. */
export async function createRun(params: BacktestParams): Promise<string> {
  const res = await fetch('/api/runs', {
    method: 'POST',
    credentials: 'same-origin',
    headers: { ...JSON_HEADERS, 'Content-Type': 'application/json' },
    body: JSON.stringify({ type: 'backtest', params }),
  });
  if (!res.ok) throw new ApiError(res.status, await errorMessage(res));
  const body = (await res.json()) as { id: string };
  return body.id;
}
