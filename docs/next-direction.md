# Next Direction Decision

Decision: add project skill discovery plus explicit `arvm skill list|show|load` as a small P3 extensibility slice (next after session export).

Rationale: roadmap P3 lists skills discovered from project and user roots with explicit loading and scoped instructions ahead of MCP and subagents; there is no `skills/` directory or discovery primitive yet, while project instructions (`AGENTS.md`) already compose into the system prompt. A pure discovery + explicit-load slice unblocks scoped skill instructions without changing the permission policy, workspace roots, session schema, agent loop, or model boundary.

Plan (one small slice):

1. Add pure `src/skills.rs` with `SKILL.md` frontmatter (`name`, `description`) parsing, name validation, and deterministic discovery across `<workspace>/skills/` and user config `skills/` roots.
2. Add `arvm skill list|show <name>|load <name>` using those helpers, failing closed on unknown names, invalid frontmatter, and invalid flags.
3. Add unit tests for discovery ordering, explicit load, unknown-skill failure, and malformed `SKILL.md` rejection.
4. Verify with `cargo fmt -- --check`, `cargo test`, `cargo build --target wasm32-unknown-unknown --lib`, and `node --input-type=module --check < web/main.js`; commit as one conventional commit.

Explicitly out of this slice: workspace-scoped additional directories, MCP client/tools, one-off or persistent subagents, ACP, skill auto-loading into the system prompt, permission-rule changes, session replay, deterministic VM replay, or small language/compiler work.
