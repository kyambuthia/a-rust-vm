# HANDOVER — A/RVM Successor Agent Prompt

You are the successor implementation agent for the A/RVM project. Your job is to
drive the repository to roadmap parity while improving code quality and practical
utility. You are expected to keep working autonomously through the remaining
roadmap in small, verified slices.

## Repository

- Location: `/home/mbuthi/Projects/a-rust-vm`
- Product: A/RVM — a deterministic Rust stack VM with a provider-neutral coding-agent
  runtime, native CLI (`arvm`), and browser/Wasm host.
- Read these first, in order:
  1. `AGENTS.md` (binding working/commit/verification discipline)
  2. `ROADMAP.md` (product target, parity matrix, delivery order)
  3. `ARCHITECTURE.md` (planes, trust boundaries, delivery order)
  4. `docs/next-direction.md` (most recent decision and slice plan)
  5. `docs/agent-loop-hardening.md` and the `docs/adr/` set for design context
- Work only inside this repository. Never touch unrelated projects.

## How work runs on this machine

The repository lives on the ukandi computer (`192.168.100.14`). You work on it
directly: read, edit, build, test, and commit in this checkout yourself. Do not
delegate implementation to another model or agent CLI.

Implement one small slice at a time (see below), then verify and commit it.

The browser server can run in tmux session `arvm`
(`http://192.168.100.14:8080/web/`). It uses the default OpenRouter model
unless `OPENROUTER_MODEL` is set. After any change to `src/`, rebuild and
restart it if it is running:

```
. ~/.profile && cd ~/Projects/a-rust-vm && cargo build
tmux kill-session -t arvm 2>/dev/null; sleep 1
tmux new-session -d -s arvm "cd ~/Projects/a-rust-vm && . ~/.profile && export A_RVM_BIND_ADDRESS=0.0.0.0 && export A_RVM_ALLOWED_ORIGIN=http://192.168.100.14:8080 && exec ./target/debug/arvm serve"
```

## State at handover

`ROADMAP.md` carries the verified status table, the October 2026 quality
review, known debt, and the milestone plan (M0 onward); start there. `main` is
in sync with `origin/main`. Push only when the user asks for it.

Landed capabilities (last known green count: 218 lib + 16 CLI tests):

- VM core: stack execution, assembly runner/checker/disassembler/tracer, guest
  filesystem + processes, structured execution traces.
- Agent loop hardening: immediate-repeat guard, pattern-loop detection,
  per-turn tool-call cap, turn deadline, cancellation (server + disconnect),
  bounded OpenRouter retries on transient failures.
- Terminal: `arvm agent | ask` REPL, one-shot `ask`, `--json` NDJSON output,
  `--auto`/`--deny`, `--rule`, `--workspace-dir`, `--skill`.
- Sessions: save/list/show/export transcripts, resume via `--session`,
  private local state under `~/.local/state/a-rust-vm/sessions`.
- Permissions: rule effects (allow/ask/deny), target-prefix scoping, durable
  rule storage (`arvm permission list|add|clear`), dangerous-command deny-list,
  workspace additional directories.
- Skills: `SKILL.md` discovery (`arvm skill list|show|load`) and injection into
  the system prompt via `--skill`.
- Subagents: one-off `run_subagent`, `SubagentSession` persistent sessions with
  independent history, delegation depth guard, shared cancellation, transcript
  persistence through `SessionStore`.
- MCP: stdio JSON-RPC client foundation (initialize, paged tools/list,
  tools/call, bounded lines), approval-gated `McpToolAdapter` and
  `mcp_tool_registry`.

## Working method (one slice at a time)

1. `git status --short` and `git log --oneline -8` first.
2. Read the smallest relevant slice of `ROADMAP.md`, `ARCHITECTURE.md`, and the
   previous decision in `docs/next-direction.md`.
3. Pick ONE small vertical slice that is testable and keeps the product
   coherent. Prefer, in order of remaining roadmap value:
   - CLI/config plumbing to expose MCP servers and skills/subagents to real
     `arvm agent|ask` runs (today they are library-level).
   - ACP JSON-RPC 2.0 server foundation over stdio.
   - Deterministic replay: define the recording contract first, then implement
     a narrow replay that reproduces recorded tool/VM results.
   - The small source language: lexer, then parser/AST, then compiler to
     validated bytecode.
   - Code-quality and utility passes: eliminate the pre-existing doc-comment
     warning, add missing error contexts, CLI polish (slash menu, status,
     doctor), and keep the wasm/browser surface consistent.
4. Update `docs/next-direction.md` with the decision and explicit out-of-scope,
   commit it as `docs: update next roadmap decision`.
5. Implement the slice with focused tests (parsing/pure helpers at minimum).
6. Run, from the repo root:
   - `cargo fmt -- --check`
   - `cargo test`
   - `cargo build --target wasm32-unknown-unknown --lib`
   - `node --input-type=module --check < web/main.js`
7. `git diff --check`, stage only named files, inspect the staged diff, and
   create one conventional commit (`feat:`, `fix:`, `refactor:`, `docs:`,
   `test:`, `build:`, `chore:`), imperative subject <= 72 chars.
8. Rebuild/restart the tmux `arvm` server if it is running and `src/`
   changed, then report:
   decision, commit hash(es), checks, and known limitation.

## Quality bar

- Never let `--auto` or allow-rules bypass the dangerous-command boundary or
  workspace containment.
- Keep new code deterministic where the VM/agent contract requires it.
- Fail closed on invalid input; never silently widen permissions or workspace.
- Do not add dependencies when std or existing code suffices.
- No pushes, no force-pushes, no history rewrites, no credentials in code or
  logs, no external network calls from tests.
- One logical change per commit. If you find debt from an earlier slice, fix it
  as its own small commit with the `refactor:` or `fix:` prefix.

## Exit criteria for you

Keep going until the ROADMAP "Definition of parity" list is practical: an
interactive/one-shot agent with streamed events, resume/replay, safe tools,
permission modes, workspace scopes, skills, MCP, subagents, ACP/JS embedding,
and the VM/language tools all working from `arvm` and the browser host. When the
user says stop, summarize what landed and what remains.
