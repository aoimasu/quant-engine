# Quant Engine — Admin SPA (`web/`)

Vite + React + TypeScript single-page app for the Quant Engine admin UI. The design system (tokens,
primitives, `AppShell`) is a faithful port of the Claude Design **"Quant Engine Design System"** project
(pulled via the `DesignSync` tool / `design-sync` skill). Fonts are self-hosted (no runtime CDN) and Lucide
icons are bundled via `lucide-react`.

QE-258 scope: scaffold + design-system port + **Login** + **AppShell** (Research nav active; Trade/Risk
disabled placeholders). The Backtests/Market-data/Strategies screens themselves land in QE-259 — here they are
empty placeholder routes.

## Prerequisites

- Node ≥ 20 (developed on Node 24) and npm ≥ 10.

## Commands

```bash
cd web
npm ci            # reproducible install from package-lock.json (use `npm install` to (re)generate the lock)
npm run dev       # Vite dev server (proxies /api → http://127.0.0.1:8080 for same-origin cookie auth)
npm run build     # type-check (tsc -b) + production build → web/dist
npm run preview   # serve the built web/dist locally
npm run lint      # ESLint (flat config)
npm test          # Vitest (jsdom) component/render checks
```

## How `qe-server` serves this SPA

`qe-server` (QE-254) serves a static SPA at `/` with an `index.html` fallback and reserves `/api`. The static
root is the env var **`QE_SERVER_STATIC_DIR`**. After `npm run build`, point the server at the build output:

```bash
# from the repo root, after `cd web && npm run build`
QE_SERVER_STATIC_DIR=web/dist cargo run -p qe-server
# (or an absolute path: QE_SERVER_STATIC_DIR=/abs/path/to/web/dist)
```

The Vite `build.outDir` is `dist` (relative to `web/`), i.e. **`web/dist`**. Deep links resolve to
`index.html` (SPA fallback); the SPA and `/api` are same-origin so the signed session cookie just works.

`node_modules/` and `dist/` are git-ignored (see the repo root `.gitignore`); `package-lock.json` **is**
committed so `npm ci` is reproducible.

## Auth flow (integrates with QE-256)

- On load the app calls `GET /api/me`. `401` → **Login**; `200 {email}` → **AppShell**.
- "Sign in with Google" navigates the browser to `GET /api/auth/login` (top-level redirect — the OAuth dance
  needs a full navigation, not `fetch`).
- Allowlist rejection: a valid Google login not on `QE_ADMIN_ALLOWED_EMAILS` yields a server-side `403`. The
  SPA renders the rejection state when loaded with `?error=forbidden` (`|rejected|403|not_allowed`). The exact
  callback→SPA rejection signal is finalised with QE-259 (see
  `docs/architecture/qe-258-frontend-scaffold-design.md`).

## Layout

```
web/
  index.html
  src/
    main.tsx                # mounts <App/>, imports fonts + global.css
    styles/                 # tokens/*.css (verbatim port) + global.css + fonts.ts (self-hosted @fontsource)
    design/                 # ported primitives (Button, Icon, Badge, Callout, Card) + AppShell
    app/                    # App (session gate), Login, Placeholder + tests
    api/session.ts          # /api/me, /api/auth/login, /api/auth/logout
    assets/                 # logo-lockup.svg, logo-mark.svg
```

## Keeping the design system in sync

This is a point-in-time port. Run the `design-sync` skill to reconcile the local library against the Claude
Design project (e.g. to pull the remaining primitives + the `BacktestResearch` kit for QE-259). Two files
intentionally deviate from a verbatim copy, both to satisfy the no-runtime-CDN constraint: `styles/fonts.ts`
(self-hosted `@fontsource` instead of the Google Fonts `@import`) and `design/Icon.tsx` (`lucide-react` instead
of the CDN `window.lucide`).
