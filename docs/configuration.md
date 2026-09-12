# Configuration

Configuration is loaded from a TOML file (default: `config.toml`). After loading, the `GREEN_DB_URL` environment variable overrides `auth.db_url` if set — this is how sops-nix injects the credential in production without it appearing in the Nix store.

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
admin_users = ["alice"]            # usernames that receive the admin role
ntfy_url = "https://ntfy.example.com/my-secret-topic"  # optional; recovery codes sent here

[mqtt]
host = "localhost"
port = 1883
# username = ""
# password set via GREEN_MQTT_PASSWORD env var

# Each integration declares a topic pattern.
# {device} captures the device ID; * matches one segment; ** matches zero or more.
[[mqtt.integrations]]
pattern = "zigbee2mqtt/{device}/**"

[[mqtt.integrations]]
pattern = "homeassistant/*/{device}/**"
name = "Home Assistant"   # optional display name; defaults to first literal segment
```

`GREEN_MQTT_PASSWORD` env var sets the broker password (same injection mechanism as `GREEN_DB_URL`).

See `config.toml.example` in the repo root for a complete, up-to-date annotated example.

## Dev config

The dev config (`config.dev.toml`) has `vault_path`, `recipe_vault_path`, `blog_vault_path` (all pointing at `fixtures/vault`), `about_path`, a real `rp_origin`, and `ntfy_url` for local development. The plaintext `db_url` is acceptable in dev; production uses the `GREEN_DB_URL` env var via sops-nix.

`[auth] rp_id`/`rp_origin` in `config.dev.toml` are pinned to the dev machine's Tailscale hostname so passkey auth works from other tailnet devices — see [development.md § Remote access over Tailscale](development.md#remote-access-over-tailscale).

## Secrets

Runtime secrets go in `secrets.toml` (gitignored, copy from `secrets.toml.example`) and are loaded as environment variables by the dev scripts:

| Env var | Overrides |
|---|---|
| `GREEN_DB_URL` | `auth.db_url` |
| `GREEN_MQTT_PASSWORD` | `mqtt.password` |

In production these are injected via sops-nix instead — see [nixos-module.md](nixos-module.md).
