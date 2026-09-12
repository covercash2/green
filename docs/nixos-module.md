# NixOS Module

The flake provides a NixOS module at `nixosModules.default` for deploying as a systemd service:
- Service runs as `green` user by default
- Configuration generated at `/etc/green/config.toml`
- Extensive systemd hardening measures applied
- State directory at `/var/lib/green` by default

## Module Options

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
| `auth.adminUsers` | listOf str | `[]` | Usernames with admin role |
| `auth.ntfyUrl` | str or null | null | ntfy topic URL for recovery codes |
| `auth.dbUrlFile` | path or null | null | Path to EnvironmentFile containing `GREEN_DB_URL=…`; overrides `dbUrl` at runtime |

When `auth.dbUrlFile` is set, the systemd unit gets `EnvironmentFile = <path>`, and `GREEN_DB_URL` in that file overrides `auth.db_url` from `config.toml`. This is how sops-nix integration works.
