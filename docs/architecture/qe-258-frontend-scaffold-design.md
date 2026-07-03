# QE-258 — Frontend scaffold + design-system port (Vite/React, AppShell, Login) — design/evidence

- **Ticket:** QE-258 (`Phase: PreP3` · `Area: frontend` · `Depends on: QE-256`)
- **Branch:** `qe-258/frontend-scaffold-login`
- **Spec refs:** `docs/superpowers/specs/2026-07-02-admin-ui-training-backtest-design.md` §7.1, §7.2, §9; ADR D4a; `docs/architecture/admin-ui-decisions.md`.

## 1. Current-state evidence (what this SPA integrates with)

### Server static-SPA serving (QE-254 — `docs/mds/reviewed/qe-254.md`)
- `qe-server` (axum) serves the built SPA at `/` via `tower-http` `ServeDir` with an `index.html` **SPA
  fallback** (deep links resolve to `index.html`; graceful 404 when the static dir is absent).
- `/api` is a **reserved** nested sub-router with its own 404 — unknown `/api/*` is NOT swallowed by the SPA
  fallback. So the SPA and the API are **same-origin**; cookie auth "just works" (no CORS).
- Static root is configured by env **`QE_SERVER_STATIC_DIR`** (CWD-relative default). This is the directory the
  SPA build output must be pointed at.

### Auth contract (QE-256 — `docs/mds/reviewed/qe-256.md`, spec §6.2/§6.3)
- `GET /api/auth/login` → 302 redirect to Google consent (sets a short-lived `qe_oauth_state` cookie).
- `GET /api/auth/callback` → verifies the ID token, checks the allowlist, sets a signed `qe_session` cookie.
  A valid Google login **not** on `QE_ADMIN_ALLOWED_EMAILS` ⇒ **403** (allowlist rejection).
- `GET /api/me` → **200 `{email}`** with a valid session, else **401**. Gated routes live under `/api`.
- `POST /api/auth/logout` clears the session.

**Consequence for the SPA:** on load, `GET /api/me`. 401 ⇒ render **Login**. 200 ⇒ render the **AppShell**.
"Sign in with Google" navigates the browser to `/api/auth/login` (full-page redirect; not fetch — the OAuth
dance needs top-level navigation). The allowlist-rejection state is surfaced when the app is loaded with a
rejection indicator in the URL (query `?error=forbidden|rejected|403`). See §4 risk note — the exact
callback→SPA rejection signal is a QE-256/QE-259 wiring detail; v1 reads a query param and shows the message.

## 2. Design-system source decision — **DesignSync port (faithful)**

The `DesignSync` tool **was available** and the Claude Design project **"Quant Engine Design System"**
(`projectId 2b443e2a-c374-4c7c-8437-e0d253e4bc65`, the exact id in the spec) was reachable. I **ported the
real assets** rather than reconstructing:

- **Tokens** (`tokens/{colors,typography,spacing,effects,base}.css`) ported **verbatim** into
  `web/src/styles/tokens/`. These are pure CSS custom properties — portable as-is.
