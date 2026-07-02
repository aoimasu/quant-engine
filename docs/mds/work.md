# Work — PR review tracker

Transient scratchpad for the **PR currently under review** only. A PR entry is added here when it
reaches review, the dedicated review agent writes `[Reviewed]`/`[Approved]` + comments inline, and on
merge the approved block is archived to `docs/mds/reviewed/<ticket>.md` and this file is **cleared back
to empty**. No running "Completed" list is kept here — the traceable history lives solely in
`docs/mds/reviewed/`.

> **Branch protection note (since QE-005):** `main` requires CI checks (`fmt`/`clippy`/`test`/`deny`)
> with `enforce_admins=true`, which blocks direct pushes. Archive bookkeeping for a merged ticket is
> therefore committed on the *next* ticket's branch so it flows through a PR + CI.

---

## QE-218 — gRPC transport (Hedge Planner ↔ Edge gateway) — [Ready-for-review]

- **PR:** #69 — https://github.com/aoimasu/quant-engine/pull/69
- **Ticket:** QE-218 (`Phase: P2` · `Area: ⑤↔⑥` · `Depends on: QE-214, QE-217`)
- **Branch:** `qe-218/grpc-transport`
- **Latest commit:** `2e2986c861feef8957d7400f00975b41bc7d2a66`
- **Evidence / design:** `docs/architecture/qe-218-grpc-transport-design.md`
- **Changed files:** `crates/runtime/src/transport.rs` (new), `crates/runtime/src/lib.rs` (module +
  re-exports), design note. (Also archives QE-216 → `docs/mds/reviewed/qe-216.md` + clears the prior
  `work.md` entry.)

### Goal
Decisions flow planner→adapter over gRPC; fills/positions/heartbeat flow back. Backpressure and reconnection
handled; the QE-301 journal-append path must **never gate** the dispatch.

### Acceptance criteria (from backlog)
- [x] A target revision reaches the adapter and fills/positions return; the append path (QE-301) never gates
  this dispatch — `target_revision_reaches_adapter_and_fills_return`, `append_never_gates_dispatch`.

### Implementation summary
- New `crates/runtime/src/transport.rs`: `PlannerAdapterLink<A: AppendSink>` — a **deterministic,
  single-threaded, pull-based** model of the planner↔adapter gRPC bidi stream. `TargetRevision` (absolute,
  monotonic `seq`) in; `AdapterReport::{Fill, Position, Heartbeat{ack_seq, health}}` (with `VenueHealth`) out.
- `pump()` → `plan_delta` vs the authoritative kept position → submit **through the QE-216 `VenueKillGate`**
  → absorb the fill into `VenueKeeper` → return fills + authoritative position + heartbeat.
- **Backpressure = coalesce-to-latest** (`submit_target` keeps only the newest revision; `dropped_superseded`
  observable) — lossless *because* `TargetPosition` is absolute.
- **Reconnection = re-snapshot + re-send latest** (`disconnect`/`reconnect`) — re-sending the latest absolute
  target is idempotent (`plan_delta` → 0 delta → no double-fill).
- **Append never gates dispatch:** the `AppendSink` (QE-301 seam) is *offered* the already-produced reports;
  its `Result` is counted (`append_failures`) but **cannot alter** the dispatch. Real tonic/gRPC wire deferred
  to the runtime binary (QE-201/202 offline-core convention); no new workspace dep; firewall unaffected.
- **Scrutinise:** (1) coalescing backpressure **drops** superseded absolute targets — is "lossless because
  absolute" fully sound (e.g. does any consumer need intermediate revisions)? (2) reconnection re-snapshots
  from the **sim** `position_report` — right source of truth vs the keeper? (3) `append_never_gates_dispatch`
  proven structurally (return value produced before `append`) — is that a genuine proof of the AC, or does a
  real async journal need more? (4) position report sourced from `gate.simulator()` while `plan_delta` reads
  the `keeper` — are sim and keeper guaranteed in sync on this path? (5) `keeper_mut()` exposed for the
  mark/account streams — acceptable encapsulation?

### Verification (toolchain 1.96.0)
- `cargo fmt --all --check` — clean
- `cargo clippy --workspace --all-targets --locked -- -D warnings` — clean
- `cargo test --workspace --locked` — 552 passed / 1 ignored / 56 suites (+6 transport tests)
- `cargo test -p qe-architecture --test firewall` — 1 passed
- `cargo deny check` — advisories/bans/licenses/sources ok
