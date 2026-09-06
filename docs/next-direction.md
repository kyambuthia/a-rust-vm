# Next Direction Decision

Decision: add file persistence for `SubagentSession` child transcripts as a small P3 slice (next after parent-child cancellation routing in commit `f1408ac`).

Rationale: `SubagentSession` history lives only in memory, so a host restart loses delegated child context and there is no durable record for inspection or future replay. Persisting child transcripts through the existing `SessionStore` atomic-save primitive with validated child ids reuses the session trust boundary instead of inventing new file I/O, and keeps the model boundary, tool schemas, permission policy, protocol, and transport untouched.

Plan (one small slice):

1. Add `save`/`load` on `SubagentSession` in `src/agent.rs` mapping id/config/history to `session::Session` messages over a caller-provided `SessionStore` (fail closed on id validation and decode errors).
2. Add unit tests for save/load round-trip, history continued after reload, and invalid-id rejection.
3. Verify with `cargo fmt -- --check`, `cargo test`, `cargo build --target wasm32-unknown-unknown --lib`, and `node --input-type=module --check < web/main.js`; commit implementation as one conventional commit.

Explicitly out of this slice: message routing beyond save/load, lifecycle management or cancellation changes, permission-rule changes, MCP client/tools, skill auto-loading, ACP, session replay or deterministic VM replay, or small language/compiler work.
