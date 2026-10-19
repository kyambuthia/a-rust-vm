#!/usr/bin/env bash
# Run every check AGENTS.md requires, stopping at the first failure.
set -euo pipefail
cd "$(dirname "$0")/.."

step() { printf '\n== %s\n' "$*"; }

step "cargo fmt --check"
cargo fmt -- --check
step "cargo clippy -D warnings"
cargo clippy --all-targets -- -D warnings
step "cargo test"
cargo test --quiet
step "wasm32 library build"
cargo build --quiet --target wasm32-unknown-unknown --lib
step "web/main.js syntax"
node --input-type=module --check < web/main.js
step "git diff --check"
git diff --check
printf '\nall checks passed\n'
