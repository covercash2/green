set unstable
set script-interpreter := ["nu"]

# list recipes by default
default:
  just --list

### run

run:
  cargo run

dev:
  ./scripts/dev.nu

### check

test:
  cargo test

# build the nix flake
check-flake:
  nix flake check

# run all the checks
check: test check-flake

### misc scripts

# create a zellij layout for mobile vibe coding
phone:
  zellij --session phone --layout scripts/phone.kdl
