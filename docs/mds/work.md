# Work — PR review tracker

Active PRs awaiting/under review for the P0/P1 ticket run. Each entry is reviewed by the
dedicated review agent, which writes `[Reviewed]`/`[Approved]` + comments inline. On merge, the
approved block is archived to `docs/mds/reviewed/<ticket>.md` and removed from here.

> **Branch protection note (since QE-005):** `main` requires CI checks (`fmt`/`clippy`/`test`/`deny`)
> with `enforce_admins=true`, which blocks direct pushes. Archive bookkeeping for a merged ticket is
> therefore committed on the *next* ticket's branch so it flows through a PR + CI.

## Completed (archived in `docs/mds/reviewed/`)
- QE-001 — Cargo workspace & crate topology — PR #1 — Approved & merged.
- QE-002 — Configuration system — PR #2 — Approved & merged.
- QE-003 — Structured logging & tracing — PR #3 — Approved & merged.
- QE-004 — Error model & result conventions — PR #4 — Approved & merged.
- QE-005 — CI pipeline — PR #5 — Approved & merged.
- QE-006 — Determinism & reproducibility harness — PR #6 — Approved & merged.
- QE-007 — Shared domain types — PR #7 — Approved & merged.
- QE-008 — Clock-skew / time-sync guard — PR #8 — Approved & merged.
- QE-009 — Risk-limit & kill-switch contract — PR #9 — Approved & merged.
- QE-010 — LMDB market-data store — PR #10 — Approved & merged.
- QE-011 — LMDB synthetic-data store — PR #11 — Approved & merged.
- QE-012 — Instrument-universe config & point-in-time membership — PR #12 — Approved & merged.
- QE-013 — Local run & deployment-agnostic packaging — PR #13 — Approved & merged. **(P0 complete)**
- QE-101 — Binance public-dumps downloader — PR #14 — Approved & merged.
- QE-102 — Venue REST month-to-date backfill client — PR #15 — Approved & merged.
- QE-103 — Data-integrity & source reconciliation validation — PR #16 — Approved & merged.
- QE-104 — Fusion, normalisation & Arrow serialisation — PR #17 — Approved & merged.
- QE-105 — Persist fused market data to LMDB — PR #18 — Approved & merged.
- QE-106 — Multi-resolution bar reconstruction (batch) — PR #19 — Approved & merged.
- QE-107 — Indicator catalogue (quantised, deterministic, parity-ready) — PR #20 — Approved & merged.
- QE-108 — Feature vector assembly → synthetic store — PR #21 — Approved & merged.
- QE-109 — Execution-friction & funding model — PR #22 — Approved & merged.
- QE-110 — Strategy genome representation (SPIKE) — PR #24 — Approved & merged.
- QE-111 — QD/MAP-Elites archive & behaviour descriptors (SPIKE) — PR #25 — Approved & merged.
- QE-112 — Adaptive operator selection (SPIKE) — PR #26 — Approved & merged.
- QE-113 — Geometric fitness, noise-robust eval & purged/embargoed CV (SPIKE) — PR #27 — Approved & merged.

---

_No PRs awaiting review._
