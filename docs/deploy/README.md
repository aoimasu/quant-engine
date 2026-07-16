# Deploying the admin UI (`qe-server` + SPA) — QE-427

Two **distinct** container images live in this repo:

| Image | Dockerfile | What it runs | Entry |
| --- | --- | --- | --- |
| **CLI batch** | `Dockerfile` | one-shot `qe train`/backtest jobs | `ENTRYPOINT ["qe"]`, `CMD ["train", …]` |
| **Server** (this doc) | `Dockerfile.server` | the long-lived `qe-server` admin HTTP service + the built SPA | `ENTRYPOINT ["qe-server"]` |

The server image is a **multi-stage** build:

1. **`web-builder`** (`node:20-slim`) — `npm ci` + `npm run build` → `web/dist` (the SPA).
2. **`server-builder`** (`rust:1.96`) — `cargo build --release -p qe-server --features http` (live
   Google OAuth) **and** `-p qe-cli` (so the server can spawn real backtest/train runs).
3. **`runtime`** (`debian:bookworm-slim`) — the `qe-server` + co-located `qe` binaries, the bundled SPA
   at `/app/web/dist`, and `config.example.toml` → `/app/config.toml`.

## Fail-closed on a network bind (QE-409)

A deployed container must be reachable, so the image binds **`QE_SERVER_ADDR=0.0.0.0:8080`**
(non-loopback). Per QE-409's `check_session_secret_policy` (`crates/server/src/auth/mod.rs`), a
non-loopback bind with **no** `QE_SESSION_SECRET` uses a random ephemeral secret that is unsafe off
loopback — so `qe-server` **refuses to boot** (`ExitCode::FAILURE`). This is covered by the unit test
`session_secret_policy_is_fail_closed_off_loopback`. Therefore **`QE_SESSION_SECRET` is required** for
this image; generate one with `openssl rand -hex 32`.

## Configuration (env)

| Var | Required | Purpose |
| --- | --- | --- |
| `QE_SESSION_SECRET` | **yes** | HMAC key for session cookies. Missing on a `0.0.0.0` bind ⇒ fail-closed. |
| `QE_OAUTH_GOOGLE_CLIENT_ID` / `QE_OAUTH_GOOGLE_CLIENT_SECRET` | for login | Google OAuth client (QE-256). |
| `QE_OAUTH_REDIRECT_URI` | for login | Public **https** URL of `/api/auth/callback`. https ⇒ cookies minted `Secure` (QE-409). |
| `QE_ADMIN_ALLOWED_EMAILS` | for login | Comma-separated sign-in allowlist. Empty ⇒ nobody can sign in (fails closed). |
| `QE_SERVER_STATIC_DIR` | preset | Built-SPA dir. Image sets `/app/web/dist`. |
| `QE_SERVER_ADDR` | preset | Bind address. Image sets `0.0.0.0:8080`. |
| `QE_SERVER_DATA_DIR` / `QE_CONFIG` | preset | Run-store state dir (`/app/data`) and qe-config (`/app/config.toml`). |

## Run it

### docker compose (recommended)

```bash
cp .env.server.example .env.server     # fill in the OAuth + session values
docker compose --env-file .env.server -f docker-compose.server.yml up --build
```

The compose file mounts a **relative** `./data:/app/data` volume (QE-013, so QE-311 stays mechanical)
and publishes `8080`. `QE_SESSION_SECRET` is marked required (`${QE_SESSION_SECRET:?…}`), so compose
errors out if it is unset.

### plain docker

```bash
docker build -f Dockerfile.server -t qe-server .
docker run --rm -p 8080:8080 \
  -e QE_SESSION_SECRET="$(openssl rand -hex 32)" \
  -e QE_OAUTH_GOOGLE_CLIENT_ID=... -e QE_OAUTH_GOOGLE_CLIENT_SECRET=... \
  -e QE_OAUTH_REDIRECT_URI=https://admin.example.com/api/auth/callback \
  -e QE_ADMIN_ALLOWED_EMAILS=you@example.com \
  -v "$(pwd)/data:/app/data" qe-server
```

Then the SPA is served at `http://<host>:8080/` and the authenticated API at `/api/*`.

### Verify fail-closed

```bash
docker run --rm -p 8080:8080 qe-server   # no QE_SESSION_SECRET
# => logs "refusing to boot" and exits non-zero (QE-409).
```

## TLS / fronting proxy (out of scope, assumed)

QE-427 does **not** terminate TLS. Run a TLS-terminating reverse proxy (nginx / Caddy / a PaaS router)
in front of `:8080` and forward to the container. Set `QE_OAUTH_REDIRECT_URI` to the **public https**
callback URL so `qe-server` derives `cookie_secure = true` and mints `Secure` cookies (QE-409); if the
scheme is not https on a non-loopback bind, boot logs an advisory `should_warn_insecure_cookies`
warning (non-fatal).

Choosing/committing a specific PaaS is also out of scope — the image makes no platform assumptions
(state lives under the relative, mountable `/app/data` volume — QE-013).
