# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Project Overview

Green is a Rust-based HTTP server that provides an internal routing/landing page for home services. It serves as a central hub displaying links to various self-hosted services (Foundry VTT, AdGuard, Grafana, PostgreSQL, Home Assistant, Frigate, etc.) and provides a CA certificate endpoint for internal TLS.

## Build & Development Commands

### Dev Server Lifecycle

The dev server runs as a **detached OS process** (via `setsid --fork`) that outlives any
nushell session — including the Claude Code Bash tool. This means you can start, stop, or
restart the server from any terminal, any Zellij pane, or any Bash tool invocation.

**All server management goes through `scripts/green.nu`.** Do not use `cargo run` directly
for development; it won't set credentials or redirect logs correctly.

```nu
# Load the commands into any nushell session
use scripts/green.nu *

# Start the server (builds if needed, truncates logs, detaches)
green start

# Stop the running server
green stop

# Rebuild and restart (the typical workflow after making code changes)
green restart

# Run in the foreground (useful when you want live stdout in the terminal)
green run
```

From the **Claude Code Bash tool** (which uses nushell):
```nu
nu --no-config-file -c "use scripts/green.nu *; green restart"
```

#### How it works

`green start` uses `setsid --fork` to place the server in a new OS session, making it a
child of PID 1. It survives when the calling session exits. `green stop` uses
`pkill --signal SIGTERM --full <abs-config-path>` to find and kill all matching processes
by their full command line — this works across sessions and nushell instances.

**Important:** `green stop` matches processes by the **absolute** config file path. Always
use `green start`/`green stop` rather than launching `cargo run` manually, or the stop
command won't find the process.

#### Log files

Both files are truncated on every `green start` so `tail -f` always reflects the current run:

| File | Contents |
|------|----------|
| `logs.ndjson` | Structured JSON tracing output from the running server (stdout) |
| `errors.log` | `cargo build` output, panics, and server stderr |

#### Sentinel file

`.watch_state.toml` (gitignored) records `config_path` and `started_at`. Its presence
indicates a server is running; `green start` stops any existing instance before starting a
new one; `green stop` removes it.

#### Typical workflow

```
# Initial setup (from the "phone" Zellij session — errors tab runs this automatically):
nu scripts/dev.nu          # starts server + tails errors.log

# After editing code:
nu --no-config-file -c "use scripts/green.nu *; green restart"

# Check if the server is up:
curl http://localhost:10000/healthcheck
```

#### Zellij "phone" session

The project includes a Zellij layout for remote development from iOS (via Termion/SSH):
```
zellij --session phone --layout scripts/phone.kdl
```

Three tabs:
- **claude** — Claude Code (focused by default)
- **errors** — runs `nu scripts/dev.nu`; starts the server then tails `errors.log`
- **logs** — `tail -f logs.ndjson` (structured tracing from the running binary)

### Build and Run (direct)
```bash
# Build the project
cargo build

# Run the server (uses config.toml by default)
cargo run

# Run with custom config
cargo run -- --config-path /path/to/config.toml

# Build release version
cargo build --release
```

### Testing
```bash
# Run all Rust tests
cargo test
just test

# Run JS tests (zero npm deps, uses node:test)
npm test
```

### Nix Development
```bash
# Enter development shell (provides rust toolchain, just, etc.)
nix develop

# Build with Nix
nix build

# Run NixOS VM test
nix build .#checks.x86_64-linux.green
```

### Code Quality
```bash
# Format code
cargo fmt

# Run linter
cargo clippy

# Fix typos (via typos CLI in devShell)
typos
```

## Architecture

### HTTP Routes
The application has two types of routes:

1. **Static Routes** (defined in `Route` enum in main.rs):
   - `/` - Index page showing all configured routes
   - `/api/ca` - Returns the CA certificate content
   - `/healthcheck` - Health check endpoint
   - `/assets` - Static file serving
   - `/auth/login`, `/auth/register` - Passkey auth pages
   - `/auth/recover` - Account recovery via ntfy OTC
   - `/breaker` - Breaker box panel (GM only)
   - `/tailscale` - Tailscale peer list (GM only)
   - `/notes`, `/notes/{slug}` - Notes vault pages

2. **Dynamic Routes** (configured via TOML):
   - Defined in config.toml under `[routes.*]` sections
   - Each route has `url` and `description` fields
   - Displayed on the index page as links

### Module Structure

- `main.rs` - Application entry point, server setup, CLI, and configuration
  - `ServerState` - Shared state containing CA certificate, index page, auth state
  - `Config` - TOML configuration structure; supports `GREEN_DB_URL` env var override for `auth.db_url`
  - `Route` enum - Static route definitions

