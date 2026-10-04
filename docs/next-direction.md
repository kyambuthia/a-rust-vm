Decision: start roadmap milestone M0 ("Guard rails") with a GitHub Actions
workflow that runs the `AGENTS.md` verification set on every push and pull
request.

Rationale: an October 2026 review found and fixed twelve defects, several of
them security-relevant (workspace escape, permission-rule bypasses, stale
browser approvals). None of the fixes are protected unless the checks run
automatically. CI is the cheapest way to keep them fixed and is a prerequisite
for the larger refactors that follow in M0.

Plan (one small slice):

1. Add `.github/workflows/ci.yml` running on push and pull request to `main`:
   `cargo fmt -- --check`, `cargo clippy --all-targets -- -D warnings`,
   `cargo test`, `cargo build --target wasm32-unknown-unknown --lib`, and
   `node --input-type=module --check < web/main.js`.
2. Cache the cargo registry and target directory; pin the toolchain to
   stable with the wasm32 target installed.
3. Verify locally that each command in the workflow passes, then commit as
   `build: run repository checks in CI`.

Next slices in M0: commit failed-turn transcripts so context survives an
errored turn, then split `agent.rs` and `main.rs` into modules without
behaviour change.

Explicitly out of this slice: release builds, deployment, CDK synth in CI,
coverage reporting, and any change to GitHub repository settings.
