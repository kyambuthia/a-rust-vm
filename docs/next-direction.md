Decision: wire MCP-discovered tools into the agent tool registry behind the existing permission/approval boundary as a small P3 slice (next after the MCP stdio client foundation in commit `80475b9`).

Rationale: the stdio client can list and call tools, but nothing exposes them to the model yet. A std-only adapter that registers one namespaced `mcp_*` tool per discovered entry, converts `ToolValue` arguments to JSON, and requires approval through `Tool::permission` closes the trust-boundary gap without new transport, policy, or protocol work.

Plan (one small slice):

1. Add `McpToolAdapter` in `src/mcp.rs` implementing `agent::Tool`: namespaced spec name (`mcp_<sanitized>`), `permission()` returning an approval request for the remote tool, `execute()` converting `ToolArguments` to JSON and delegating to `McpClient::call_tool`, mapping errors fail-closed.
2. Add `mcp_tool_registry(config)` builder that lists tools via `McpClient` and registers one adapter per entry; reject empty/unsanitizable names and duplicate registry names.
3. Add unit tests for namespacing/sanitization, permission-request shape, argument conversion, call success/error propagation, and registry build from a fake shell server.
4. Verify with `cargo fmt -- --check`, `cargo test`, `cargo build --target wasm32-unknown-unknown --lib`, and `node --input-type=module --check < web/main.js`; commit implementation as one conventional commit.

Explicitly out of this slice: MCP resources, prompts, completion, progress, notifications, cancellation wiring, subscriptions, CLI flags/server configuration plumbing, ACP, session replay or deterministic VM replay, or small language lexer/parser work.
