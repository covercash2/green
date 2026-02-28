set unstable
set script-interpreter := ["nu"]

# list recipes by default
default:
  just --list

run:
  cargo run

dev:
  ./scripts/dev.nu

test:
  cargo test
