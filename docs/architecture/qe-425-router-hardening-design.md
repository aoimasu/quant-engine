# QE-425 — Harden the axum router: request timeout, body cap, concurrency limit

**Ticket:** QE-425 (`Phase: PreP3` · `Area: backend / robustness` · `Effort: S`)
**Spec of record:** `### QE-425` in `docs/reviews/2026-07-15-team-improvement-review.md`.
**Crate:** `crates/server` (the QE-254 admin-UI backend; a second, async composition root).

## Current gaps

The `/api` router in `crates/server/src/lib.rs::build_router` composes routing + JSON + a QE-413
tracing stack (`SetRequestId` → `TraceLayer` → `PropagateRequestId`) but has **no transport-layer
robustness backstop**:

- **No per-request timeout.** A slow/stuck handler — a blocking `std::fs` read behind
  `spawn_blocking`, the coverage scan over the market store, a wedged Google `tokeninfo` call — has
  no server-side deadline. It ties up a connection indefinitely.
- **No explicit body-size limit on `POST /api/runs`.** axum applies a default 2 MiB request-body cap,
  but the run-spec JSON (window + resolution + universe + costs + strategy config) is a few KB at
  most; 2 MiB is far larger than this endpoint should ever accept.
- **No concurrency backstop independent of the run pool.** The `RunManager` semaphore
  (`max_concurrency`) bounds *subprocess* runs, but nothing bounds concurrent HTTP work generally.

## Layers added and where applied

All changes are **transport-layer only**; no handler logic is refactored.

### 1. Per-request timeout — `tower_http::timeout::TimeoutLayer`

- **Where:** applied to the `/api` **handler** routes only — the merged public auth routes
  (`/auth/*`) and the protected subtree (`/me`, `/runs*`, read APIs) — via a `hardened` sub-router.
  `GET /api/health` is routed **outside** the timeout layer, and the static/SPA `ServeDir` lives on
  the outer router (never under `/api`), so **neither health nor static is affected**.
- **Value:** `API_REQUEST_TIMEOUT = 30s` (a named `const` with rationale). 30s comfortably exceeds
  every legitimate handler (fs reads, a coverage scan, an OAuth token exchange) while still bounding
  a wedged request. Kept as a const (not config-driven): the server has no per-request tuning knob
  today and adding one for a single value is disproportionate for Effort S.
- **Status produced:** tower-http's `TimeoutLayer` short-circuits an over-deadline request with a
  clean **`408 Request Timeout`** (empty body), infallibly — no `HandleError` / error mapping needed.
- **Feature added:** `tower-http` feature `timeout` (already-present crate). It pulls only
  `http-body` (already a transitive dep) and `tokio/time`; **no new crates enter the lockfile**, so
  `cargo deny` is unaffected.

### 2. Body cap — `axum::extract::DefaultBodyLimit`

- **Where:** applied to the run-lifecycle routes in `crates/server/src/runs/api.rs::routes()`. The
  only body-carrying route there is `POST /api/runs`; the sibling GETs are bodyless, so the layer is
  a no-op for them.
- **Value:** `RUN_SPEC_BODY_LIMIT = 256 KiB`. A run-spec JSON is a few KB in practice; 256 KiB leaves
  ~20–50× headroom for a pathologically large universe array while rejecting any multi-MB body far
  below axum's 2 MiB default. Within the spec's suggested 64 KiB–1 MiB band.
- **Status produced:** axum's request-body-limit returns **`413 Payload Too Large`** when the body
  exceeds the cap (checked against `Content-Length` and the streamed length), before the handler runs.
- **Elsewhere:** other `/api` routes keep axum's built-in 2 MiB default (a sensible global backstop).

### 3. Global concurrency limit / load-shed — **DEFERRED** (documented)

