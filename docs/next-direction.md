# Next Direction Decision

Decision: add persistent subagent sessions with independent conversation history as a small P3 slice (next after one-off `delegate_subagent` and depth guard in commits `37cf92e`/`64b4998`).

Rationale: `run_subagent` and `SubagentTool` always start from an empty conversation, so multi-step delegated work cannot accumulate context. A small `SubagentSession` type in `src/agent.rs` that owns an id, a `SubagentConfig`, and a `Vec<ConversationMessage>` — with a `send(model, tools, prompt)` turn that rehydrates an `Agent` via `set_conversation` and persists the bounded result — gives independent child sessions without touching the model boundary, tool schemas, permission policy, session files, protocol, or transport.

Plan (one small slice):

1. Add `SubagentSession::{new, id, history, send}` plus an `InvalidId` error variant in `src/agent.rs`, reusing `Agent` conversation and transcript bounds.
2. Add unit tests for id validation, empty-prompt rejection, history persistence across two turns, and transcript truncation.
3. Verify with `cargo fmt -- --check`, `cargo test`, `cargo build --target wasm32-unknown-unknown --lib`, and `node --input-type=module --check < web/main.js`; commit implementation as one conventional commit.

Explicitly out of this slice: parent-child cancellation/message routing, lifecycle management beyond an in-memory session object, session-file persistence of child transcripts, MCP client/tools, skill auto-loading, ACP, permission-rule changes, session replay, deterministic VM replay, or small language/compiler work.
