# QE-201 — Venue-aware REST client (rate-limit + ephemeral cache) — design note

`Phase: P2` · `Area: ② Market observables` · `Depends on: QE-004` · `Branch: qe-201/venue-rest-client`

## Goal (from backlog)

All REST ingress flows through a venue-aware rate-limit handler; closed-window historical responses are
immutable and cacheable.

- Rate-limit handler honouring venue weights; paginated + retried fetchers.
- Ephemeral REST cache (read-through + write-back) for closed-window historical responses, sitting **below**
  the rate-limit handler.

**Acceptance criteria.**
- [ ] Rate-limit pressure backs off without dropping requests; closed-window responses are served from
  cache on repeat.

**Out of scope.** wss (QE-202/203); the offline backfill REST (`qe-ingest::rest`, QE-102) — that is the
*batch* path; QE-201 is the *live/runtime* venue client.

## Current-state evidence & placement

- `qe-venue` is a scaffold (only `crate_name()`), the venue-connectivity layer established in QE-001. It is
  a **runtime-side** crate — the information firewall (QE-132) forbids `qe-wfo`/`qe-ensemble` from reaching
  it, and forbids it from being an *upstream* of the search/portfolio side; it does not (and must not)
  depend on `qe-wfo`/`qe-ensemble`. QE-201 lands here.
- The offline backfill already models the network as a seam: `qe-ingest::rest::{RestSource, RestError}`
  with `RateLimited{retry_after_ms}/Transient/Fatal`, and `qe-ingest::fetcher::{Fetcher, HttpFetcher,
  FakeFetcher}` behind an `http` feature (concrete `ureq` client default-off so CI stays offline). QE-201
  mirrors that seam discipline in `qe-venue` rather than depending on the offline crate (the live venue
  client and the batch downloader are parallel consumers of the same idea, not a dependency edge).
- `qe-domain` gives `Timestamp`, `TimeInterval` (half-open `[start,end)`), `InstrumentId`, `Resolution`.

## Design

The data path is **client → rate-limit handler → ephemeral cache → transport**. The cache sits *below* the
rate-limit handler (per the spec) so a cache hit costs **no** rate-limit weight and **no** transport call;
only a miss descends to the weighted transport.

### D1 — Venue weight rate limiter (`ratelimit.rs`)

`RateLimiter` is a pure, deterministic **rolling-window weight budget** (Binance-style: e.g. 1200 weight /
60_000 ms). It tracks `(window_start_ms, used_weight)`. `acquire(weight, now_ms) -> Acquire`:

- if `now_ms >= window_start_ms + window_ms`, the window rolls (reset `used = 0`, `window_start = now_ms`);
- if `used + weight <= budget`, charge it and return `Acquire::Ready`;
- else return `Acquire::WaitUntil(window_start_ms + window_ms)` — the earliest instant the window rolls and
  the weight fits. **It never rejects/drops** — it always yields a time at which the request proceeds.

A separate `note_retry_after(until_ms)` lets a venue `429`'s `Retry-After` push the next-allowed instant
forward (the handler honours the venue over its own estimate). Pure `now_ms` in, decision out — no clock.

### D2 — Time/sleep seam (`clock.rs`)

`trait Clock { fn now_ms(&self) -> i64; fn sleep_until(&self, deadline_ms: i64); }` — the single seam for
*both* reading time and backing off. `SystemClock` (always compiled; `std::time` + `thread::sleep`) for
production; `ManualClock` (test) records every `sleep_until` and advances a logical clock instead of
sleeping, so backoff is asserted deterministically with **zero wall-clock**.

### D3 — Transport seam + request model (`rest.rs`)

- `VenueRequest { endpoint, instrument, params, window, weight }` where `window: TimeInterval` is the data
  span the call covers and `weight` its venue cost. `is_closed_window(now_ms)` = `window.end() <= now`
  (immutable history → cacheable); an open/in-progress window is never cached.
- `RestResponse { bytes }`. `RestError { RateLimited{retry_after_ms}, Transient(String), Fatal(String) }`
  (same classification as the offline path, owned by `qe-venue`).
- `trait RestTransport { fn send(&self, req:&VenueRequest) -> Result<RestResponse, RestError>; }` — the one
  network seam. `HttpRestTransport` (ureq, system TLS) behind the `http` feature; `FakeTransport` (test)
  serves a scripted response sequence per endpoint and counts hits.

