# QE-411 — Take run-store / read blocking `std::fs` off the async executor (extends QE-266)

`Phase: PreP3` · `Area: backend / async correctness` · `Depends on: QE-255` · `Effort: M`

Spec of record: `docs/reviews/2026-07-15-team-improvement-review.md` § `### QE-411`.

## Why

The run-lifecycle HTTP handlers perform synchronous `std::fs` on tokio worker threads. Every
such call parks an async executor thread on a disk read/write, and `list_runs` scales it to
`O(runs)` blocking `read_meta()` calls in a loop. The QE-257 read handlers (`crates/server/src/read.rs`)
already model the correct fix — the blocking filesystem/LMDB work runs inside
`tokio::task::spawn_blocking`, keeping the async worker free. The runs path never got the same
treatment. This ticket brings the run-store fs ops onto the same pattern.

## Current-state evidence (post-QE-407, HEAD `2bfe2d9`)

QE-407 reworked `crates/server/src/runs/{api.rs,manager.rs}` (graceful shutdown, a supervised-task
registry, honest success). Line numbers in the spec predate QE-407; the current blocking sites are:

- `crates/server/src/runs/api.rs`
  - `list_runs` (async handler): `store.read_index()` then a `for entry in index.iter().rev()` loop
    calling `store.read_meta(&entry.id)` — one blocking read per run, on the async body.
  - `get_run` (async handler): `store.read_meta(&id)`.
  - `get_result` (async handler): `store.read_meta(&id)` then `std::fs::read(store.result_path(&id))`.
- `crates/server/src/runs/manager.rs`
  - `create` (async, reached from the `create_run` handler):
    - `self.store.init_run(&meta)?` — `create_dir_all` + touch `stdout.log` + write `meta.json`,
      **before** the index lock, on the async body.
    - Under `self.index_lock.lock().await`: `self.store.read_index()?` … `self.store.write_index(&index)?`
      — a blocking read-modify-write of `index.json` **while holding the async mutex**, which parks the
      executor thread *and* holds the lock across the blocking work.

`crates/server/src/runs/store.rs` holds the fs primitives (`read_index`, `write_index`, `read_meta`,
`write_meta`, `result_path`, `init_run`) and the atomic-write helper. `RunStore` is `#[derive(Clone)]`
(just a `PathBuf`), so a `spawn_blocking` closure can own a cheap clone — exactly how `read.rs` clones
`Arc`s into its closures.

### Out of scope (deliberately untouched)

- `supervise` (a spawned supervisor task, not a handler body) writes `meta.json`/`stdout.log` line by
  line via `store.write_meta` / `std::fs::OpenOptions`. Rewrapping each streaming progress write in
  `spawn_blocking` is a large, risky behaviour change and is **not** a handler body — left as-is.
- `RunManager::shutdown` / `terminally_mark_interrupted` / `reconcile_orphans` (lifecycle + startup
  paths, not request handlers) — left as-is so the QE-407 drain semantics are preserved verbatim.
- The on-disk format / atomic-write strategy — explicitly out of scope per the spec.

## Decisions

1. **Mirror `read.rs` exactly.** Each async run handler clones the cheap `RunStore` and runs its fs work
   inside `tokio::task::spawn_blocking`, matching the `Ok(Ok(..)) / Ok(Err(..)) / Err(_)` join-result
   arms already used by `list_vintages` / `market_data_coverage`.
2. **`list_runs`: one `spawn_blocking` closure, not one per run.** A single closure reads the index and
   loops `read_meta` off-thread, returning `Result<Vec<RunMeta>, String>` where the `Err` carries the
   **already-formatted** error string (`"failed to read index: {e}"` / `` "failed to read run `{id}`: {e}" ``)
   so the 500 body is byte-identical. The newest-first ordering (`index.iter().rev()`) and the
   skip-on-missing-meta (`Ok(None) => {}`) semantics are preserved inside the closure.
