# Multi-Machine Support

Green should work across multiple NixOS machines. Each machine runs its own
instance of the server, but they share a single PostgreSQL database for auth.
Features are enabled per-machine via NixOS module options. Privileged (GM)
users can navigate between machines and monitor services across the fleet.

## Decisions

- **Auth**: single shared PostgreSQL DB. All machines point at it. Passkeys
  are portable because `rp_id = "chrash.net"` is shared.
- **Peers**: GM-only throughout — consistent with local service monitoring.
- **Transport**: peer-to-peer HTTP between instances (no shared substrate).
- **Cross-domain auth**: server-side proxy with pre-shared secret. Machine A's
  server calls B's `/api/services` directly, passing `X-Green-Api-Key`. No
  browser CORS involvement. See Stage 3 for full details.

---

## Stage 1 — NixOS Feature Flags

> Status: **complete**

Features are implicitly enabled by the presence of config sections in
`config.toml` (e.g. `[mqtt]` enables the MQTT pages, `[systemd]` enables the
services dashboard). The NixOS module exposes explicit option groups so each
machine's configuration is self-documenting.

**Scope**: `flake.nix` NixOS module + `dotfiles/modules/green.nix`. No Rust
changes.

### What was added

| NixOS option | config.toml section generated | Controls |
|---|---|---|
| `services.green.systemd.units` | `[[systemd.units]]` table array | Services dashboard |
| `services.green.mqtt.*` | `[mqtt]` + `[[mqtt.integrations]]` | MQTT pages (pre-existing) |
| `services.green.vaultPath` | `vault_path = "…"` | Notes vault (pre-existing) |

`services.green.systemd` is a `nullOr submodule`. When set to `null` (the
default), no `[[systemd.units]]` lines are generated and the services
dashboard returns 404.

### `RestrictAddressFamilies` fix

The systemd hardening in the NixOS module now includes `AF_UNIX` alongside
`AF_INET` and `AF_INET6`. This is required for the server to communicate with
systemd over its D-Bus socket when querying unit status via `systemctl show`.

### Production config (`dotfiles/modules/green.nix`)

```nix
services.green.systemd = {
  units = [
    { name = "postgresql";  url = "https://db.green.chrash.net"; }
    { name = "grafana";     url = "https://grafana.green.chrash.net"; }
    { name = "prometheus";  url = "https://prometheus.green.chrash.net"; }
    { name = "home-assistant"; url = "https://homeassistant.green.chrash.net"; }
    { name = "mosquitto"; }
    { name = "caddy"; }
    { name = "pgadmin"; }
    { name = "jellyfin"; }
    { name = "ultron"; url = "https://ultron.green.chrash.net"; }
  ];
};
```

Units whose `url` matches a `routes.*` URL are automatically removed from the
routes list on the home page (deduplication logic in `src/index.rs`).

---

## Stage 2 — Peer Registry

> Status: **complete**

Each instance knows about the others and links to them in the nav drawer
(GM-only). No live status — static links only.

**Scope**: Rust server + NixOS module.

### What was added

**`src/main.rs`**
- `PeerInfo { name, url, api_key }` — the `api_key` field is optional and
  unused until Stage 3; it is already present so Stage 3 requires no struct
  changes.
- `Config.peers: Vec<PeerInfo>` — TOML `[[peers]]` table array.
- `ServerState.peers: Arc<[PeerInfo]>` — for use in handlers.

**`src/index.rs`**
- `NavLink.is_gm: bool` — when `true`, the link is only rendered in the nav
  drawer for users whose `AuthUserInfo::is_gm()` returns true.
- Peer entries are appended to `nav_links` at startup with `is_gm: true`.

**`templates/base.html`**
- Nav drawer conditionally renders `is_gm` links:
  ```html
  {% if !link.is_gm %}…{% else %}{% if let Some(u) = auth_user %}{% if u.is_gm() %}…{% endif %}{% endif %}{% endif %}
  ```
