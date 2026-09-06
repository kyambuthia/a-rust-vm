# Next Direction Decision

Decision: add workspace-scoped additional directories to `workspace::Workspace` as a small P1 safety slice (next after the delegation depth guard in commit `64b4998`).

Rationale: `Workspace` currently allows exactly one canonicalized root, so child agents and jobs cannot be granted a bounded second directory without widening the primary root. Accepting an explicit list of additional canonicalized roots — with absolute paths permitted only when contained in an allowed root and relative paths still resolved under the primary root — extends the containment boundary without changing tool schemas, the model boundary, permission policy, session schema, CLI flags, or protocol.

Plan (one small slice):

1. Add `Workspace::with_additional_directories` plus `workspace_tool_registry_with_directories` and `coding_tool_registry_with_directories`, keeping `Workspace::new` behavior unchanged.
2. Resolve relative paths under the primary root; accept absolute paths only when canonicalized inside an allowed root, failing closed otherwise.
3. Reject duplicate or nested additional roots at construction so containment stays unambiguous.
4. Add unit tests for reads/writes via an additional directory, rejection of paths outside all roots, and construction-time rejection of nested roots.
5. Verify with `cargo fmt -- --check`, `cargo test`, `cargo build --target wasm32-unknown-unknown --lib`, and `node --input-type=module --check < web/main.js`; commit as one conventional commit.

Explicitly out of this slice: persistent subagents with independent sessions, parent-child cancellation/message routing, MCP client/tools, CLI flags for extra directories, skill auto-loading, ACP, permission-rule changes, session replay, deterministic VM replay, or small language/compiler work.
