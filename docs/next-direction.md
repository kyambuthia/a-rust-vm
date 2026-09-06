# Next Direction Decision

Decision: add durable permission rule storage as a small file-backed slice (next after `--rule` flags).

Rationale: in-memory allow/ask/deny rules plus repeatable `--rule` CLI flags landed, but rules are ephemeral per invocation, so every `--auto` run must restate policy and long-lived scoping is impractical; persisting validated rules to the private state directory (`A_RVM_STATE_DIR`/`rules.json` with XDG/HOME fallback) with CLI-first merge precedence is the smallest deterministic slice that makes scoped `--auto` repeatable, stays ahead of skills/MCP/subagents trust needs, and matches the existing atomic-save session pattern.

Plan (one small slice):

1. Derive serde for `PermissionRule`/`PermissionPolicy` plus a `rules()` accessor in `src/permissions.rs`.
2. Add `permission_rules_path`, atomic save, fail-closed load, and CLI-first merge helpers in `src/main.rs`.
3. Add `arvm permission list|add <rule>|clear` reusing `parse_permission_rule`; wire the merged policy into the live agent for one-shot and REPL turns.
4. Add unit tests for file round-trip, missing/corrupt files, stored-rule validation, and CLI-over-file precedence.
5. Verify with `cargo fmt -- --check`, `cargo test`, `cargo build --target wasm32-unknown-unknown --lib`, and `node --input-type=module --check < web/main.js`; commit as one conventional commit.

Explicitly out of this slice: workspace-scoped additional directories, command policy/dangerous-command handling, per-workspace rule scoping, rule editing beyond add/clear, and skills/MCP/subagents or VM replay work.
