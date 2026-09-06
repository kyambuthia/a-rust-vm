# Next Direction Decision

Decision: finish permission rules with repeatable `--rule` CLI flags (candidate 1 follow-up, narrowly scoped).

Rationale: in-memory allow/ask/deny rules plus session approvals landed, but rules are unreachable from the CLI, so guarded execution is still coarse in practice; exposing `--rule "<allow|ask|deny> <tool|*>[:<target>]"` on `agent|ask` (one-shot and REPL) is the smallest deterministic slice that makes `--auto` safe to scope, stays ahead of skills/MCP/subagents trust needs, and defers VM replay while tool policy is still incomplete.

Plan (one small slice):

1. Add `--rule` extraction plus policy build in `src/main.rs` reusing `parse_permission_rule`, failing closed on invalid rules.
2. Wire the policy into the live agent for one-shot and interactive turns; skip `--rule` values in one-shot prompt parsing.
3. Add unit tests for rule extraction and invalid-rule rejection.
4. Verify with `cargo fmt -- --check`, `cargo test`, `cargo build --target wasm32-unknown-unknown --lib`, and `node --input-type=module --check < web/main.js`; commit as one conventional commit.

Explicitly out of this slice: durable rule storage, workspace-scoped additional directories, command policy/dangerous-command handling, and skills/MCP/subagents or VM replay work.
