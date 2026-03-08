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

# create a zellij layout for mobile vibe coding
phone:
  zellij --session phone --layout scripts/phone.kdl
