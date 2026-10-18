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
| Model adapters | OpenRouter (browser server only; no streaming, no usage accounting), external process bridge; the CLI default shells out to `opencode run` on every model step |
| CLI | `run/check/disassemble/trace`, `agent` REPL, one-shot `ask` with `--json`, `--auto/--deny`, `--rule`, `--skill`, `--workspace-dir`, `--session`; `session`, `skill`, `permission`, `workspace`, `doctor`, `version` |
| Workspace tools | list/read/search/write/run with containment, dangling-symlink refusal, dangerous-command refusal, bounded output, 30 s command timeout |
| Permissions | allow/ask/deny rules (typed path and command matching), durable rule store, session approvals |
| Sessions | Save/list/show/export transcripts, resume via `--session`; state files are owner-only (`0600`) and written atomically |
| Browser host | Anonymous and ALB/Cognito-bound sessions, uploads, document/sheet/PDF/tabulation jobs, streamed agent events, approvals, durable snapshots, ECS/Fargate CDK stack |
| Skills | `SKILL.md` discovery and prompt injection |
| MCP | stdio client (handshake, paging, interleaved messages); configured servers register approval-gated `mcp_<server>_<tool>` tools in `agent`/`ask`; workspace servers need `arvm mcp trust` |
| Subagents | One-off and persistent subagents with depth guard and shared cancellation **library only, not reachable from `arvm`** |
| Config | User and workspace `config.json` (rules, MCP servers); workspace files cannot grant `allow` |
| ACP, replay, language, LLVM | Not started |
| CI | GitHub Actions runs the `AGENTS.md` checks on every push and pull request |

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

### Measured baseline (19 October 2026)

These numbers drive the order below. They were measured on the ukandi machine
or read from the code.

| Measure | Value | Why it matters |
| --- | --- | --- |
| `arvm` cold start (release) | 4 ms, 8.5 MB binary | The Rust side is already fast |
| `opencode --version` start | 1.3 s | Paid on **every** CLI model step before any model work |
| CLI model path | `arvm model-bridge` -> `opencode run` -> provider, request pasted into a prompt as JSON | Two extra processes, no streaming, a nested agent that can reply in the wrong shape |
| OpenRouter adapter | Blocking, no SSE streaming, no token or cost accounting, no prompt caching, one tool call per step | Slow first token, no cost visibility, repeated full-price prompts |
| Steps per turn | 8 (CLI and browser) | Too few to read, edit, test, and fix in one turn |
| Context kept | 24 messages or 32 KB of text | Far below current model windows; the agent forgets files it just read |
| Edit tool | None; `write_file` rewrites whole files up to 128 KB | Slow, expensive, and error-prone for small changes |
| `run_command` environment | Inherits the full parent environment (fixed in S1.2) | A model could print `OPENROUTER_API_KEY`, AWS credentials, or tokens |
| Command timeout | Kills `sh`, not its process group (fixed in S1.3) | Background children survived timeouts and held the tool open |
| State files | `0664`, directories `0775` (fixed in S1.1) | Transcripts, rules, and trust records were readable by other local users |
| CLI error handling | 62 `process::exit` calls across `src/cli/` | Hard to test, and cleanup steps get skipped |

### Known debt and risks

- **`server.rs` is ~2k lines** (~1.6k without tests) and mixes HTTP parsing,
  routing, and session handling.
- **MCP spawns a server per call.** Stateful servers lose state between calls,
  and every call pays a full handshake.
- **Hand-written HTTP/1.0 server.** No keep-alive, `Transfer-Encoding` is
  ignored, one thread per connection. Fine behind the ALB; not a public edge.
- **Single-process sessions.** Horizontal scaling waits for a shared session
  store (`desiredCount=1` in `infra/`).
- **Browser approvals use `window.confirm`.** No exact-scope display,
  no keyboard-first flow, no audit trail.
- **Command deny rules are best-effort.** `sh -c`, aliases, and interpreters
  can disguise a command; the real boundary has to be a sandbox.
- **Prompt injection.** Tool output, project instructions, and MCP results
  reach the model with the same standing as the user's words.

## How the agent becomes quick, useful, and efficient

A coding agent turn is a loop: build a prompt, wait for the model, run tools,
repeat. Each goal maps to one part of that loop.

- **Quick means wall-clock time per finished task.** The model call dominates
  it, so the first lever is overhead around the call: no extra processes,
  one reused HTTP connection, streamed tokens, and a cached prompt prefix so
  the provider does not reprocess the same history every step. The second
  lever is fewer steps: an edit tool instead of whole-file rewrites, and
  independent reads run in parallel within one step. Rust-side overhead is
  already small (4 ms start) and should stay that way.
- **Useful means the share of real tasks finished without help.** That
  depends on the tools (precise edits, line-numbered reads, bounded search,
  long-running commands), on enough budget to read, edit, test, and fix in
  one turn, and on context that keeps what the agent just learned. Fewer,
  better approval prompts also matter: prompts that ask about harmless reads
  train people to press `y` blindly.