- **`tokens/fonts.css`** is the one file I did **not** copy verbatim: the source loads the three families from
  the **Google Fonts CDN via `@import`**. The ticket forbids a runtime CDN dependency (prod serves static
  assets with a strict origin). I therefore **self-host** the three families via the `@fontsource/*` packages
  (`space-grotesk`, `hanken-grotesk`, `jetbrains-mono`), imported in `main.tsx` so Vite bundles the `.woff2`
  into the build. The `--font-display/-sans/-mono` token names are unchanged, so every ported rule still
  resolves to the correct family. (The design readme explicitly blesses this: "To self-host: drop the woff2
  files in and the manifest picks them up.")
- **Primitives** needed for this ticket — `Button`, `Icon`, `Badge`, `Callout`, `Card` — ported from
  `components/**` into `web/src/design/`. The source primitives are already clean React ES modules; I converted
  them to **TypeScript (`.tsx`)** with typed props, preserving every className and the injected CSS **verbatim**
  so the visual system is identical.
- **`Icon`** is the second necessary adaptation: the source wraps **Lucide from a CDN** (`window.lucide`). To
  avoid a runtime CDN, the port renders the bundled **`lucide-react`** package, keeping the same
  `name="database"` kebab-case API (mapped to lucide-react's PascalCase icon registry).
- **`AppShell`** (`ui_kits/trading-dashboard/AppShell.jsx`) ported into `web/src/design/AppShell.tsx`. The
  source used `window.QuantEngineDesignSystem_2b443e` globals, a relative-path `<img>` for the logo, and a
  trading-dashboard footer (`Momentum v3 · LIVE · $4.82M`). The port uses real ES imports, an imported SVG
  asset, and — appropriate to this admin surface — a footer showing the **signed-in user's email + Sign out**.
  The sidebar CSS/structure/nav sections are preserved verbatim.
- **Logo assets** (`assets/logo-lockup.svg`, `logo-mark.svg`) ported verbatim into `web/src/assets/`.

The `design-sync` skill can reconcile the local library with the Claude Design project later (e.g. to pull the
remaining ~18 primitives + the `BacktestResearch` kit for QE-259). No reconstruction was needed — this is a
faithful port of the real tokens/primitives/AppShell.

## 3. Decisions

| # | Decision | Rationale |
|---|----------|-----------|
| Toolchain | **Vite + React 19 + TypeScript** under `web/` | Ticket prefers TS; the design primitives are already ES-module React, so TS adds typing with no structural change. `web/` is outside the cargo workspace (no `cargo metadata` impact). |
| Build output | Vite `build.outDir` = `dist` (i.e. **`web/dist`**) | Point the server at it with `QE_SERVER_STATIC_DIR=web/dist` (or an absolute path). Documented in `web/README.md`. |
| Fonts | **Self-hosted** via `@fontsource/*`, imported in `main.tsx` | No runtime CDN (strict prod origin); reproducible; `.woff2` bundled by Vite. |
| Icons | **`lucide-react`** (bundled), same `Icon name="…"` API | No runtime CDN; tree-shaken. |
| Routing | **State-based nav** (no react-router) in `App`/`AppShell` | v1 only needs Login vs Shell + a Research active item; screens are placeholders (QE-259). Fewer deps = less surface. |
| Rejection state | Login reads `?error=` query param | The callback returns 403 server-side; the SPA-facing signal is finalised with QE-259. v1 reads a query param and renders the allowlist-rejection Callout. |
| Tests | **Vitest + React Testing Library + jsdom** | Standard Vite testing stack; component/render checks per §9. |
| Lint | **ESLint (flat config)** + `typescript-eslint` + `react-hooks`/`react-refresh` | `npm run lint` gate. |
| CI | New **`.github/workflows/frontend.yml`** (separate from Rust `ci.yml`) | Runs `npm ci → lint → build → test` on PRs/main. Not a required check until an admin marks it. |

## 4. Test plan

Frontend (Vitest + RTL), per acceptance criteria & §9:
1. **Unauthenticated load ⇒ Login.** Mock `GET /api/me` → 401; `App` renders the Login screen (brand lockup +
   "Sign in with Google").
2. **Mocked session ⇒ AppShell + Research nav.** Mock `GET /api/me` → 200 `{email}`; `App` renders the shell
   with the Research group (Strategies · Backtests · Market data) and the user's email in the footer.
3. **Trade/Risk disabled.** In the shell, the Trade group items (Dashboard/Positions/Orders) and Risk are
   rendered but `disabled` (present-but-disabled placeholders).
4. **Login rejection state.** Login rendered with the rejection indicator shows the allowlist-rejection Callout.
5. **Sign-in navigation.** Clicking "Sign in with Google" targets `/api/auth/login`.
6. **Primitive smoke.** Button/Badge/Callout render with their design classNames.

Rust green gate (must stay green — no Rust changed): `fmt --check`, `clippy -D warnings`, `test --workspace`,
firewall test, `cargo deny check`. `web/` is outside the workspace so `cargo metadata` / the decoupling &
firewall guards are unaffected — double-checked by running the firewall test.

## 5. Risks

- **Rejection signal wiring.** The precise callback→SPA rejection handshake (query param vs redirect vs page)
  is a QE-256/QE-259 detail; v1 reads `?error=` and renders the message. Low risk — cosmetic to rewire.
- **`node_modules`/build output.** Must be git-ignored **before** staging the big `web/` tree (`.gitignore`
  updated first). `package-lock.json` is committed for reproducible `npm ci`.
- **New Node toolchain in a Rust repo.** Isolated under `web/`; the Rust gates are untouched. A separate CI job
  protects the SPA build going forward.
- **Design drift.** This is a point-in-time port; the `design-sync` skill reconciles later. Only two files
  deviate from verbatim (fonts → self-host, Icon → bundled Lucide) — both required by the no-CDN constraint and
  documented above.
