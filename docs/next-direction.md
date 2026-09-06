# Next Direction Decision

Decision: add `arvm session export <id> [--json]` as a small sessions/replay slice (next after dangerous-command policy).

Rationale: sessions already persist and resume transcripts, and roadmap P2 explicitly lists transcript export ahead of tool-call and deterministic VM replay; `session show` is human-only text, so a small machine-readable JSON export plus stable text export is the smallest end-to-end slice that unblocks deterministic fixtures, replay work, and scripting without changing the session schema, permission policy, workspace roots, or agent loop.

Plan (one small slice):

1. Add pure `format_session_text` and `format_session_json` helpers for `Session` transcripts in `src/main.rs`.
2. Add `arvm session export <id> [--json]` using those helpers, failing closed on unknown ids and invalid flags.
3. Add unit tests for text export, JSON round-trip, empty-session export, and unknown-session failure path.
4. Verify with `cargo fmt -- --check`, `cargo test`, `cargo build --target wasm32-unknown-unknown --lib`, and `node --input-type=module --check < web/main.js`; commit as one conventional commit.

Explicitly out of this slice: workspace-scoped additional directories, command allowlists, rule editing beyond add/clear, redacted diagnostics, skills/MCP/subagents, structured tool-call replay, deterministic VM replay, ACP, session listing changes, or small language/compiler work.
