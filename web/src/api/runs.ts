/*
 * Runs / vintages / market-data API client — the QE-259 backtest screens talk to
 * the session-gated, same-origin qe-server (QE-255 run lifecycle, QE-257 read APIs).
 *
 *   GET  /api/runs                 → { runs: RunListItem[], next_cursor } (QE-410 slim page, newest
 *                                    first; wire `next_cursor` is mapped to `nextCursor` client-side)
 *   POST /api/runs                 → 201 { id } | 400 { error }
 *   GET  /api/runs/:id             → RunMeta (status + progress) | 404
 *   GET  /api/runs/:id/result      → BacktestResult (§8.1) | 409 (not ready) | 404
 *   GET  /api/vintages             → VintageListItem[]
 *   GET  /api/market-data/coverage → CoverageRow[]
 */

import { emitUnauthorized } from './authEvents';

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

/** Campaign mode of an `evolve` run (design §13.6). Mirrors `qe_run_protocol::EvolveMode`. */
export type EvolveMode = 'sandbox' | 'production';

/**
 * Evolve-campaign parameters (QE-452) — the `params` object of a `type:"evolve"` create-run request.
 * Kept **hand-in-lockstep** with `crates/run-protocol/src/lib.rs::EvolveParams` (the source of truth).
 * **`seed` is REQUIRED** (diverges from {@link TrainParams}' optional seed — an evolve approval must stay
 * byte-reproducible off the recorded seed). The caps (`depth≤4`, `nodes≤16`, `lookback≤200`, `k≤16`,
 * `windows ⊆ {5,10,20,50,100}`) are enforced client-side (mirroring `validate_evolve`) and re-enforced
 * server-side as a uniform `400`.
 */
export interface EvolveParams {
  /** Master illumination seed (**required**). */
  seed: number;
  /** Campaign mode — `sandbox` (research) or `production` (gated on QE-454 prereqs). */
  mode: EvolveMode;
  start: string;
  end: string;
  resolution: string;
  /** Illumination generations; omitted ⇒ the CLI default. */
  generations?: number;
  /** Offspring evaluated per generation; omitted ⇒ the CLI default. */
  offspring?: number;
  /** Quantiser state count for the trivial decision head. */
  states?: number;
  /** Declared max tree depth (≤ 4). */
  depth?: number;
  /** Declared max tree node count (≤ 16). */
  nodes?: number;
  /** Declared max indicator lookback in bars (≤ 200). */
  lookback?: number;
  /** Declared window-length lattice (each entry ∈ {5,10,20,50,100}). */
  windows?: number[];
  /** Frozen-pool size `K` (≤ 16). */
  k?: number;
  config?: string;
  profile?: string;
}

/** Training parameters — the `params` object of a `type:"train"` create-run request (QE-261). */
export interface TrainParams {
  start: string;
  end: string;
  resolution: string;
  seed?: number;
  generations?: number;
  population?: number;
  holdout?: number;
  embargo?: number;
  config?: string;
  profile?: string;
}

/** Latest MAP-Elites search-generation snapshot (QE-260 `gen` line). */
export interface GenSnapshot {
  generation: number;
  generations: number;
  coverage: number;
  coverage_long: number;
  coverage_short: number;
  /** Best-so-far archive fitness; `null` while still −∞ (before any accepted elite). */
  best_fitness: number | null;
}

/** Latest ensemble-construction snapshot (QE-260 `ensemble` line). */
export interface EnsembleSnapshot {
  folds: number;
  members: number;
  score: number | null;
}

/** The G1 gate verdict snapshot (QE-260/QE-134 `gate` line). */
export interface GateSnapshot {
  promoted: boolean;
  failed: string[];
  in_sample_sharpe: number | null;
  holdout_sharpe: number | null;
  dsr: number | null;
  spa_pvalue: number | null;
  n_trials: number;
}

