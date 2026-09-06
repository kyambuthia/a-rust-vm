# Next Direction Decision

Decision: add fail-closed dangerous-command policy for `run_command` as a small tool-boundary slice (next after durable permission rules).

Rationale: `run_command` executes arbitrary shell via `sh -c` in the workspace directory, and stored/CLI `allow run_command` rules can currently auto-approve any command string; prefix matching alone is fragile against destructive commands, so a small deterministic deny-list enforced inside `RunCommandTool::execute` is the smallest safety slice that keeps scoped `--auto` usable, stays ahead of skills/MCP/subagent trust needs, and matches the existing validate-at-the-boundary discipline.

Plan (one small slice):

1. Add a `reject_dangerous_command` helper in `src/workspace.rs` with a small explicit case-insensitive pattern list (recursive root/home removal, `mkfs`, `dd` to `/dev/`, `shutdown`/`reboot`/`halt`/`poweroff`, fork-bomb body).
2. Enforce it at the start of `RunCommandTool::execute` so it fails closed even when a permission rule allows the command or one-shot `--auto` is used.
3. Add unit tests for blocked commands, safe commands passing through, and allow-rule plus dangerous-command still failing closed.
4. Verify with `cargo fmt -- --check`, `cargo test`, `cargo build --target wasm32-unknown-unknown --lib`, and `node --input-type=module --check < web/main.js`; commit as one conventional commit.

Explicitly out of this slice: workspace-scoped additional directories, configurable command allowlists, per-workspace rule scoping, rule editing beyond add/clear, redacted diagnostics, and skills/MCP/subagents, VM replay, sessions replay, ACP, or small language/compiler work.
