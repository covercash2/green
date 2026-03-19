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

# run JS tests with coverage report
js-coverage:
  npm run coverage

# lint JS with biome
lint-js:
  biome lint assets/js/ test/js/

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

### misc scripts

# create a zellij layout for mobile vibe coding
phone:
  zellij --session phone --layout scripts/phone.kdl
