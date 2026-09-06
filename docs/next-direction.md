# Next Direction Decision

Decision: wire repeatable `--workspace-dir <path>` CLI flag into the agent/ask command as a small P1 slice (next after workspace additional directories in commit `58e47c0`).

Rationale: `workspace::Workspace::with_additional_directories` and `coding_tool_registry_with_directories` already accept bounded extra roots, but `arvm agent|ask` still builds its registry from the working directory only, so the CLI cannot grant a bounded second directory. Parsing a repeatable `--workspace-dir` flag and passing it to `coding_tool_registry_with_directories` — failing closed on unresolvable, non-directory, or overlapping roots — exposes the existing containment boundary without changing tool schemas, the model boundary, permission policy, session schema, or protocol.

Plan (one small slice):

1. Add `extract_workspace_directories` in `src/main.rs`, exclude the flag and its value from one-shot prompts, and build the agent registry with `coding_tool_registry_with_directories`.
2. Fail closed with exit 2 on invalid extra directories; keep default behavior unchanged when the flag is absent.
3. Document the flag in CLI help output.
4. Add unit tests for flag extraction, prompt exclusion, and one-shot parsing with the flag present.
5. Verify with `cargo fmt -- --check`, `cargo test`, `cargo build --target wasm32-unknown-unknown --lib`, and `node --input-type=module --check < web/main.js`; commit implementation as one conventional commit.

Explicitly out of this slice: persistent subagents with independent sessions, parent-child cancellation/message routing, MCP client/tools, skill auto-loading, ACP, permission-rule changes, session replay, deterministic VM replay, or small language/compiler work.
