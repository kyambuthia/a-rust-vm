# Next Direction Decision

Decision: add a one-off subagent primitive (`agent::run_subagent`) with its own bounded session and tool limits as a small P3 extensibility slice (next after skill injection).

Rationale: commit `335cbcf` closed the skill-injection loop, so the next roadmap gap is delegation (`ROADMAP.md` P3: one-off subagents, then persistent subagents and lifecycle). A pure library helper over the existing bounded `Agent` turn loop delivers isolated child execution without changing the model boundary, tool registry, permission policy, workspace roots, session schema, CLI, or protocol. It keeps the parent conversation isolated, fails closed on empty prompts and tool limits, and truncates the child transcript so callers get a bounded result.

Plan (one small slice):

1. Add `agent::SubagentConfig` (max steps, max tool calls, context limits, optional system prompt) plus `SubagentResult` (events, assistant text, tool-call count) and a `run_subagent(model, tools, prompt, config)` helper that runs a fresh `Agent` with default-deny approvals.
2. Add unit tests for delegated tool execution, empty-prompt failure, tool-call limit enforcement, and parent-conversation isolation with system-prompt override.
3. Verify with `cargo fmt -- --check`, `cargo test`, `cargo build --target wasm32-unknown-unknown --lib`, and `node --input-type=module --check < web/main.js`; commit as one conventional commit.

Explicitly out of this slice: persistent subagents with independent sessions, parent-child lifecycle/cancellation/message routing, MCP client/tools, workspace-scoped additional directories, skill auto-loading, ACP, permission-rule changes, session replay, deterministic VM replay, or small language/compiler work.
