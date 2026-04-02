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

# run Rust tests with nextest
test:
  cargo nextest run

# compile TS → assets/js/ (commit the output)
build-js:
  deno bundle --platform=browser --minify --outdir assets/js src/js/auth-login.ts src/js/auth-register.ts src/js/mqtt.ts src/js/mqtt-devices.ts src/js/logs.ts src/js/services.ts

# type-check TS source files
check-js:
  deno check src/js/

# run JS tests (no type-check; test mocks are intentionally loose)
js-test:
  deno test --no-check test/js/*.test.ts

# run JS tests with coverage report
js-coverage:
  deno test --no-check --coverage=.deno-coverage test/js/*.test.ts

# lint JS/TS with biome
lint-js:
  biome lint src/js/ test/js/

# run all tests (Rust + JS)
test-all: test js-coverage

# Minimum acceptable line coverage percentage.
# Changing this here affects both `just coverage` and `nix flake check`.
coverage_threshold := "70"

# run tests and measure coverage; fails if line coverage drops below threshold
coverage threshold=coverage_threshold:
  cargo llvm-cov nextest --fail-under-lines {{threshold}}

# for nix sandbox builds: run coverage and write LCOV report to OUT
[private]
coverage-nix out threshold=coverage_threshold:
  cargo llvm-cov nextest --fail-under-lines {{threshold}} --lcov --output-path {{out}}

# print a coverage summary without enforcing the threshold
coverage-report:
  cargo llvm-cov nextest --summary-only

# build the nix flake
check-flake:
  nix flake check

# run all the checks (tests + flake)
check: test check-flake

### hooks

# install git hooks from scripts/hooks/ into .git/hooks/
install-hooks:
  cp scripts/hooks/pre-push .git/hooks/pre-push
  chmod +x .git/hooks/pre-push
  echo "installed pre-push hook"

# all checks run by the pre-push hook (called via nix develop)
[private]
_pre-push-checks:
  cargo fmt --check
  cargo clippy -- -D warnings
  just coverage
  deno check src/js/
  just js-test

### misc scripts

# create a zellij layout for mobile vibe coding
phone:
  zellij --session phone --layout scripts/phone.kdl
