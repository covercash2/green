# CLAUDE.md

This file is guidance for Claude Code sessions in this repo — it is not the project's
documentation. Actual documentation lives where any contributor can find it:

- [README.md](README.md) — what the project does, quick start, tech stack
- [docs/development.md](docs/development.md) — dev server lifecycle, testing, Nix shell, code quality, git hooks, remote access over Tailscale
- [docs/architecture.md](docs/architecture.md) — HTTP routes, module structure, template rendering, JS pipeline, key dependencies
- [docs/configuration.md](docs/configuration.md) — TOML config reference, secrets, dev config
- [docs/nixos-module.md](docs/nixos-module.md) — NixOS module options

When those docs and the code disagree, trust the code and fix the docs.

## Things worth flagging before you do them out of habit

- **Never run `cargo run` directly for the dev server.** Use `scripts/green.nu`
  (`green start`/`stop`/`restart`/`run`) — `cargo run` won't set credentials or
  redirect logs correctly, and `green stop` matches processes by config path, so it
  can't find or kill a server started outside `green start`. Details:
  [docs/development.md](docs/development.md#dev-server-lifecycle).
- **No inline `<script>` blocks in templates.** JS goes through
  `src/js/*.ts` → `just build-js` → `assets/js/*.js`. Details:
  [docs/architecture.md § JavaScript](docs/architecture.md#javascript).
- **Every Askama template struct needs `auth_user: Option<AuthUserInfo>`** —
  `base.html` requires it.