/** Rich training progress a `train` run exposes for polling (QE-261) — latest of each kind. */
export interface TrainProgress {
  generation?: GenSnapshot;
  ensemble?: EnsembleSnapshot;
  gate?: GateSnapshot;
  /** The sealed vintage id from the terminal `done` (the deep-link target). */
  vintage?: string;
}

/**
 * The status/progress fields common to every run's `meta.json` (§6.1 / §8.2), independent of the run
 * `type`. The `type`-specific `params`/`train` fields are added by the {@link RunMeta} union variants.
 */
export interface RunMetaBase {
  id: string;
  status: RunStatus;
  progress: Progress;
  created_ms: number;
  started_ms: number | null;
  finished_ms: number | null;
  exit: number | null;
  error: string | null;
  artifacts: string[];
}

/**
 * A run's `meta.json` — the authoritative status + progress record — as a **discriminated union on
 * `type`** (QE-406). This mirrors the Rust wire contract: `type` is `qe_server::runs::RunMeta.type`
 * (`backtest`/`train`), and `params` is the matching `qe_run_protocol::{BacktestParams, TrainParams}`
 * wire DTO. Kept **hand-in-lockstep** with `crates/run-protocol/src/lib.rs` (the source of truth) —
 * update both together. Narrow on `meta.type` at each consumer instead of casting, so a train run can
 * no longer be statically read as a backtest (and vice-versa).
 */
export type RunMeta = BacktestRunMeta | TrainRunMeta | EvolveRunMeta;

/** A `type:"backtest"` run — its `params` is a {@link BacktestParams}. */
export interface BacktestRunMeta extends RunMetaBase {
  type: 'backtest';
  params: BacktestParams;
}

/** A `type:"evolve"` run (QE-452) — its `params` is an {@link EvolveParams}. */
export interface EvolveRunMeta extends RunMetaBase {
  type: 'evolve';
  params: EvolveParams;
}

/** A `type:"train"` run (QE-261) — {@link TrainParams} + the rich {@link TrainProgress} for polling. */
export interface TrainRunMeta extends RunMetaBase {
  type: 'train';
  params: TrainParams;
  /** Rich training progress — present only on `train` runs (QE-261). */
  train?: TrainProgress;
}

/** Type-predicate narrowing a {@link RunMeta} to the `train` variant (for `.filter(isTrainRun)`). */
export function isTrainRun(run: RunMeta): run is TrainRunMeta {
  return run.type === 'train';
}

/** Type-predicate narrowing a {@link RunMeta} to the `backtest` variant (for `.filter(isBacktestRun)`). */
export function isBacktestRun(run: RunMeta): run is BacktestRunMeta {
  return run.type === 'backtest';
}

