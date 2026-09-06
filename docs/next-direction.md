# Next Direction Decision

Decision: inject explicitly loaded skills into the agent system prompt via `ask/agent --skill <name>` as a small P3 extensibility slice (next after skill discovery).

Rationale: commit `9062b49` added deterministic discovery plus explicit `arvm skill list|show|load`, but loaded skill text never reaches the model boundary; `AGENTS.md` instructions already compose into the system prompt via `system_prompt_with_instructions`. Composing caller-selected skills into that same prompt closes the loop without changing the permission policy, workspace roots, session schema, agent loop, or model boundary. Explicit `--skill` flags keep loading scoped and fail-closed instead of auto-injecting every discovered skill.

Plan (one small slice):

1. Add pure `agent::system_prompt_with_skills(base, skills)` helper that appends loaded scoped-instruction blocks in caller order, returning the base prompt unchanged when empty.
2. Wire `ask/agent --skill <name>` (repeatable) in `src/main.rs`: extract names, exclude them from the one-shot prompt, fail closed on unknown/invalid skills, and compose them with project instructions for the live agent system prompt.
3. Add unit tests for prompt composition, `--skill` extraction/exclusion, and unknown-skill failure.
4. Verify with `cargo fmt -- --check`, `cargo test`, `cargo build --target wasm32-unknown-unknown --lib`, and `node --input-type=module --check < web/main.js`; commit as one conventional commit.

Explicitly out of this slice: workspace-scoped additional directories, skill auto-loading, MCP client/tools, one-off or persistent subagents, ACP, permission-rule changes, session replay, deterministic VM replay, or small language/compiler work.
