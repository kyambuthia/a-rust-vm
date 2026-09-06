# Next Direction Decision

Decision: propagate parent cancellation into the `delegate_subagent` tool as a small P3 slice (next after transcript persistence in commit `84adf6b`).

Rationale: `SubagentSession::send` already shares the parent cancel token, but the parent-visible `SubagentTool` still runs `run_subagent` with a fresh token, so host/parent cancellation cannot stop a delegated child turn started through tool dispatch. Sharing the parent `Arc<AtomicBool>` through the `Tool` boundary reuses the existing cancellation primitive instead of inventing new lifecycle I/O, and keeps the model boundary, tool schemas, permission policy, protocol, and transport untouched.

Plan (one small slice):

1. Add a default `set_cancel_token` hook on `Tool` plus `ToolRegistry::set_cancel_token` fan-out in `src/agent.rs`; store the token on `SubagentTool` and forward it through a new `run_subagent_with_cancel` (existing `run_subagent` keeps its shape over a fresh token).
2. Have `Agent::execute_with_approval` share its cancel token into the registry before each tool execution, and surface a `cancelled` marker when child events contain `Cancelled`.
3. Add unit tests for pre-cancelled child turns, registry-to-tool token sharing, and parent-to-tool propagation via a probe tool.
4. Verify with `cargo fmt -- --check`, `cargo test`, `cargo build --target wasm32-unknown-unknown --lib`, and `node --input-type=module --check < web/main.js`; commit implementation as one conventional commit.

Explicitly out of this slice: MCP client/tools, skill auto-loading, ACP, session replay or deterministic VM replay, small language lexer/parser work, permission-rule changes, or `SubagentSession` persistence-format changes.
