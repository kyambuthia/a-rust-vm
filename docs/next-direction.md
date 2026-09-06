# Next Direction Decision

Decision: expose the one-off subagent primitive to the agent as a bounded `delegate_subagent` tool as a small P3 extensibility slice (next after the library-only helper in commit `37cf92e`).

Rationale: `run_subagent` exists but no parent `Agent` can call it, so delegation (`ROADMAP.md` P3: parent-child lifecycle after one-off subagents) is unreachable through the tool boundary. A `SubagentTool` wrapper with caller-supplied child model/tools factories and a fixed `SubagentConfig` delivers one bounded delegation level without changing the model boundary, permission policy, workspace roots, session schema, CLI, or protocol. The child still starts isolated with default-deny approvals, fails closed on empty tasks and tool limits, and returns a truncated transcript.

Plan (one small slice):

1. Add `agent::SubagentTool` (factories for child `Box<dyn Model>` plus child `ToolRegistry`, fixed `SubagentConfig`) implementing `Tool` as `delegate_subagent` with a single required `task` argument, returning the bounded child transcript.
2. Add unit tests for delegated execution through a parent `Agent`, empty-task failure, and child-limit propagation.
3. Verify with `cargo fmt -- --check`, `cargo test`, `cargo build --target wasm32-unknown-unknown --lib`, and `node --input-type=module --check < web/main.js`; commit as one conventional commit.

Explicitly out of this slice: persistent subagents with independent sessions, parent-child cancellation/message routing, nested delegation depth tracking, MCP client/tools, workspace-scoped additional directories, skill auto-loading, ACP, permission-rule changes, session replay, deterministic VM replay, or small language/compiler work.