/** Type-predicate narrowing a {@link RunMeta} to the `evolve` variant (for `.filter(isEvolveRun)`). */
export function isEvolveRun(run: RunMeta): run is EvolveRunMeta {
  return run.type === 'evolve';
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

/**
 * A **401 Unauthorized** from any session-gated endpoint (QE-409) — an expired/cleared session
 * mid-session. Subclasses {@link ApiError} (status is fixed to `401`) so every existing
 * `instanceof ApiError` consumer keeps working, while pollers/screens can narrow with
 * `instanceof UnauthorizedError` to treat it as **terminal-auth** (stop, don't retry, don't render a
 * "failed" surface). Whenever one is thrown the API client also emits the app-level `unauthorized`
 * signal (see {@link emitUnauthorized}), so the shell flips back to `Login` without a reload.
 */
export class UnauthorizedError extends ApiError {
  constructor(message: string) {
    super(401, message);
    this.name = 'UnauthorizedError';
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

/**
 * Turn a non-OK {@link Response} into the right thrown error. A **401** becomes an
 * {@link UnauthorizedError} *and* fires the app-level `unauthorized` signal here — the single choke
 * point so **any** 401 (list/run polling, coverage, vintages, create) flips the shell exactly once.
 * Every other status becomes a generic {@link ApiError}.
 */
async function throwForResponse(res: Response): Promise<never> {
  const message = await errorMessage(res);
  if (res.status === 401) {
    emitUnauthorized();
    throw new UnauthorizedError(message);
  }
  throw new ApiError(res.status, message);
}

async function getJson<T>(url: string): Promise<T> {
  const res = await fetch(url, { credentials: 'same-origin', headers: JSON_HEADERS });
  if (!res.ok) await throwForResponse(res);
  return (await res.json()) as T;
}

/** A run's `type` discriminant (`'backtest' | 'train'`). */
export type RunType = RunMeta['type'];

/**
 * The **slim** list projection (QE-410) — one row of `GET /api/runs`, deliberately smaller than the
 * full {@link RunMeta} served by `GET /api/runs/:id`. Carries only what the run lists render live:
 * identity (`id`/`type`/`label`), lifecycle (`status`/`progress`), the small live training progress
 * (`train`), and `created_ms`. The heavy immutable `params` is **deferred** to the detail endpoint —
 * open a run (`getRun`) to read its full {@link RunMeta} (with `params`).
 *
 * `label` is the server's index label — the vintage id (backtest) or `"train {start}→{end}"` (train) —
 * so a list has an identifying column without shipping `params`.
 */
export interface RunListItem {
  id: string;
  type: RunType;
  /** Human discovery label (vintage id / window). */
  label: string;
  status: RunStatus;
  progress: Progress;
  /** Rich training progress — present only on `train` rows. */
  train?: TrainProgress;
  created_ms: number;
}

/** Query for {@link listRuns}: every field is optional and they compose server-side. */
export interface RunListQuery {
  /** Filter to a single run type (`?type=`, QE-408). */
  type?: RunType;
  /** Filter to a single lifecycle status (`?status=`). */
  status?: RunStatus;
  /** Page size (`?limit=`); the server defaults to 50 and caps at 200. */
  limit?: number;
  /** Pagination cursor (`?cursor=`) — the run id after which (older than which) to resume. */
  cursor?: string;
}

/** One page of {@link listRuns}: the newest-first slim rows plus the cursor for the next (older) page. */
export interface RunPage {
  runs: RunListItem[];
  /** Cursor to pass as `RunListQuery.cursor` for the next page, or `null` when this is the last page. */
  nextCursor: string | null;
}

/** The on-wire `GET /api/runs` envelope (snake-case `next_cursor`), mapped to {@link RunPage}. */
interface RunPageWire {
  runs: RunListItem[];
  next_cursor: string | null;
}

/**
 * List runs, newest first, as a paginated page of the slim {@link RunListItem} projection (QE-410).
 * Every {@link RunListQuery} field is optional and composes: `type`/`status` filter, `limit`/`cursor`
 * paginate (the cursor is id-anchored and stable under concurrent creates). Callers that need a run's
 * full `params` open it with {@link getRun}.
 */
export async function listRuns(query: RunListQuery = {}): Promise<RunPage> {
  const params = new URLSearchParams();
  if (query.type) params.set('type', query.type);
  if (query.status) params.set('status', query.status);
  // Only forward a positive limit; a non-positive value is nonsensical on the wire (the server would
  // clamp it to 1 anyway), so omit it and let the server default apply.
  if (query.limit != null && query.limit > 0) params.set('limit', String(query.limit));
  if (query.cursor) params.set('cursor', query.cursor);
  const qs = params.toString();
  const wire = await getJson<RunPageWire>(qs ? `/api/runs?${qs}` : '/api/runs');
  return { runs: wire.runs, nextCursor: wire.next_cursor };
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

/** POST a create-run request and resolve to the new run id; throws {@link ApiError} on a 400. */
async function postRun(type: string, params: unknown): Promise<string> {
  const res = await fetch('/api/runs', {
    method: 'POST',
    credentials: 'same-origin',
    headers: { ...JSON_HEADERS, 'Content-Type': 'application/json' },
    body: JSON.stringify({ type, params }),
  });
  if (!res.ok) await throwForResponse(res);
  const body = (await res.json()) as { id: string };
  return body.id;
}

/** Create + spawn a backtest run. Resolves to the new run id; throws {@link ApiError} on a 400. */
export function createRun(params: BacktestParams): Promise<string> {
  return postRun('backtest', params);
}

/** Create + spawn a training run (QE-261). Resolves to the new run id; throws {@link ApiError} on 400. */
export function createTrainRun(params: TrainParams): Promise<string> {
  return postRun('train', params);
}

/**
 * Create + spawn an `evolve` campaign (QE-452). Resolves to the new run id; throws {@link ApiError} on a
 * `400` — including a **production launch refused** by the compiled prereq const (surfaced honestly, not
 * hidden). Client-side {@link EvolveParams} validation mirrors the server's `validate_evolve` caps.
 */
export function createEvolveRun(params: EvolveParams): Promise<string> {
  return postRun('evolve', params);
}

// ---- formula-pool + evolve-archive types (QE-452 Phase B wire; kept in lockstep with `pools.rs`) ------

/** A pool's durable governance lifecycle (design §13.3). Wire tokens are snake_case. */
export type PoolLifecycleState = 'draft' | 'approved' | 'sealed' | 'rejected' | 'revoked';

/** The campaign mode a pool was produced under. */
export type PoolMode = 'sandbox' | 'production';

/** A governance action against a pool (the four `POST` transitions). */
export type PoolTransition = 'approve' | 'reject' | 'revoke' | 'seal';

/** One frozen formula: its canonical S-expression + the content-addressed `formula_hash`. */
export interface PoolFormula {
  sexpr: string;
  formula_hash: string;
}

/**
 * The deflation-summary block (design §5/§13.5) — the minimum honest stat set the PoolReview gate renders.
 * The `Decimal` bars arrive as **strings** (byte-stable hashing), so parse for display, never for math.
 */
export interface DeflationSummary {
  /** Whether the trial basis came from the real GP-aware trial counter (QE-439). */
  gp_aware: boolean;
  /** Distinct-canonical formulas evaluated (incl. rejects) — the QE-439 basis. */
  distinct_evaluations: number;
  /** The trial basis `N` (= `max(distinct, analytic floor)`) the DSR deflated against. */
  n_trials: number;
  /** The analytic `cells·gens·windows` floor (`N == floor` is the "QE-439 not wired" tell). */
  analytic_floor: number;
  /** Size of the uncensored Sharpe-dispersion population. */
  variance_trials: number;
  /** Cross-trial Sharpe variance over the uncensored population (string Decimal). */
  trial_variance: string;
  /** The best-of-`N` noise Sharpe bar `E[max SR]` (string Decimal; finite via the log-N path). */
  expected_max_sharpe: string;
  /** The champion's Deflated Sharpe Ratio (string Decimal; necessary-not-sufficient floor). */
  champion_dsr: string;
  /** Uncensored PBO over the full population (string Decimal), or `null` if unestimable. */
  uncensored_pbo: string | null;
}

/** The pool's review lineage (design §13.10) — the reproducible provenance binding an approval. */
export interface PoolLineage {
  campaign_id: string;
  seed: number;
  mode: PoolMode;
  code_commit: string;
  input_snapshot_id: string;
  config_hash: string;
  pool_hash: string;
}

/** The hashed content of a formula pool (`GET /api/formula-pools/{id}`'s `content`). */
export interface FormulaPoolContent {
  format_version: number;
  pool_id: string;
  mode: PoolMode;
  formulas: PoolFormula[];
  deflation: DeflationSummary;
  lineage: PoolLineage;
}

/** One appended governance event (the QE-454 audit-entry placeholder). */
export interface TransitionRecord {
  transition: PoolTransition;
  actor: string;
  ts_ms: number;
  from: PoolLifecycleState;
  to: PoolLifecycleState;
}

/** A pool list row — the slim summary the PoolBrowser renders (`GET /api/formula-pools`). */
export interface PoolSummary {
  id: string;
  mode: PoolMode;
  content_hash: string;
  pool_hash: string;
  formula_count: number;
  gp_aware: boolean;
  distinct_evaluations: number;
  lifecycle: PoolLifecycleState;
}

/** The pool detail view (`GET /api/formula-pools/{id}`) the PoolReview gate consumes. */
export interface PoolDetail {
  content: FormulaPoolContent;
  content_hash: string;
  lifecycle: PoolLifecycleState;
  history: TransitionRecord[];
}

/** The result of a successful governance transition (`200 {pool_id,lifecycle}`). */
export interface TransitionResult {
  pool_id: string;
  lifecycle: PoolLifecycleState;
}

/** One occupied MAP-Elites niche of an evolve run's archive (`GET /api/runs/{id}/archive` `cells`). */
export interface ArchiveCell {
  family: string;
  timescale: string;
  complexity: string;
  node_count: number;
  best_fitness: number | null;
}

/** The GP-aware trial-count basis the CampaignMonitor's TrialCountBar renders. */
export interface ArchiveTrialBasis {
  distinct_evaluations: number;
  n_trials: number;
  analytic_floor: number;
  expected_max_sharpe: number | null;
  occupied_cells: number;
  total_cells: number;
}

/** The `archive.json` snapshot an evolve run writes (`GET /api/runs/{id}/archive`). */
export interface EvolveArchive {
  pool_id: string;
  mode: string;
  generations: number;
  offspring: number;
  cells: ArchiveCell[];
  trial_basis: ArchiveTrialBasis;
}

// ---- formula-pool + evolve-archive API (session-gated reads + role-gated governance) ------------------

/** List the frozen formula pools (both roots), each hash-verified server-side. */
export function listFormulaPools(): Promise<PoolSummary[]> {
  return getJson<PoolSummary[]>('/api/formula-pools');
}

/** Fetch one pool's verified detail (K formulas + deflation + lineage + lifecycle); `404` → throws. */
export function getFormulaPool(id: string): Promise<PoolDetail> {
  return getJson<PoolDetail>(`/api/formula-pools/${encodeURIComponent(id)}`);
}

/** Fetch an evolve run's MAP-Elites archive snapshot; `404` (no archive yet) → throws {@link ApiError}. */
export function getRunArchive(id: string): Promise<EvolveArchive> {
  return getJson<EvolveArchive>(`/api/runs/${encodeURIComponent(id)}/archive`);
}

/**
 * POST a governance transition. Resolves to the new lifecycle on `200`; **throws {@link ApiError}** on a
 * `409` (a production Seal gated on QE-454, or an illegal edge), `404`, or `403` (role-less) — the caller
 * surfaces the server's message honestly and re-reads the pool to reflect the true (unchanged) lifecycle.
 */
export async function postPoolTransition(
  id: string,
  transition: PoolTransition,
): Promise<TransitionResult> {
  const res = await fetch(`/api/formula-pools/${encodeURIComponent(id)}/${transition}`, {
    method: 'POST',
    credentials: 'same-origin',
    headers: JSON_HEADERS,
  });
  if (!res.ok) await throwForResponse(res);
  return (await res.json()) as TransitionResult;
}

/** The outcome of `POST /api/runs/{id}/halt`. */
export interface HaltResult {
  id: string;
  status: RunStatus;
  halted: boolean;
}

/** Cooperatively halt a running evolve campaign; throws {@link ApiError} on `404`/`409` (already terminal). */
export async function haltRun(id: string): Promise<HaltResult> {
  const res = await fetch(`/api/runs/${encodeURIComponent(id)}/halt`, {
    method: 'POST',
    credentials: 'same-origin',
    headers: JSON_HEADERS,
  });
  if (!res.ok) await throwForResponse(res);
  return (await res.json()) as HaltResult;
}
