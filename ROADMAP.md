# A/RVM Roadmap

## Product target

A/RVM is a terminal-first, LLM-powered coding agent whose distinctive runtime
is an inspectable, deterministic Rust virtual machine. The same agent runtime
serves three clients: the native CLI (`arvm`), the browser workspace
(`arvm serve` + Wasm), and editors (ACP). The VM, debugger, small language,
and LLVM work remain first-class features, not demos.

`ARCHITECTURE.md` describes the longer-term browser-workspace model
(artifacts, control plane, runners). This roadmap is the delivery plan that
gets there: the coding agent is the first workload on that control plane, so
milestones below favour work that serves both.

## Where the project stands (October 2026)

Status was verified against the code, not earlier checklists.

| Area | State |
| --- | --- |
| VM core | Integer stack VM: arithmetic, comparisons, `DUP/POP/SWAP/OVER`, `NEG/NOT`, `JMP/JZ`; static CFG + stack-depth validation; step and stack-depth limits; transactional steps; assembler, disassembler, tracer; Wasm debugger ABI |
| Guest runtime | Per-instance virtual filesystem with quotas, round-robin guest processes, syscalls, versioned snapshots, owner-scoped VM manager |
| Agent loop | Provider-neutral model trait, deterministic router with fallback, one shared turn loop (streaming and batch), step/tool-call caps, turn deadline, cancellation, repeat and pattern-loop guards |
| Model adapters | OpenRouter (server-side key, bounded retries, redaction), external process bridge |
| CLI | `run/check/disassemble/trace`, `agent` REPL, one-shot `ask` with `--json`, `--auto/--deny`, `--rule`, `--skill`, `--workspace-dir`, `--session`; `session`, `skill`, `permission`, `workspace`, `doctor`, `version` |
| Workspace tools | list/read/search/write/run with containment, dangling-symlink refusal, dangerous-command refusal, bounded output, 30 s command timeout |
| Permissions | allow/ask/deny rules (typed path and command matching), durable rule store, session approvals |
| Sessions | Save/list/show/export transcripts, resume via `--session`, private local state |
| Browser host | Anonymous and ALB/Cognito-bound sessions, uploads, document/sheet/PDF/tabulation jobs, streamed agent events, approvals, durable snapshots, ECS/Fargate CDK stack |
| Skills | `SKILL.md` discovery and prompt injection |
| MCP | stdio client (handshake, paging, interleaved messages) and approval-gated tool adapter **library only, not reachable from `arvm`** |
| Subagents | One-off and persistent subagents with depth guard and shared cancellation **library only, not reachable from `arvm`** |
| ACP, replay, language, LLVM | Not started |
| CI | None; checks run by hand per `AGENTS.md` |

### Quality review, October 2026

A review pass fixed these defects (each a focused commit with a regression
test):

- VM: a faulting instruction no longer advances the VM, so stepping again
  cannot report a bogus `HALT` result (`00205e8`).
- Assembler: validation errors name the real source line despite comments
  (`71e1ce6`).
- Workspace: writes through dangling symlinks could escape the workspace
  (`c17c139`).
- Permissions: allow rules approved compound commands and partial names;
  deny rules missed `./`, absolute, and chained spellings; session approvals
  outranked deny rules (`54ba9d3`).
- Browser host: cookieless request floods evicted active sessions
  (`2c047cb`); late approval clicks were replayed against later tool calls
  and parked decisions were unbounded (`5449f9c`); dot-only identities could
  name the snapshot directory's parent (`2ddb444`).
- MCP: no `notifications/initialized`, interleaved notifications broke calls,
  and valid responses failed if the server did not exit (`82adfd6`).
- OpenRouter: parallel tool calls made turns fail (`e4b5ddf`).
- Browser `/run` stopped loops after N steps without a result (`1513d82`).
- Removed panicking Wasm exports and deduplicated the turn loop
  (`53d6fad`, `b352ee2`).

### Known debt and risks

- **No CI.** Every guarantee above depends on someone running the checks.
- **Large modules.** `agent.rs` (~3.9k lines) and `main.rs` (~2k lines) mix
  model routing, tools, subagents, CLI parsing, and REPL code.
- **Failed turns lose context.** When a turn ends in an error (step limit,
  timeout, loop guard), its user prompt and tool results are not committed to
  the conversation, so the next turn starts without them.
- **MCP spawns a server per call.** Stateful servers lose state between calls,
  and every call pays a full handshake.
- **Hand-written HTTP/1.0 server.** No keep-alive, `Transfer-Encoding` is
  ignored, one thread per connection. Fine behind the ALB; not a public edge.
