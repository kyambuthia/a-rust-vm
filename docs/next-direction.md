# Next Direction Decision

Decision: deepen permission modes with rules by tool and target plus session-scoped approvals (candidate 1, narrowly scoped).

Rationale: the agent loop hardening slices (repeat guard, tool/time caps, cancellation, retry) and ask/auto/deny one-shot modes are landed, so the highest-value next step is making guarded execution predictable and auditable before adding skills, MCP, or subagents; rule-based allow/ask/deny by tool and target plus session-remembered approvals is the smallest deterministic core that tightens the existing `PermissionRequest`/`PermissionDecision` boundary, keeps `--auto` safe to use, and is a prerequisite trust boundary for any extensibility work, whereas VM replay is valuable but secondary while tool execution policy is still coarse.

Plan (one small slice):

1. Add `src/permissions.rs` with no new dependencies: `PermissionEffect` (`Allow`/`Ask`/`Deny`), `PermissionRule { effect, tool, target_prefix }`, `parse_permission_rule` for `"<allow|ask|deny> <tool|*>[:<target-prefix>]"`, `PermissionPolicy::decide(tool, target)` (first match wins, default `Ask`), `rule_target(&ToolCall)` extracting the guarded target (`path`/`command`/`program` or empty), and `SessionApprovals` (remembered `tool + target` allows, with clear).
2. Wire into `src/agent.rs` `execute_with_approval`: policy `Allow` or remembered approval executes without prompting; policy `Deny` denies with an explicit message; policy `Ask` delegates to the host callback and remembers subsequent `Allow` decisions for the rest of the session.
3. Add unit tests for rule parsing (including rejection of bad effects, empty tools, empty targets) and for policy decisions (exact tool, `*` wildcard, target-prefix scoping, first-match order, default ask) plus session-approval behavior (second identical write skips the prompt, differing target re-prompts, deny never remembered).
4. Verify with `cargo fmt -- --check`, `cargo test`, `cargo build --target wasm32-unknown-unknown --lib`, and `node --input-type=module --check < web/main.js`; commit as one conventional commit.

Explicitly out of this slice: workspace-scoped additional directories, command policy/dangerous-command handling, persistent rule storage, and CLI flags for rules (follow-up slices).
