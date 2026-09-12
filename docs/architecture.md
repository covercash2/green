# Architecture

## HTTP Routes

The application has two types of routes.

The public surface and the admin surface are deliberately separated: everything homelab-oriented lives under the `/admin` prefix and is gated by `AdminUser`, while `/` and its siblings are safe to show an anonymous visitor.

1. **Public routes** (defined in the `Route` enum in `main.rs`, plus a few raw-string routes in `build_router`):
   - `/` - Public landing page (hero, blurb, recent blog post teasers, links to `/blog`/`/recipes`/`/about`)
   - `/blog`, `/blog/{slug}` - Blog post index and detail pages (vault-scanned Markdown, tagged `blog`)
   - `/about` - About-me page (single Markdown file, rendered once at startup)
   - `/recipes`, `/recipes/{slug}` - Recipe vault pages (vault-scanned Markdown, tagged `recipe`)
   - `/notes`, `/notes/{slug}` - D&D campaign notes vault pages (public but unlisted — not linked from the landing page, but not admin-gated either, since non-admin players need it)
   - `/qr`, `/api/qr` - QR code generator (unauthenticated; generic public utility, not homelab info)
   - `/api/ca` - Returns the CA certificate content
   - `/healthcheck` - Health check endpoint
   - `/metrics` - Prometheus scrape endpoint (unauthenticated)
   - `/assets` - Static file serving
   - `/auth/login`, `/auth/register` - Passkey auth pages
   - `/auth/recover` - Account recovery via ntfy OTC
   - `/webhook` - GitHub deployment webhook (HMAC-verified, not admin-gated)

2. **Admin routes** (admin only, all under `/admin`):
   - `/admin` - Consolidated dashboard: local systemd services, peer service groups, and every homelab-infra link (config-driven `[routes.*]` entries plus breaker/tailscale/mqtt/logs)
   - `/admin/breaker`, `/admin/api/breaker/{key}` - Breaker box panel
   - `/admin/tailscale` - Tailscale peer list
   - `/admin/mqtt`, `/admin/mqtt/devices` - Live MQTT feed and device inventory (SSE stream at `/admin/api/mqtt/stream`)
   - `/admin/logs/app`, `/admin/logs/errors` - Dev log viewers (SSE streams under `/admin/api/logs/*`)
   - `/admin/services` - Systemd service status dashboard (browser page)
   - `/api/services` - JSON API for the above; **not** under `/admin` because it's called machine-to-machine between peer Green instances (`AdminOrPeer` extractor: admin session cookie or shared `X-Green-Api-Key`) and the URL suffix is hardcoded per-peer, not configurable — renaming it would require redeploying every peer simultaneously

3. **Dynamic routes** (configured via TOML):
   - Defined in `config.toml` under `[routes.*]` sections
   - Each route has `url` and `description` fields
   - Displayed on the admin dashboard as links (not the public landing page)

## Module Structure

- `main.rs` - Application entry point, server setup, CLI, and configuration
  - `ServerState` - Shared state containing CA certificate, index page, auth state
  - `Config` - TOML configuration structure; supports `GREEN_DB_URL` env var override for `auth.db_url`
  - `Route` enum - Static route definitions

- `auth.rs` - WebAuthn / passkey authentication
  - `AuthConfig` - Config struct (`rp_id`, `rp_origin`, `db_url`, `admin_users`, `ntfy_url`)
  - `AuthState` - Shared state: DB pool, session store, reg/auth/OTC challenge stores, reusable HTTP client
  - Extractors: `AuthUser`, `AdminUser`, `MaybeAuthUser`
  - Handlers: login/register challenge+finish, logout, **recovery** (GET+POST `/auth/recover`, POST `/auth/recover/verify`)
  - Recovery: generates a 6-char A–Z0–9 OTC (rejection-sampling, no modulo bias), stores it with a 10-minute TTL, sends it via ntfy, then verifies atomically and invalidates all existing sessions

- `route.rs` - Dynamic route types (`Routes`, `RouteInfo`)
- `index.rs` - Public landing page template and handler
- `admin.rs` - Admin dashboard template and handler (moved off the landing page)
- `blog/mod.rs` - Blog vault scanning and routes (mirrors `notes/recipes.rs`)
- `about.rs` - About-me page (single Markdown file, rendered once at startup)
- `error.rs` - Application error types with `IntoResponse` implementation
- `io.rs` - File I/O utilities (async file reading, TOML loading)
- `notes.rs` - Notes vault scanning, slug types, secret redaction
- `breaker.rs` / `breaker_detail.rs` - Breaker box panel rendering
- `tailscale.rs` - Tailscale peer list via Unix socket
- `qr.rs` - QR code generation

For the TOML configuration format itself, see [configuration.md](configuration.md).

## Template Rendering

Uses Askama template engine with templates in `templates/` directory. All template structs carry `auth_user: Option<AuthUserInfo>` required by `base.html`. The index page is pre-rendered at startup.

## JavaScript

**No inline `<script>` blocks in templates.** All JS lives in the pipeline:

```
src/js/*.ts  →  just build-js  →  assets/js/*.js  ←  <script type="module" src="...">
     ↑
test/js/*.test.ts  (deno test, no browser)
```

Every page that needs JS gets a paired module:
- `src/js/<page>.ts` — TypeScript source; pure exported functions + DOM binding block
- `test/js/<page>.test.ts` — Deno tests importing directly from the `.ts` source
- `assets/js/<page>.js` — compiled output (committed); loaded by the template

Pattern: pure exported functions with injected deps (`fetch`, etc.) for testability; DOM binding block at bottom guarded by `typeof document !== 'undefined'`. When adding JS to a template, create the `.ts` source and tests first, then run `just build-js`.

## Key Dependencies

- **axum** - Web framework
- **tokio** - Async runtime
- **tower/tower-http** - Middleware and services
- **askama** - Template engine
- **serde/toml** - Configuration parsing
- **tracing** - Structured logging (JSON format)
- **clap** - CLI argument parsing
- **webauthn-rs** - WebAuthn / passkey implementation
- **sqlx** - Async PostgreSQL client; migrations in `migrations/`
- **axum-extra** - Cookie jar support
- **reqwest** - HTTP client for ntfy notifications (instance shared in `AuthState`)
- **uuid** - Session tokens and user IDs
- **pulldown-cmark** - Markdown rendering (notes vault, breaker box)

## Development Notes

- Uses Rust 2024 edition
- All routes are async handlers
- Tracing is configured for JSON output in production
- CA certificate is loaded once at startup and shared via Arc
- Index page is pre-rendered at startup for efficiency
- Static assets expected in `assets/` directory
- `GREEN_DB_URL` env var overrides `auth.db_url` from config — set by sops-nix EnvironmentFile in production