### D4 — The client (`rest.rs`)

`VenueRestClient<T: RestTransport, C: Clock>` owns the limiter + cache + transport + clock.
`fetch(req) -> Result<RestResponse, RestError>`:

1. **Cache read-through** — if `req.is_closed_window(clock.now)` and the cache holds the key, return it
   (no weight, no transport).
2. **Retry loop** (bounded `max_attempts`): `acquire(req.weight, now)`; on `WaitUntil(t)` →
   `clock.sleep_until(t)` then re-acquire (the *back-off*, never a drop); send; on
   `RateLimited{retry_after_ms}` → `note_retry_after(now+retry_after)`, `sleep_until`, retry; on
   `Transient` → exponential backoff `sleep_until`, retry; on `Fatal` → return. Exhausting attempts returns
   the last error.
3. **Cache write-back** — on success, if closed-window, store the bytes under the key.

`paginate(base_req, advance, stop) -> Result<Vec<RestResponse>, RestError>` drives D4 across pages (each
page is a `fetch`, so each page is independently rate-limited, retried, and cached).

### D5 — Ephemeral cache (`cache.rs`)

`RestCache` — an in-memory `HashMap<CacheKey, Vec<u8>>` (`RefCell` for interior mutability through `&self`).
`CacheKey` = endpoint + instrument + canonical params + window. *Ephemeral* = process-lifetime only (vs the
persistent sha-verified `qe-ingest::RawCache`); closed-window historical responses are immutable so an
in-memory memo is sufficient and correct. `get`/`put`; only the client decides what is cacheable.

## Module / API plan

New deps for `qe-venue`: `thiserror`, `serde`/`serde_json` (request/cache keying); optional `http` feature →
`ureq` (system TLS), default-off. Modules: `ratelimit`, `clock`, `rest`, `cache`; re-exported from `lib.rs`.

## Test plan (TDD)

1. **Rate-limit back-off without dropping (AC).** Budget = 2×weight, window W; transport always succeeds.
   Fire 5 same-weight requests through a `ManualClock`. Assert all 5 reach the transport (none dropped) and
   the clock was advanced (`sleep_until` recorded) when the window filled — i.e. it *waited*, it didn't drop.
2. **429 honoured, not dropped.** `FakeTransport` returns `RateLimited{retry_after_ms}` twice then `Ok`.
   Assert `fetch` returns `Ok`, the recorded sleeps cover the `Retry-After`, and the request completed.
3. **Closed-window served from cache on repeat (AC).** Closed window; first `fetch` → 1 transport hit;
   identical second `fetch` → still 1 transport hit (served from cache), bytes equal.
4. **Open window is not cached.** `window.end() > now` → two fetches → 2 transport hits (no caching of
   mutable in-progress data).
5. **`RateLimiter` unit.** budget accounting, window roll resets used, `WaitUntil` is the roll instant,
   `note_retry_after` pushes the next-allowed instant out.
6. **Transient retry + Fatal stop.** Transient then Ok → succeeds with backoff; Fatal → returns immediately.
7. **`paginate`** drives multiple pages, each rate-limited/cached.

## Gates

`cargo fmt --check`, `cargo clippy --workspace --all-targets -D warnings`, `cargo test -p qe-venue`,
`cargo test --workspace`, `cargo test -p qe-architecture --test firewall` (no firewall regression),
`cargo deny check`.

## Risks

- **Deterministic time.** All backoff goes through `Clock::sleep_until`; core tests use `ManualClock` so no
  test sleeps and timing is asserted exactly. Wall-clock only via `SystemClock` in production.
- **Cache correctness hinges on "closed window".** Only `window.end() <= now` responses are memoised; an
  open window is always re-fetched. This is the immutability invariant the spec relies on.
- **Bounded retries.** `max_attempts` caps the loop so a permanently-rate-limited venue eventually returns
  an error rather than looping forever — "backs off without dropping" within the attempt budget, then
  surfaces the failure (never a silent drop).
- **Firewall.** Deps are `qe-domain` + leaf third-party only; `qe-venue` gains no `qe-wfo`/`qe-ensemble`
  edge, so the QE-132 guard stays green.
```