- **Efficient means tokens and money per finished task.** Everything that
  enters the context costs tokens on every later step. Long tool output is
  truncated in context and kept for retrieval on request; old turns are
  compacted when the window fills; the stable prefix (system prompt, tool
  list, history) is ordered for caching; cheap work such as summaries goes to
  a cheaper model.
- **Secure by construction.** Deny lists cannot win against `sh -c`. The
  boundary is what a command can reach: a scrubbed environment, private
  state, a killed process group, and then a real sandbox. Once commands run
  in a sandbox, routine commands can run without prompts. Security then also
  makes the agent faster to use.
- **Measured, not guessed.** Each slice below states the number it should
  move. A benchmark harness (Q1) runs fixed tasks against a scripted model in
  CI and against a real model on demand, and records steps, tokens, cache
  hits, wall time, and success. A change that makes the numbers worse needs a
  reason in its commit body.

### Budgets

| Budget | Target |
| --- | --- |
| Overhead per model step, excluding provider time | <= 50 ms |
| Time to first streamed token | provider latency + <= 100 ms |
| Read-only tool (read, search on a 10k-file repository) | <= 50 ms typical, hard output cap |
| Prompt cache hits on steps 2+ of a turn (caching providers) | >= 70% of input tokens |
| `arvm` cold start | <= 20 ms |
| Approval prompts for read-only work | 0 |

## Milestones

Each milestone is a sequence of small slices; each slice is one focused,
verified commit as described in `AGENTS.md`. The order puts cheap, high-value
fixes first, then the biggest speed lever, then the harness, so later tool
and context work can be measured.

### Done

- **M0 Guard rails:** CI on every push (`40455c3`), turns that end without an
  answer keep their context, `agent.rs` and `main.rs` split into modules
  (`311a27b`, `fbad5b8`).
- **M1.1-M1.2 Configuration and MCP:** user and workspace `config.json`
  (`61601b1`); configured MCP servers in `agent`/`ask`, with workspace servers
  gated by `arvm mcp trust` (`30b55d4`).

### S1: Close the leaks (done)

1. ~~Private state: create the state and config directories `0700` and every
   session, rule, trust, and snapshot file `0600`, writing atomically~~.
   New directories are `0700`; existing ones keep their mode.
2. ~~Scrubbed environment for `run_command` and MCP servers: pass an allow list
   (`PATH`, `HOME`, `USER`, `LANG`, `LC_*`, `TERM`, `TMPDIR`, `SHELL`) plus
   names listed in config `env.passthrough`; never pass `*_API_KEY`,
   `*_TOKEN`, `*_SECRET`, `AWS_*`, or `OPENROUTER_*`~~. The key is
   `envPassthrough`, user configuration only.
3. ~~Run each command in its own process group and kill the whole group on
   timeout and cancellation (`std::os::unix::process::CommandExt::process_group`,
   no new dependency)~~. The group is also killed when the command exits,
   so `cmd &` cannot hang the tool or outlive it.
4. ~~Redact secret-looking values (provider keys, AWS keys, bearer tokens) from
   tool output before it reaches the model, the screen, or a transcript,
   reusing the OpenRouter redaction code~~. `src/redact.rs` also hides
   private-key blocks, `UPPER_SECRET=value` lines, and the values of
   secret-named host variables.
5. ~~Mark untrusted content: wrap tool and MCP output in delimiters the system
   prompt tells the model to treat as data, and show which project
   instruction files were loaded in `/status`~~. Tool output already travels
   in the provider's `tool` role, so no extra delimiters; the system prompt
   now names the injection rule, and `/status` lists instructions, skills,
   config files, MCP servers, and rule count.
6. ~~`cargo audit` (or `cargo deny`) in CI~~. The first audit found ten
   advisories (rustls, quinn-proto, zip), fixed by updating them.

Exit: a scripted model that runs `env`, `cat ~/.local/state/a-rust-vm/...`
as another user, and `sleep 100 &` learns no secret, reads no file, and leaves
no process behind.

### U1: Native fast model path (in progress)

1. ~~Run the OpenRouter adapter in process for `arvm agent|ask` whenever a key
   is configured, with one reused HTTP client; keep the process bridge only as
   an explicit `A_RVM_MODEL_PROGRAM` option~~. Measured: 8.0 s native vs
   43.9 s through the bridge for the same two-step task. The adapter's 4 KB
   prompt and message caps and 4k-token output cap were raised too.
2. ~~SSE streaming for text and tool-call deltas~~. Measuring it showed the
   first token waited on silent reasoning and on slow providers, so
   `OPENROUTER_REASONING` (default `low`) and `OPENROUTER_PROVIDER_SORT`
   (default `throughput`) were added. Typical one-step answers went from
   8 s to 3-5 s.
3. ~~Usage events: input, output, and cached tokens, cost, and latency per step;
   `/usage` in the REPL and a summary line after `ask`~~. Events also carry
   the provider and first-token time.
4. ~~Cache-friendly prompts: a fixed order (system prompt, tools, history,
   new turn) and provider cache markers where supported~~. History now trims
   to half its budget when full, so the prompt start stays stable for several
   turns; Anthropic models get `cache_control` markers. Measured with Haiku
   4.5 and an 8k-token prompt: step 2 read 8,065 tokens from cache, cost
   9x less, and answered in 0.9 s instead of 4.3 s.
