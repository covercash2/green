# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Project Overview

Green is a Rust-based HTTP server that provides an internal routing/landing page for home services. It serves as a central hub displaying links to various self-hosted services (Foundry VTT, AdGuard, Grafana, PostgreSQL, Home Assistant, Frigate, etc.) and provides a CA certificate endpoint for internal TLS.

## Build & Development Commands

### Build and Run
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
# Run all tests
cargo test
just test
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

2. **Dynamic Routes** (configured via TOML):
   - Defined in config.toml under `[routes.*]` sections
   - Each route has `url` and `description` fields
   - Displayed on the index page as links

### Module Structure

- `main.rs` - Application entry point, server setup, CLI, and configuration
  - `ServerState` - Shared state containing CA certificate and index page
  - `Config` - TOML configuration structure
  - `Route` enum - Static route definitions

- `route.rs` - Dynamic route types (`Routes`, `RouteInfo`)
- `index.rs` - Index page template and handler
- `error.rs` - Application error types with `IntoResponse` implementation
- `io.rs` - File I/O utilities (async file reading, TOML loading)

### Configuration

Configuration is loaded from a TOML file (default: `config.toml`) with this structure:
```toml
port = 10000
ca_path = "path/to/ca.pem"
log_level = "info"  # or "debug", "warn", etc.

[routes.service_name]
url = "service.example.com"
description = "Service description"
```

### Template Rendering

Uses Askama template engine with templates in `templates/` directory. The index page template is rendered with route data sorted alphabetically by name.

## NixOS Module

The flake provides a NixOS module at `nixosModules.default` for deploying as a systemd service:
- Service runs as `green` user by default
- Configuration generated at `/etc/green/config.toml`
- Extensive systemd hardening measures applied
- State directory at `/var/lib/green` by default

## Key Dependencies

- **axum** - Web framework
- **tokio** - Async runtime
- **tower/tower-http** - Middleware and services
- **askama** - Template engine
- **serde/toml** - Configuration parsing
- **tracing** - Structured logging (JSON format)
- **clap** - CLI argument parsing

## Development Notes

- Uses Rust 2024 edition
- All routes are async handlers
- Tracing is configured for JSON output in production
- CA certificate is loaded once at startup and shared via Arc
- Index page is pre-rendered at startup for efficiency
- Static assets expected in `assets/` directory
