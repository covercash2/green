# green

Home services hub — a self-hosted landing page and internal tooling server.

## What it does

- **Landing page** — links to all self-hosted services, configured in TOML
- **MQTT feed** — live message stream from home-automation brokers; per-device history and a publish form
- **Device inventory** — tracks which devices have appeared on each MQTT integration
- **Notes vault** — renders an Obsidian-style Markdown vault, filtered by tag
- **Breaker box** — visual breaker panel rendered from Markdown
- **Passkey auth** — WebAuthn login; GM role gates privileged pages
- **Account recovery** — one-time codes delivered via [ntfy](https://ntfy.sh)
- **Prometheus metrics** — `/metrics` endpoint for MQTT message counters
- **CA endpoint** — `/api/ca` serves the internal CA certificate

## Quick start

```bash
# Enter the dev shell (provides Rust toolchain, deno, just, etc.)
nix develop

# Copy and edit the example config
cp config.toml.example config.dev.toml
cp secrets.toml.example secrets.toml
$EDITOR config.dev.toml secrets.toml

# Install git hooks
just install-hooks

# Start the dev server (detached, logs to logs.ndjson / errors.log)
use scripts/green.nu *
green start

# Run tests
just test-all
```

## Configuration

See `config.toml.example` for a full annotated example.
Runtime secrets go in `secrets.toml` (gitignored) and are injected as env vars:

| Env var | Overrides |
|---|---|
| `GREEN_DB_URL` | `auth.db_url` |
| `GREEN_MQTT_PASSWORD` | `mqtt.password` |

## Development

```bash
just test          # Rust tests
just js-test       # JS/TS tests (deno)
just coverage      # Rust tests + 70 % line-coverage check
just build-js      # compile src/js/*.ts → assets/js/*.js
just lint-js       # biome lint
```

See `CLAUDE.md` for detailed architecture notes, module structure, and the JS pipeline convention.

## Deployment

A NixOS module is provided at `nixosModules.default`. See `CLAUDE.md` → *NixOS Module* for options.

## Tech stack

Rust · axum · tokio · askama · sqlx (PostgreSQL) · webauthn-rs · rumqttc · prometheus · Deno · TypeScript