3. **`get_result`: one closure returning a small outcome enum** (`Body / NotFound / NotReady(status) /
   Missing / MetaError(msg)`); the `Response` (status codes + JSON bodies) is built on the async side from
   that enum, keeping every status code and body byte-identical. Any failure to read `result.json`
   (previously any `std::fs::read` `Err`) still maps to the `409 result artefact missing` body.
4. **Move the result read into the store.** Add `RunStore::read_result(id) -> io::Result<Vec<u8>>`
   (a thin `fs::read(result_path)`), so `api.rs` no longer references `std::fs` directly and all run-store
   fs primitives live in `store.rs` (consistent with `read_meta`/`read_index`). Behaviour is identical to
   the previous inline `std::fs::read` (any `Err` ⇒ 409 missing).
5. **`create`: two `spawn_blocking` calls.**
   - `init_run` runs in `spawn_blocking` (clone of `store` + `meta`) **before** the lock, awaited to
     completion — same ordering as today (init before index append).
   - The `index.json` read-modify-write runs in a single `spawn_blocking` closure **while the async
     `index_lock` guard is held**. Holding the async mutex across the `.await` still serialises concurrent
     creates (the guard is `Send` and held across the await), but the blocking disk work no longer parks
     the executor thread. Join errors map to `CreateError::Io` via `std::io::Error::other`.
6. **New join-error 500s** (`"run listing task failed"`, `"run task failed"`, `"result task failed"`) only
   fire if the blocking task panics/cancels — impossible on the normal path — mirroring read.rs's
   `"vintage listing task failed"`. No normal-path behaviour change.

## Behaviour-invariance argument

- `list_runs`: same index read, same `.rev()` order, same skip-missing-meta, same error strings, same
  `Json(Vec<RunMeta>)` body ⇒ byte-identical output and status.
- `get_run`: same `read_meta` match arms (`Some`→200 body, `None`→404 body, `Err`→500 same message).
- `get_result`: same sequence (meta → status gate → result bytes) and the same five outcomes with
  identical status codes and JSON bodies.
- `create`: same validation, same id/meta construction, same init-then-index-append ordering, same
  `index_lock` serialisation, same `CreateError` variants ⇒ same 201/400/503/500 outcomes.

The persisted `meta.json` / `index.json` bytes are produced by the unchanged `store` write helpers, so
no golden/fixture content changes. This ticket does not touch any vintage-producing code path.

## Test plan

- Existing `crates/server/tests/runs.rs` integration suite exercises every async path end-to-end
  (create→running→succeeded, result 200, failure 409, bounded pool queueing, uniform-400 validation,
  train rich progress, unknown-id 404s, QE-407 shutdown/reconcile/honest-success) — all must stay green,
  proving behaviour unchanged through the `spawn_blocking` wrapping.
- Add `list_runs_newest_first_and_skips_missing_meta`: two runs A then B, both succeeded; assert the list
  is newest-first `[B, A]`; then delete A's `meta.json` on disk and assert the list is `[B]` (indexed but
  meta-missing is skipped, not a 500) — directly proving the batched closure preserves order + skip.
- Add a source-scan guard unit test in `api.rs`: the non-test region of the handler source contains no
  `std::fs::` token (needle built dynamically to avoid self-match; `//` comment lines stripped) — encodes
  the AC "no blocking `std::fs` remains on an async handler body".
- Keep `store.rs` unit tests; add a `read_result` round-trip assertion.

## Risks

- **Holding `index_lock` across an `.await`.** Mitigated: the guard is `Send`, the closure is the only
  awaited work under the lock, and no other lock is acquired inside it — no deadlock, same serialisation.
- **Join-error paths** are new but unreachable on the normal path; they only convert a panicked blocking
  task into a clean 500 / `CreateError::Io` instead of propagating a panic.
- **Firewall**: no new cross-crate deps (`spawn_blocking` is tokio, already a dep); server ⊬
  runtime/venue/cli/wfo/ensemble is unaffected.

## Rollback

Revert the single commit; the change is confined to `crates/server/src/runs/{api.rs,manager.rs,store.rs}`
plus tests. No schema, no data migration.
