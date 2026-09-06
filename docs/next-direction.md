# Next Direction Decision

Decision: add parent-child cancellation routing for persistent subagent sessions as a small P3 slice (next after in-memory `SubagentSession` in commit `ca2e4bf`).

Rationale: `SubagentSession::send` rehydrates a fresh `Agent` without propagating any cancel token, so cancelling the parent (or the child handle) cannot stop child work between steps and orphaned child turns can keep calling the model. Wiring one shared `Arc<AtomicBool>` from parent/host into the child `Agent` via the existing `with_cancel_token` hook gives fail-closed cancellation without touching the model boundary, tool schemas, permission policy, session files, protocol, or transport.

Plan (one small slice):

1. Add `Agent::cancel_token` (clone) plus `SubagentSession::{with_cancel_token, cancel_token, cancel}` in `src/agent.rs`, wiring the token into `send` via `with_cancel_token`; keep `PartialEq/Eq` on id/config/history only.
2. Add unit tests for pre-cancelled `send` (no model call, `Cancelled`+`Done`), shared parent token propagation, and `cancel()` affecting the next `send`.
3. Verify with `cargo fmt -- --check`, `cargo test`, `cargo build --target wasm32-unknown-unknown --lib`, and `node --input-type=module --check < web/main.js`; commit implementation as one conventional commit.

Explicitly out of this slice: automatic parent `Agent` cancel propagation into `SubagentTool` (needs a `Tool` trait context change), parent-child message routing beyond cancellation, lifecycle management beyond the in-memory session object, session-file persistence of child transcripts, MCP client/tools, skill auto-loading, ACP, permission-rule changes, session replay, deterministic VM replay, or small language/compiler work.
