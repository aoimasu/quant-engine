# QE-427 — Container/deploy path for the admin server + SPA; fail closed

`Phase: cross-cutting` · `Area: architecture / deployment` · `Effort: M` · `Depends on: QE-013, QE-254, QE-258` · `Precedes: QE-311`

Spec of record: `### QE-427` in `docs/reviews/2026-07-15-team-improvement-review.md` (lines 685-703).

## 1. Current state (the gap)

- The sole `Dockerfile` builds **only** `-p qe-cli` and ships `ENTRYPOINT ["qe"]` / `CMD ["train", …]`.
  It packages a **batch job**, not the long-lived `qe-server` HTTP service, and it never builds the SPA
  (`web/dist`). There is no image / compose / manifest that runs the admin UI, so "deploy the admin UI"
  has no reproducible artefact.
- `qe-server` (`crates/server`) is the long-lived HTTP service: it serves the SPA static assets at `/`
  and the `/api/*` routes (health, OAuth session, run lifecycle, read APIs). Verified knobs
  (`crates/server/src/lib.rs`, `config.rs`, `auth/mod.rs`, `runs/spawn.rs`):
  - `QE_SERVER_ADDR` — bind address. **Default `127.0.0.1:8080` (loopback).**
  - `QE_SERVER_STATIC_DIR` — built-SPA dir served at `/`. Default `crates/server/static` (a placeholder).
    **This is the real static-dir knob and it matches the spec's `QE_SERVER_STATIC_DIR` verbatim** —
    QE-419/QE-425 did not rename it (QE-425 changed *how* `ServeDir`/`ServeFile` fall back to
    `index.html`, not the env var).
  - `QE_SERVER_DATA_DIR` — run-store state dir. Default `data` (relative). Run store lives at
    `<data_dir>/runs`.
  - `QE_SERVER_CLI_BIN` — path to the `qe` (qe-cli) binary the server spawns for backtest/train runs.
    `resolve_cli_bin()` falls back to a `qe` co-located with the `qe-server` executable, else `PATH`.
  - `QE_CONFIG` — `qe-config` file (default `config.toml`); pinned onto every spawned `qe-cli` (QE-419).
  - OAuth/session (`crates/server/src/auth/mod.rs`): `QE_OAUTH_GOOGLE_CLIENT_ID`,
    `QE_OAUTH_GOOGLE_CLIENT_SECRET`, `QE_OAUTH_REDIRECT_URI`, `QE_SESSION_SECRET`,
    `QE_ADMIN_ALLOWED_EMAILS`. The **real** Google ID-token verifier is behind the `http` cargo feature
    (default-off); the binary must be built `--features http` for live Google sign-in.
- Storage dirs come from `config.example.toml [storage]` — `market_dir = "data/lmdb/market"`,
  `artifacts_dir = "data/artifacts"`, both **relative** (QE-013), so QE-311 stays mechanical.

## 2. Fail-closed is already implemented (QE-409) — this ticket only makes the image trigger it

`crates/server/src/main.rs` calls `qe_server::auth::check_session_secret_policy(&cfg.addr,
auth_config.session_secret_is_ephemeral)` at boot and returns `ExitCode::FAILURE` when the bind is
**non-loopback** and no explicit `QE_SESSION_SECRET` was set (the session secret is then a random
ephemeral fallback, safe only on loopback). This is the AC's second half and it is already covered by
the unit test `session_secret_policy_is_fail_closed_off_loopback` (`crates/server/src/auth/mod.rs`).

**This ticket does not re-implement that.** The image's job is to make the policy *apply*: a deployed
container is only useful if reachable from outside, so the image binds `QE_SERVER_ADDR=0.0.0.0:8080`
(non-loopback). With that bind and no `QE_SESSION_SECRET`, `qe-server` refuses to boot — exactly the AC.
The compose/run docs therefore mark `QE_SESSION_SECRET` **required**.

## 3. Design of the server image

Add a **distinct** server image so the existing CLI batch image (`Dockerfile`) is untouched. Chosen
form: a **second file, `Dockerfile.server`** (clearest separation; the CLI `Dockerfile` keeps its own
`ENTRYPOINT ["qe"]`). Multi-stage:

1. **SPA build stage** (`node:20-slim`, `web-builder`): `npm ci` + `npm run build` (`tsc -b && vite
   build`) → `web/dist`. Only the `web/` context is copied so a Rust source change does not bust the npm
   layer cache.
2. **Rust build stage** (`rust:1.96`, `server-builder`): `cargo build --release --locked` for **both**
   `qe-server` (with `--features http` so live Google OAuth works) and `qe-cli` (so the server can spawn
   real backtest/train runs via the co-located `qe`). `QE_CODE_COMMIT` build-arg threaded through for
   provenance parity with the CLI image (QE-420).
