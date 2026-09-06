# Next Direction Decision

Decision: add the MCP client foundation with lazy tool discovery over stdio as a small P3 slice (next after parent-cancel propagation in commit `1f7050a`).

Rationale: the roadmap lists an MCP client with lazy tool discovery; the skills and subagent tracks are complete (persistent, cancellable, depth-limited). A std-only stdio JSON-RPC client supporting `initialize`, `tools/list` (with cursor pagination), and `tools/call` unlocks external tools behind the existing permission/approval boundary without new transport, policy, or protocol work, and discovery stays lazy (spawned only when the host asks) instead of loading servers at startup.

Plan (one small slice):

1. Add `src/mcp.rs` (wired into `src/lib.rs`) with std-only newline-delimited JSON-RPC 2.0 types: `McpConfig` (command, args, env allowlist as name=value pairs), `McpTool` (name, description, input schema), and `McpClient` that spawns one stdio server per request, sends `initialize` then `tools/list` (cursor loop with a bounded page cap) or `tools/call`, and parses one JSON response line.
2. Validate at the boundary: non-empty command/name, bounded stdout line (64 KiB), `result` JSON must carry a `tools` array (list) or `content` array (call); surface stderr/exit details and fail closed on malformed/oversized output.
3. Add unit tests for list pagination aggregation, call success/error shapes, malformed JSON, missing command, and oversized lines, all with tiny fake shell servers.
4. Verify with `cargo fmt -- --check`, `cargo test`, `cargo build --target wasm32-unknown-unknown --lib`, and `node --input-type=module --check < web/main.js`; commit implementation as one conventional commit.

Explicitly out of this slice: MCP resources, prompts, completion, progress, notifications, cancellation, subscriptions, trust boundaries/permission wiring, ACP, session replay or deterministic VM replay, small language lexer/parser work, or permission-rule changes.