5. Model settings in `config.json`: default and fast model, OpenAI-compatible
   base URL (so local servers work), and request timeout.

Exit: on the same task, steps cost provider latency plus <= 50 ms, the first
token streams, and `/usage` shows tokens, cache hits, and cost.

### Q1: Benchmark harness and recording contract

1. Recording contract: every model request summary, response, tool call, and
   tool result in a turn is written to the session with a schema version.
   This is also the base for replay (M3).
2. `tests/` integration suite that drives the real `arvm` binary with a
   scripted model and scripted MCP server, like the manual checks used for
   M1.1-M1.2.
3. `scripts/bench`: ten fixed coding tasks in throwaway repositories (fix a
   failing test, rename across files, add a CLI flag, ...) run against a real
   model on demand, reporting success, steps, tokens, cache hits, and time.

Exit: CI runs the scripted suite, and every U2/U3 commit body can quote
before-and-after benchmark numbers.

### U2: Tools that finish tasks

1. `edit_file`: replace one exact, unique string (or a list of them) and
   return a short diff; same approval and path rules as `write_file`.
2. `read_file` with line ranges and line numbers; a `glob` tool; `search`
   that skips `.gitignore`d and binary files and caps results.
3. Parallel tool calls: accept several calls per step; run read-only tools
   concurrently and guarded tools in order.
4. Budgets instead of a step count: default about 50 steps per turn with a
   token and wall-clock budget, set in config.
5. Token-aware context: estimate tokens, use the model's window, and compact
   older turns into a summary (fast model) when near the limit.
6. Retained tool results: long output is truncated in context with an id;
   `read_tool_result <id> [range]` fetches the rest.
7. Read-only commands without prompts: a built-in allow set (`git status`,
   `git diff`, `git log`, `ls`, `cat`, `rg`) that still passes the dangerous-
   command and metacharacter checks.

Exit: the benchmark's success rate rises and tokens per task fall against the
Q1 baseline; read-only exploration triggers no approval prompts.

### U3: Terminal that stays out of the way

1. Ctrl-C cancels the running turn and keeps the session; a second Ctrl-C
   exits.
2. Line editing, multiline input, and history per workspace. The standard
   library cannot do raw terminal input, so this is the one place a small,
   audited dependency (or a minimal `termios` binding) is justified; record
   the decision in an ADR.
3. Approval prompt with `y` / `n` / `a` (always for this session) / `p`
   (save as a rule), showing the exact command or the diff of an edit.
4. Edits shown as coloured diffs; long tool output collapsed.
5. Slash commands `/status`, `/usage`, `/clear`, `/compact`, `/model`.
6. Background commands with logs and lifecycle (`start`, `status`, `stop`).

Exit: a full session (edit, test, iterate, interrupt, resume) runs in
`arvm agent` without restarting.

### S2: Sandboxed commands

1. Runner contract from `ARCHITECTURE.md` for `run_command`.
2. Linux sandbox: `bwrap` when installed, otherwise Landlock, with the
   workspace writable, the rest read-only, and no network unless config or an
   approval allows it.
3. In a sandbox, routine commands run without prompts; outside one, current
   behaviour stays.
4. Browser approval panel with exact scope, keyboard flow, and audit log.

Exit: a scripted model cannot write outside the workspace or reach the
network through `sh -c`, `python -c`, or aliases, and a normal
build-and-test loop needs no approvals.

### Q2: Codebase quality (alongside, one small commit at a time)

1. One error type per CLI command returning `Result` to `main`, replacing
   the scattered `process::exit` calls so failures are testable.
2. Split `server.rs` into HTTP parsing, routes, and session handling with no
   behaviour change.
3. Property and fuzz-style tests (deterministic generators, no new
   dependency) for the assembler, permission-rule parser, MCP framing, and
   HTTP request parser.
4. Public API docs for the library modules the CLI and server share; keep
   clippy at `-D warnings`.
5. Keep CI under 5 minutes; cache builds and keep the test suite free of
   network calls and sleeps.

Exit: no module over 1,500 lines, no `process::exit` outside `main`, and
every parser has a generated-input test.

### Later: extensibility, replay, language, protocol

These keep their earlier scope and move behind the work above, because they
add reach rather than making the core loop faster or safer.

- **M1.3-M1.4:** one MCP process per session with supervision; `run_subagent`
  as a CLI tool with bounded child budgets.
- **M3 Replay:** `arvm session replay <id>` on top of the Q1 recording
  contract; corrupt-session recovery and `--no-save`.
- **M4 Language:** locals, call frames, a small source language compiled to
  validated bytecode, `compile_source` and source-level trace tools.
- **M5 Protocol:** ACP over stdio, a versioned HTTP API shared with the
  browser, and a JavaScript SDK.
- **M6 Operations:** shared session store and HTTP edge hardening, LLVM
  backend, signed releases and version channels.

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

And it is quick and efficient when the budgets above hold on the Q1
benchmark, which is checked on every release.
