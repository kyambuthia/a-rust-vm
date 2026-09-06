# Next Direction Decision

Decision: add a delegation depth guard to the `delegate_subagent` tool as a small P3 safety slice (next after the delegation tool in commit `61fd723`).

Rationale: `SubagentTool` builds fresh child models/tools per call, so nothing stops a child registry from containing another `delegate_subagent` and recursing without bound on one thread. A thread-local depth counter checked against `SubagentConfig::max_depth` (default 1) fails closed on nested delegation without changing the model boundary, permission policy, workspace roots, session schema, CLI, or protocol.

Plan (one small slice):

1. Add `max_depth` to `agent::SubagentConfig` plus a depth-exceeded error surfaced as a tool error.
2. Guard `SubagentTool::execute` with a thread-local counter that resets after each child turn.
3. Add unit tests for blocked nesting, one allowed level with `max_depth: 2`, and counter reset across sequential calls.
4. Verify with `cargo fmt -- --check`, `cargo test`, `cargo build --target wasm32-unknown-unknown --lib`, and `node --input-type=module --check < web/main.js`; commit as one conventional commit.

Explicitly out of this slice: persistent subagents with independent sessions, parent-child cancellation/message routing, MCP client/tools, workspace-scoped additional directories, skill auto-loading, ACP, permission-rule changes, session replay, deterministic VM replay, or small language/compiler work.