- `auth.rs` - WebAuthn / passkey authentication
  - `AuthConfig` - Config struct (`rp_id`, `rp_origin`, `db_url`, `gm_users`, `ntfy_url`)
  - `AuthState` - Shared state: DB pool, session store, reg/auth/OTC challenge stores, reusable HTTP client
  - Extractors: `AuthUser`, `GmUser`, `MaybeAuthUser`
  - Handlers: login/register challenge+finish, logout, **recovery** (GET+POST `/auth/recover`, POST `/auth/recover/verify`)
  - Recovery: generates a 6-char A–Z0–9 OTC (rejection-sampling, no modulo bias), stores it with a 10-minute TTL, sends it via ntfy, then verifies atomically and invalidates all existing sessions

- `route.rs` - Dynamic route types (`Routes`, `RouteInfo`)
- `index.rs` - Index page template and handler
- `error.rs` - Application error types with `IntoResponse` implementation
- `io.rs` - File I/O utilities (async file reading, TOML loading)
- `notes.rs` - Notes vault scanning, slug types, secret redaction
- `breaker.rs` / `breaker_detail.rs` - Breaker box panel rendering
- `tailscale.rs` - Tailscale peer list via Unix socket
- `qr.rs` - QR code generation

### Configuration

Configuration is loaded from a TOML file (default: `config.toml`). After loading, `GREEN_DB_URL` environment variable overrides `auth.db_url` if set — this is how sops-nix injects the credential in production without it appearing in the Nix store.

```toml
port = 10000
ca_path = "path/to/ca.pem"
log_level = "info"  # or "debug", "warn", etc.

[routes.service_name]
url = "service.example.com"
description = "Service description"

[auth]
rp_id = "example.com"              # WebAuthn relying party ID
rp_origin = "https://green.example.com"
db_url = "postgres://green:pass@localhost/green"  # overridable by GREEN_DB_URL env var
gm_users = ["alice"]               # usernames that receive the GM role
ntfy_url = "https://ntfy.example.com/my-secret-topic"  # optional; recovery codes sent here
```

The dev config (`config.dev.toml`) has `vault_path`, real `rp_origin`, and `ntfy_url` for local development. The plaintext `db_url` is acceptable in dev; production uses the `GREEN_DB_URL` env var via sops-nix.

### Template Rendering

Uses Askama template engine with templates in `templates/` directory. All template structs carry `auth_user: Option<AuthUserInfo>` required by `base.html`. The index page is pre-rendered at startup.

### JavaScript

JS files live in `assets/js/`. Each auth flow has a paired module:
- `auth-login.js` / `test/js/auth-login.test.js`
- `auth-register.js` / `test/js/auth-register.test.js`
- `auth-recover.js` / `test/js/auth-recover.test.js`

Pattern: pure exported functions with injected deps (`fetch`, `startAuthentication`, etc.) for testability; DOM binding block at bottom guarded by `typeof document !== 'undefined'`. Tests use `node:test` with zero npm deps.

## NixOS Module

The flake provides a NixOS module at `nixosModules.default` for deploying as a systemd service:
- Service runs as `green` user by default
- Configuration generated at `/etc/green/config.toml`
- Extensive systemd hardening measures applied
- State directory at `/var/lib/green` by default

### Module Options

| Option | Type | Default | Description |
|--------|------|---------|-------------|
| `port` | port | 47336 | Listen port |
| `caPath` | path | — | Path to CA certificate |
| `logLevel` | str | `"info"` | tracing env-filter string |
| `routes` | attrsOf submodule | (built-in defaults) | Dynamic route list |
| `dataDir` | path | `/var/lib/green` | State directory |
| `auth.rpId` | str | — | WebAuthn RP ID |
| `auth.rpOrigin` | str | — | WebAuthn origin URL |
| `auth.dbUrl` | str | — | Postgres connection URL (put a placeholder; use `dbUrlFile` in prod) |
| `auth.gmUsers` | listOf str | `[]` | Usernames with GM role |
| `auth.ntfyUrl` | str or null | null | ntfy topic URL for recovery codes |
| `auth.dbUrlFile` | path or null | null | Path to EnvironmentFile containing `GREEN_DB_URL=…`; overrides `dbUrl` at runtime |

When `auth.dbUrlFile` is set, the systemd unit gets `EnvironmentFile = <path>`, and `GREEN_DB_URL` in that file overrides `auth.db_url` from config.toml. This is how sops-nix integration works.

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