- Peer links get the `.nav-drawer-link-peer` CSS class (accent colour, top border).

### Config shape

```toml
[[peers]]
name = "orion"
url  = "https://green.orion.chrash.net"
# api_key = "…"   ← add when Stage 3 is deployed
```

### NixOS module option

```nix
services.green.peers = [
  { name = "orion"; url = "https://green.orion.chrash.net"; }
];
```

### Acceptance criteria

- Peers appear in the nav drawer for GM users only. ✓
- Non-GM users see no peers in the UI. ✓

---

## Stage 3 — Remote Service Monitoring

> Status: **complete** · deployment pending (see checklist below)

Aggregate `/api/services` from configured peers on the home page. Services
grouped by machine (local first, then each peer).

**Scope**: Rust server + NixOS module.

### How it works (auth flow)

```
Browser (GM user on A)
    │
    │  GET /  (session cookie for A)
    ▼
Green A  ──── local systemctl ───────────────► Vec<ServiceStatus>
    │
    │  GET https://B/api/services             (server-to-server, not browser)
    │  Header: X-Green-Api-Key: <api_key>     (from A's [[peers]] config)
    │  Timeout: 5 s
    ▼
Green B  ──── validates header ──────────────► Vec<ServiceStatus> (JSON)
    │
    │  (if unreachable / timeout / non-2xx)
    └──────────────────────────────────────── PeerServiceGroup { online: false }
```

The home page renders local services first, then one section per peer. A peer
that does not respond within 5 seconds is shown as "offline" — no crash, no
blocking.

Non-GM users skip all peer fetches entirely (the network calls are never made).

### Authentication model

Two paths reach `/api/services`, handled by the `GmOrPeer` extractor in
`src/services.rs`:

1. **GM session cookie** — existing `GmUser` path; used when a GM visits
   `/api/services` in their browser.

2. **Peer API key** — machine A sends `X-Green-Api-Key: <token>` in an HTTPS
   request. Machine B validates the token against its own `peer_api_key`
   config value (injected at runtime via `GREEN_PEER_API_KEY` env var).

Header check happens first: if `X-Green-Api-Key` is present but wrong, the
request is rejected with 403 immediately (no cookie fallback).

### What was added

**`src/services.rs`**
- `PeerServiceGroup { name, url, online, services }` — one group per peer.
- `GmOrPeer` extractor — see auth model above.
- `fetch_peer_services(peer, client) -> PeerServiceGroup` — makes the outbound
  HTTPS call with timeout and graceful error handling.
- `ServiceStatus` and `Health` now derive `Deserialize` (needed to parse peer
  JSON responses).
- `PEER_AUTH_HEADER` constant (`"X-Green-Api-Key"`).
- `PEER_FETCH_TIMEOUT` constant (5 seconds).
- Module-level doc comment with full ASCII flow diagram, auth rationale, and
  security notes.

**`src/main.rs`**
- `PeerInfo.api_key: Option<String>` — the secret A sends to B.
- `Config.peer_api_key: Option<String>` — the inbound secret B accepts.
- `ServerState.http_client: reqwest::Client` — shared client (connection
  pooling); never create per-request.
- `ServerState.peer_api_key: Option<Arc<str>>` — runtime key for inbound auth.
- `Config::load` reads `GREEN_PEER_API_KEY` env var and stores it in
  `Config.peer_api_key`.

**`src/index.rs`**
- `Index.peer_groups: Vec<PeerServiceGroup>` — filled per-request in the
  index handler.
- `index` handler fetches peer groups in parallel (only for GM users, only for
  peers with `api_key` set).

**`templates/index.html`**
- Renders peer group sections after local services, with machine name as heading
  and an "offline" badge when unreachable.

**`assets/css/services.css`**
- `.svc-peer-heading` — machine name heading between local and peer sections.
- `.svc-peer-offline` — muted "offline" label.
- `.svc-peer-grid` — grid wrapper for peer service cards.