- **Single-process sessions.** Horizontal scaling waits for a shared session
  store (`desiredCount=1` in `infra/`).
- **Browser approvals use `window.confirm`.** No exact-scope display,
  no keyboard-first flow, no audit trail.
- **Command deny rules are best-effort.** `sh -c`, aliases, and interpreters
  can disguise a command; the real boundary has to be a sandboxed runner.

## Milestones

Each milestone is a sequence of small slices; each slice is one focused,
verified commit as described in `AGENTS.md`. Exit criteria are what a user can
do, checked by tests or a scripted demo.

### M0: Guard rails (next)

1. GitHub Actions workflow running `cargo fmt --check`, `cargo clippy -D
   warnings`, `cargo test`, the wasm32 build, and `node --check web/main.js`.
2. Commit the failed-turn transcript (prompt, tool calls, and a terminal
   error marker) so the next turn keeps its context.
3. Split `agent.rs` into `agent/{model,router,tools,loop,subagent}.rs` and
   `main.rs` into `cli/` command modules, with no behaviour change.

Exit: every push is checked automatically; the largest module is under
1,500 lines.

### M1: Wire the extensibility that already exists

1. `arvm` configuration file (`.arvm/config.json` in the workspace, plus
   user config) declaring MCP servers and default rules, with environment and
   flag overrides.
2. Register MCP tools in `agent`/`ask` runs behind the existing approval
   flow; `arvm mcp list|tools` for inspection.
3. Keep one MCP server process alive per session instead of per call, with
   supervision and restart limits.
4. Expose `run_subagent` as a tool in CLI runs with bounded child budgets
   and a reduced tool set.

Exit: a user configures an MCP server and a subagent budget in one file and
uses both from `arvm ask`.

### M2: Terminal parity

1. Line editing, prompt history persisted per workspace, multiline input,
   Esc/Ctrl-C to interrupt the running turn.
2. Slash-command menu with categories; `/status`, `/usage`, `/clear`.
3. Background commands with logs and lifecycle (`start`, `status`, `stop`)
   instead of only the 30 s foreground tool.
4. Retained tool results: truncate in context, inspect in full on request.

Exit: a full coding session (edit, test, iterate, interrupt, resume) runs in
`arvm agent` without restarting.

### M3: Deterministic replay and recovery

1. Recording contract: every model response and tool result in a turn is
   written to the session with a schema version.
2. `arvm session replay <id>` re-runs a turn against recorded model output,
   re-executing VM tools and comparing results; host tools use recorded
   results.
3. Corrupt-session recovery and `--no-save`.

Exit: a recorded session replays to identical VM results on another machine.

### M4: The A/RVM language

1. Locals (`LOAD n`, `STORE n`) with validated slot counts.
2. Call frames: `CALL`, `RET`, bounded frame depth, static validation.
3. Small source language: lexer, parser, AST, compiler to validated bytecode,
   with source-mapped errors.
4. Agent tools: `compile_source`, source-level `trace`, and program history
   in the terminal and browser.

Exit: the agent writes a recursive function in the source language,
compiles, traces, and explains it from real VM results.

### M5: Protocol and embedding

1. ACP JSON-RPC 2.0 server over stdio: session create/load/prompt/cancel,
   streamed assistant, tool, and permission events.
2. Versioned HTTP API shared by browser and ACP event shapes.
3. Browser approval panel with exact scope, keyboard flow, and audit log.
4. JavaScript SDK for the headless agent and the terminal widget.

Exit: an ACP-capable editor drives `arvm` end to end, and the browser uses
the same event schema.

### M6: Execution and operations

1. Runner contract from `ARCHITECTURE.md`; move format readers and the VM
   behind it.
2. Sandboxed native runner (namespaces/seccomp or a WASI runtime) so command
   policy no longer relies on deny lists.
3. Shared session store and HTTP edge hardening, then lift the single-task
   limit.
4. LLVM backend for the bytecode (native and Wasm), benchmarked against the
   interpreter.
5. Release artifacts, version channels, and performance budgets.

Exit: a signed release runs untrusted commands in a sandbox and scales past
one task.

## Definition of done

A/RVM reaches practical parity when a user can:

1. Start an interactive terminal or send a one-shot prompt.
2. Ask the LLM to inspect and modify a workspace.
3. See streamed output, tool calls, tool results, and approvals.
4. Resume or replay a prior session.
5. Run foreground and background commands safely.
6. Configure models, permissions, workspaces, skills, and MCP servers.
7. Delegate isolated work to subagents.
8. Connect an editor through ACP.
9. Embed the agent or terminal in a JavaScript/Wasm host.
10. Ask the agent to compile, execute, inspect, and explain A/RVM programs.