Deferred deliberately (the spec marks it optional: "add it only if it composes cleanly … else
document why deferred"). Rationale:

- The **only** expensive / long-lived work is subprocess runs, already bounded by the `RunManager`
  semaphore (`max_concurrency`), which surfaces back-pressure honestly (`queued`, and `503` once
  shutting down). A second global limit would **double-limit confusingly** alongside it.
- A plain `tower::ConcurrencyLimit` **queues** rather than sheds, so it never returns `503`. To emit
  `503` you must add `tower::LoadShed` **plus** an `axum`/`tower` `HandleError` to map the shed error
  to a status — two extra `tower` features and new error-mapping surface, disproportionate for
  Effort S.
- The new **per-request timeout** already backstops the "stuck handler" failure mode this ticket is
  chiefly about.

If a global limit is wanted later, the clean shape is `LoadShedLayer` + `ConcurrencyLimitLayer` on
the `hardened` sub-router with a `HandleError` mapping `Overloaded` → `503`.

## 408 / 413 / 503 mapping summary

| Condition | Layer | Status | How produced |
|---|---|---|---|
| Handler exceeds the deadline | `tower_http::timeout::TimeoutLayer` (30s) on `/api` handlers | **408** | Layer short-circuits with a 408 response (infallible; no error mapping) |
| `POST /api/runs` body over cap | `axum DefaultBodyLimit::max(256 KiB)` on the runs routes | **413** | axum request-body-limit rejects before the handler |
| Overload / load-shed | *deferred* | (503) | Would be `LoadShed` + `HandleError`; not added this ticket |

## Why health / static are excluded

- **Health** (`GET /api/health`) is a readiness probe that must *always* answer promptly; it is
  routed outside the `hardened` (timeout-wrapped) sub-router so no deadline can ever short-circuit it.
- **Static / SPA** (`ServeDir` + `index.html` fallback) is served by `fallback_service` on the outer
  router, entirely outside `/api`; a long-lived asset stream must never be killed by an API deadline,
  and it carries no request body worth capping.

## Interaction with existing middleware / graceful shutdown / auth

- **Layer order on `/api`:** `TraceLayer` stays outermost (so a timed-out request still logs its
  `408` + latency), then the `hardened` sub-router applies the timeout, then `require_session` /
  handlers run. Ordering: `SetRequestId → Trace → Propagate` (unchanged) wraps
  `Timeout → auth → handler`.
- **Auth:** `require_session` (`route_layer` on the protected subtree) is unchanged and still runs
  inside the timeout — an authenticated request is still authenticated; an unauthenticated one still
  `401`s well within the deadline.
- **Graceful shutdown (QE-407):** untouched. The timeout is a per-request deadline entirely
  independent of `axum::serve(...).with_graceful_shutdown(...)` and the `RunManager` drain; no shared
  state, no interference.

## Test plan (`crates/server`)

- **413 (non-vacuous), integration (`tests/hardening.rs`), through the real `build_router`:**
  - an authenticated `POST /api/runs` with a body **over** 256 KiB ⇒ **413**;
  - a **within-limit** authenticated `POST /api/runs` reaches the handler (an intentionally invalid
    small body ⇒ **400** validation, *not* 413) — proving the cap does not reject normal bodies.
- **408 (non-vacuous), unit test in `lib.rs` (`cfg(test)`):** the exact
  `tower_http::timeout::TimeoutLayer` type applied by `build_router`, over a deliberately slow route
  with a tiny (50 ms) deadline ⇒ **408**; a fast route under the same layer ⇒ **200** (control). A
  real 30 s end-to-end timeout is untestable in-process and there is no production handler that hangs
  deterministically, so the layer's status behaviour is asserted directly (spec-sanctioned).
- **Happy path intact, through the real router:** `GET /api/health` ⇒ **200** and a normal
  `GET /api/runs` (authenticated, empty store) ⇒ **200** — the added layers don't break the
  happy path; existing `tests/http.rs` / `tests/runs.rs` continue to pass unchanged.

## Risks

- **Low.** Additive tower layers on the `/api` subtree; no handler, wire, or file-format change.
- **Byte-identical goldens/vintages:** this is server transport only — no golden or vintage output is
  touched.
- **Body cap too tight?** 256 KiB is ~20–50× a realistic run spec; if a legitimate very-large
  universe ever approaches it, the const is a one-line bump with a clear 413 signal (no silent
  truncation).