**`flake.nix`**
- `services.green.peers[].apiKey` — per-peer outbound key (prefer env var).
- `services.green.peerApiKey` — inbound key (prefer env var).

### Security notes (read before deploying)

- Keys must be long random strings (≥ 32 bytes, base64-encoded).
  Generate with: `openssl rand -base64 32`
- Keys must **not** appear in `config.toml` in the Nix store. Use
  `GREEN_PEER_API_KEY` via sops-nix EnvironmentFile (same mechanism as
  `GREEN_DB_URL`).
- All communication is over HTTPS (Caddy + mkcert/Tailscale TLS).
- String comparison is plain `==` (not constant-time). Acceptable for this
  LAN-only threat model. If exposed to the internet, switch to `subtle::ConstantTimeEq`.
- A single `GREEN_PEER_API_KEY` value is used for both the outbound key (all
  peers share it) and the inbound key. If you need per-peer outbound keys,
  set `apiKey` per-peer in the NixOS module directly (accept that it lands in
  the store, or use sops template per-value).

---

## Deployment Checklist — Stage 3

These steps must be done **before** the feature is live. All the code is
merged; only secrets and config wiring remain.

### Step 1 — Generate a shared peer API key

On any machine (no special access needed):

```sh
openssl rand -base64 32
# example output: "wQ3+Kp/nZxLmR8VtAiYeDFjHbOcSGuM0qNeXW1TvP2k="
```

This single value is used on both sides: machine A sends it, machine B accepts
it.

### Step 2 — Add `peer_api_key` to `secrets/green.yaml`

Add the key alongside the existing secrets. **Must include all existing keys**
or they will be dropped from the file.

```sh
# Write ALL secrets as plaintext (update existing values if needed)
cat > /tmp/green-secrets-plain.yaml << 'EOF'
green_db_password: fEthOeLD4yuv8832E1EqjGhnaykvkLme0Kfi7QlV
pgadmin_password: 0FARA63sY3U6rqDkU09dBciyc0xfNPDY
mqtt_password: "PdkT/zT6E75GNnfkDqAf1DoK0PKpEUMZqZjoA2HiU0c="
peer_api_key: "<value from Step 1>"
EOF

# Encrypt in-place (PTY required — rops checks isatty(stdin))
script -q -c 'rops encrypt -i --age age1lvh945n6pxhwxqyrt6x5fcyvgeytnh4cg47zj2000ltmqal4xyjs0adv96 /tmp/green-secrets-plain.yaml && echo done' /dev/null

# Move into place
mv /tmp/green-secrets-plain.yaml ~/github/covercash2/dotfiles/secrets/green.yaml
```

### Step 3 — Declare the secret in `dotfiles/modules/sops.nix`

Add to the `sops` block:

```nix
secrets.peer_api_key = {
  sopsFile = ../secrets/green.yaml;
  owner = "green";
  group = "green";
  mode = "0400";
};
```

And add to the `green-env` EnvironmentFile template:

```nix
templates."green-env" = {
  content = ''
    GREEN_DB_URL=postgres://green:${config.sops.placeholder.green_db_password}@localhost:${builtins.toString config.services.postgresql.settings.port}/green
    GREEN_MQTT_PASSWORD=${config.sops.placeholder.mqtt_password}
    GREEN_PEER_API_KEY=${config.sops.placeholder.peer_api_key}
  '';
  # owner/group/mode unchanged
};
```

### Step 4 — Wire peers in `dotfiles/modules/green.nix`

Add `api_key` reference to the peer entries. Because the actual value comes
from the env var (not the Nix option), leave `apiKey` null here and rely on
`GREEN_PEER_API_KEY` to set it at runtime:

```nix
# The api_key field in [[peers]] is overridden at runtime by GREEN_PEER_API_KEY.
# Leave the NixOS option null to keep the secret out of the Nix store.
# The Rust server reads GREEN_PEER_API_KEY and applies it to peer_api_key (inbound)
# but NOT to [[peers]].api_key (outbound). For outbound, set api_key in the TOML
# directly or use a separate sops template line: PEER_<name>_API_KEY if peers
# need distinct keys.
#
# For now: both machines share one key via GREEN_PEER_API_KEY.
# - This machine (green) sends it as the outbound key → set api_key in [[peers]]
# - This machine (green) accepts it as the inbound key → set by env var automatically

peers = [
  # When ready to add a second machine:
  # { name = "orion"; url = "https://green.orion.chrash.net"; apiKey = null; }
  # Note: with one shared key, set api_key in config.toml via the sops template,
  # not via the NixOS option, to keep it out of the store.
];
```

> **Note on the current single-machine setup**: With only one machine deployed,
> Stage 3 has no visible effect yet. The peer sections will appear when a second
> machine with a green instance is configured and added to `peers`.

### Step 5 — Verify the Nix build

```sh
cd ~/github/covercash2/dotfiles
nix build .#nixosConfigurations.green.config.system.build.toplevel
```

### Step 6 — Deploy

```sh
sudo nixos-rebuild switch --flake ~/github/covercash2/dotfiles#green
```

### Step 7 — Smoke test

```sh
# Inbound: test that a correct key is accepted
curl -s -H "X-Green-Api-Key: $(cat /run/secrets/peer_api_key)" \
  http://localhost:47336/api/services | jq length

# Inbound: test that a wrong key is rejected (expect 403)
curl -o /dev/null -w "%{http_code}" \
  -H "X-Green-Api-Key: wrong" http://localhost:47336/api/services

# Outbound: add a peer with the key and check the home page as a GM user
# (requires a second machine — skip until then)
```

---

## Config reference

### Machine A — caller (fetches peer services)

```toml
[[peers]]
name    = "orion"
url     = "https://green.orion.chrash.net"
api_key = "wQ3+Kp/nZxLmR8VtAiYeDFjHbOcSGuM0qNeXW1TvP2k="
# In production: leave this blank and inject via sops template instead.
```

### Machine B — callee (serves peer requests)

```toml
peer_api_key = "wQ3+Kp/nZxLmR8VtAiYeDFjHbOcSGuM0qNeXW1TvP2k="
# In production: injected at runtime via GREEN_PEER_API_KEY env var.
# The value in config.toml can be empty ("") or omitted when env var is used.
```

### Environment variables (injected by sops-nix EnvironmentFile)

| Variable | Where used | Effect |
|---|---|---|
| `GREEN_DB_URL` | `Config::load` | Overrides `auth.db_url` |
| `GREEN_MQTT_PASSWORD` | `Config::load` | Overrides `mqtt.password` |
| `GREEN_PEER_API_KEY` | `Config::load` | Sets `peer_api_key` (inbound key) |

> **Gap**: `GREEN_PEER_API_KEY` currently only sets the **inbound** `peer_api_key`.
> The **outbound** `api_key` in `[[peers]]` must still be set in `config.toml`
> (or the NixOS `apiKey` option). If you want to keep the outbound key out of
> the store too, add another env var (e.g. `GREEN_PEER_OUTBOUND_KEY`) or use a
> sops template line that writes directly into config.toml. This is a known gap
> to address before deployment.

---

## Key files

| File | Role |
|---|---|
| `src/services.rs` | Full auth flow doc, `GmOrPeer` extractor, `fetch_peer_services` |
| `src/main.rs` | `PeerInfo`, `Config`, `ServerState`, env var overrides |
| `src/index.rs` | Index handler: parallel peer fetch, `peer_groups` field |
| `templates/index.html` | Peer group rendering |
| `assets/css/services.css` | `.svc-peer-*` classes |
| `flake.nix` | `peers`, `peerApiKey` NixOS options |
| `dotfiles/modules/green.nix` | Production peer list |
| `dotfiles/modules/sops.nix` | Secret declarations + EnvironmentFile template |
| `dotfiles/secrets/green.yaml` | Encrypted secrets (includes `peer_api_key` after Step 2) |