3. **Runtime stage** (`debian:bookworm-slim`): copy `qe-server` + `qe` into `/usr/local/bin` (co-located
   so `resolve_cli_bin()` finds `qe` with no extra config), copy the SPA into `/app/web/dist`, copy
   `config.example.toml` → `/app/config.toml`. Set:
   - `QE_SERVER_ADDR=0.0.0.0:8080` (non-loopback → the fail-closed policy applies),
   - `QE_SERVER_STATIC_DIR=/app/web/dist` (the bundled SPA — the real knob, matching the spec),
   - `QE_SERVER_DATA_DIR=/app/data`, `QE_CONFIG=/app/config.toml`,
   - `EXPOSE 8080`, `VOLUME ["/app/data"]` (relative volume in compose — QE-013),
   - `ENTRYPOINT ["qe-server"]` (no `CMD` args — the server reads env).

Because `QE_SESSION_SECRET` is deliberately **not** baked in, `docker run` with only the defaults refuses
to boot (fail-closed demonstrated); supplying `QE_SESSION_SECRET` + OAuth env boots the authenticated
server + SPA.

## 4. Compose / run manifest

`docker-compose.server.yml` runs the server image with a **relative** bind mount (`./data:/app/data`,
QE-013 — keeps QE-311 mechanical), publishes `8080`, and threads the required env from the host
environment: `QE_SESSION_SECRET` (required — no default, so a missing value both fails compose
interpolation intent and, if empty, trips the fail-closed policy), the OAuth trio, and
`QE_ADMIN_ALLOWED_EMAILS`. A committed `.env.server.example` documents every variable.

## 5. TLS / fronting proxy (out of scope, documented)

TLS termination is explicitly out of scope (spec). The image speaks plain HTTP on `8080`; a real deploy
puts a TLS-terminating reverse proxy (nginx/Caddy/PaaS router) in front. The OAuth `redirect_uri` must
be the **public https** URL so `qe-server` mints `Secure` session cookies (QE-409's `cookie_secure` is
derived from the `redirect_uri` scheme; the advisory `should_warn_insecure_cookies` warns if it is not
https). Documented in `docs/deploy/README.md`.

## 6. How the AC is demonstrated

- **"A documented image runs the authenticated server + SPA end-to-end"**:
  `docker build -f Dockerfile.server -t qe-server .` then
  `docker compose -f docker-compose.server.yml up` (env from `.env.server`). The container serves the
  built SPA at `/` and the authenticated `/api/*`; docs give the exact commands.
- **"A non-loopback bind without a session secret refuses to start"**: the image binds `0.0.0.0:8080`;
  omit `QE_SESSION_SECRET` and `qe-server` exits `FAILURE` via `check_session_secret_policy`. Proven in
  code by the existing test `session_secret_policy_is_fail_closed_off_loopback`
  (`crates/server/src/auth/mod.rs`) — not re-implemented here.

## 7. Was `docker build` runnable here?

**Yes** — a Docker daemon turned out to be available, so the image was built AND the AC was verified
live (not only argued from source):

- `docker build -f Dockerfile.server -t qe-server .` → success (143 MB runtime image).
- **Fail-closed** (`docker run` with no `QE_SESSION_SECRET`): the container binds `0.0.0.0:8080` and
  exits non-zero with `refusing to boot: bound to a non-loopback address (0.0.0.0:8080) with an
  ephemeral session secret` — the AC's second half, live.
- **End-to-end** (`docker run` with `QE_SESSION_SECRET`): `qe-server listening`
  (`static_dir=/app/web/dist`, `cli_bin=/usr/local/bin/qe` — the co-located CLI resolved); `GET
  /api/health` → `200 {"status":"ok"}`; `GET /` → `200 text/html` serving the built SPA shell
  (`<title>Quant Engine</title>`, `<div id="root">`).

The artefacts the image copies were also built on the host to double-check: `web/dist` via `npm run
build`, and `qe-server` (`--features http`) via `cargo build`.

**Build note:** the runtime is `debian:bookworm-slim`, so the Rust builder is pinned to
`rust:1.96-bookworm` (not the default `rust:1.96`, which tracks a newer Debian with a glibc the slim
runtime lacks — the first build linked fine but the binary failed at runtime with
`GLIBC_2.38/2.39 not found`). The **existing CLI `Dockerfile` uses the unpinned `rust:1.96`** and so
carries the same latent glibc mismatch — flagged as an out-of-scope observation; not touched here.

## 8. Risks / blast radius

- **No Rust/crate changes** — the fail-closed behaviour already exists; this is Dockerfile + compose +
  docs only. Green gate (`fmt`/`clippy`/`test`/`deny`/firewall) should be unaffected; run to confirm.
- **`http` feature pulls `native-tls`** at build time only (already a workspace dependency; QE-253/256).
- **Image size**: two Rust binaries + slim SPA on `debian:bookworm-slim`; multi-stage keeps toolchains
  out of the runtime layer.
- **`.dockerignore`**: add one so `target/`, `web/node_modules/`, `web/dist/`, `.git` do not bloat the
  build context (the SPA is rebuilt inside the image from source).
