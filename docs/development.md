# Development

## Dev Server Lifecycle

The dev server runs as a **detached OS process** (via `setsid --fork`) that outlives any
nushell session — including a Claude Code Bash tool invocation. This means you can start,
stop, or restart the server from any terminal, any Zellij pane, or any Bash tool invocation.

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

From a non-interactive shell (e.g. the Claude Code Bash tool, which uses nushell):
```nu
nu --no-config-file -c "use scripts/green.nu *; green restart"
```

### How it works

`green start` uses `setsid --fork` to place the server in a new OS session, making it a
child of PID 1. It survives when the calling session exits. `green stop` uses
`pkill --signal SIGTERM --full <abs-config-path>` to find and kill all matching processes
by their full command line — this works across sessions and nushell instances.

**Important:** `green stop` matches processes by the **absolute** config file path. Always
use `green start`/`green stop` rather than launching `cargo run` manually, or the stop
command won't find the process.

### Log files

Both files are truncated on every `green start` so `tail -f` always reflects the current run:

| File | Contents |
|------|----------|
| `logs.ndjson` | Structured JSON tracing output from the running server (stdout) |
| `errors.log` | `cargo build` output, panics, and server stderr |

### Sentinel file

`.watch_state.toml` (gitignored) records `config_path` and `started_at`. Its presence
indicates a server is running; `green start` stops any existing instance before starting a
new one; `green stop` removes it.

### Typical workflow

```
# Initial setup (from the "phone" Zellij session — errors tab runs this automatically):
nu scripts/dev.nu          # starts server + tails errors.log

# After editing code:
nu --no-config-file -c "use scripts/green.nu *; green restart"

# Check if the server is up:
curl http://localhost:10000/healthcheck
```

### Zellij "phone" session

The project includes a Zellij layout for remote development from iOS (via Termion/SSH):
```
zellij --session phone --layout scripts/phone.kdl
```

Three tabs:
- **claude** — Claude Code (focused by default)
- **errors** — runs `nu scripts/dev.nu`; starts the server then tails `errors.log`
- **logs** — `tail -f logs.ndjson` (structured tracing from the running binary)

### Remote access over Tailscale

The dev machine is `hoss` (tailnet `faun-truck.ts.net`). The server already binds
`0.0.0.0`, so once it's running it's reachable from any tailnet device at
`http://hoss:10000` (MagicDNS) — good enough for browsing public pages (blog,
recipes, about) from another device, e.g. an iPad.

**Passkey auth needs more than that**, though: WebAuthn requires a secure
context, so login/registration only works over HTTPS (or `localhost`). To get
real HTTPS on the tailnet, publish the dev server with `tailscale serve`:

```nu
use scripts/green.nu *
green serve           # -> https://hoss.faun-truck.ts.net  (proxies to localhost:<port>)
green serve status    # show current tailscale serve config
green serve stop      # tear it down (tailscale serve reset)
```

`tailscale serve` needs root by default. Run this once so it doesn't prompt
for a password every time:
```bash
sudo tailscale set --operator=$USER
```

`config.dev.toml`'s `[auth] rp_id`/`rp_origin` are already pinned to
`hoss.faun-truck.ts.net` to match. If you rename the tailnet, move to a
different dev machine, or switch to a real domain (e.g. via AdGuard DNS
rewrite + Caddy reverse proxy with a DNS-01 cert), update all three of:
`TAILNET_ADDRESS` in `scripts/green.nu`, `[auth] rp_id`, and `[auth]
rp_origin` in `config.dev.toml` — then re-register passkeys, since they're
bound to the old `rp_id` and won't carry over.

## Build and Run (direct)
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

## Testing
```bash
# Run all Rust tests
cargo test
just test

# Run JS tests (zero npm deps, uses node:test)
npm test
```

## Nix Development
```bash
# Enter development shell (provides rust toolchain, just, etc.)
nix develop

# Build with Nix
nix build

# Run NixOS VM test
nix build .#checks.x86_64-linux.green
```

## Code Quality
```bash
# Format code
cargo fmt

# Run linter
cargo clippy

# Fix typos (via typos CLI in devShell)
typos
```

## Git Push Hook

A pre-push hook lives in `scripts/hooks/pre-push`. After cloning (or after any teammate adds/updates the hook), install it:

```bash
just install-hooks
```

The hook runs `just _pre-push-checks` inside `nix develop`, which covers:

1. `cargo fmt --check` — formatting
2. `cargo clippy -- -D warnings` — lints
3. `just coverage` — Rust tests + 70% line-coverage threshold
4. `deno check src/js/` — TypeScript type-check
5. `just js-test` — JS unit tests
