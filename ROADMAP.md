# A/RVM Parity Roadmap

## Product target

A/RVM is a terminal-first, LLM-powered coding agent with an inspectable Rust
virtual machine as its distinctive runtime. It should provide a complete agent
workflow: interactive terminal use, model-backed tool execution, safe project
changes, sessions, extensibility, editor integration, and browser embedding.
The VM, debugger, language, and LLVM work remain first-class features.

## Current state

- [x] Rust integer stack VM
- [x] Arithmetic instructions and explicit execution errors
- [x] Comparison, stack-manipulation, and control-flow instructions (EQ, LT,
  DUP, POP, SWAP, OVER, JMP, JZ) with bounded execution and CFG validation
- [x] `step`, `reset`, instruction-pointer, and stack inspection APIs
- [x] Native CLI demo
- [x] Browser terminal UI
- [x] WebAssembly build and browser debugger exports
- [x] Public GitHub repository
- [x] Project-level agent and commit rules
- [x] Provider-neutral model boundary and streamed agent events
- [x] Native interactive agent command with a line-oriented process bridge
- [x] Workspace list, read, search, write, and command tools
- [x] Workspace containment checks and approval-aware guarded tools
- [x] Local session persistence primitives with atomic saves
- [x] Deterministic model router with profile selection, capability checks, and
  pre-output fallback
- [x] Agent turn loop with bounded tool execution and streamed output
- [x] Workspace and file tools
- [x] Guest-only tool adapter for isolated filesystem and process operations
- [x] Permission runtime for guarded workspace actions
- [x] Persistent session primitives
- [x] Browser event streaming and approval handoff for guarded tools
- [x] Browser upload into a bounded guest filesystem
- [ ] Skills, MCP, subagents, and ACP

## Feature parity matrix

### P0: Core agent foundation

- [x] Typed event protocol for user input, assistant text, tool calls, tool
  results, permissions, errors, and turn completion
- [x] Provider-neutral model interface with streaming text and tool calls
- [ ] Server-side credential boundary; no provider secrets in browser code
- [x] Deterministic route request carried through the model boundary
- [x] System prompt and project-instruction loading
- [x] Conversation state and bounded context assembly
- [x] Tool registry with JSON schemas and deterministic dispatch
- [ ] Cancellation, timeouts, retry limits, and maximum agent steps
- [ ] Machine-readable JSON output alongside terminal text output

### P1: Terminal and CLI

- [ ] Persistent transcript and scrollback
- [ ] Prompt history with up/down navigation
- [ ] Multiline input and interrupt with Escape/Ctrl-C
- [ ] Slash-command menu and command categories
- [ ] Interactive `ask` mode and non-interactive one-shot mode
- [x] Model selection and fast-mode controls
- [ ] Status, version, doctor, usage, and credits commands
- [ ] Clean output mode for scripts and automation
- [ ] Completion behavior and terminal notifications

### P1: Workspace and coding tools

- [ ] List files, glob files, inspect metadata, and open files
- [ ] Lexical search and semantic-search boundary
- [ ] Create, edit, delete, rename, and copy files
- [ ] Directory creation
- [ ] Git-aware workspace context
- [ ] Ignore-file handling and context limits
- [ ] Foreground terminal execution
- [ ] Long-running background commands with logs and lifecycle state
- [ ] Tool-result truncation with retained-result inspection
- [ ] Image attachment and vision input where the host supports it

### P1: Permission and workspace safety

- [ ] `ask`, `auto`, and explicit unrestricted development modes
- [ ] Allow, ask, and deny rules by tool and target
- [ ] Session-scoped approvals
- [ ] Workspace-scoped additional directories
- [ ] Path containment and symlink policy
- [ ] Command policy and dangerous-command handling
- [ ] Fail-closed behavior after session, permission, or workspace changes
- [ ] Human-readable approval prompts with exact action scope
- [ ] Redacted diagnostics and secret-safe logging

### P2: A/RVM runtime and language

- [x] Program object with bytecode validation
- [x] Disassembler and stable instruction names
- [x] Structured execution trace for agent-visible program runs
- [x] Constants, booleans, and comparisons (PUSH, 0/1 flags, EQ, LT)
- [ ] Unary operations
- [ ] Locals and variable storage
- [x] Conditional jumps and loops (JMP, JZ with join-depth validation)
- [ ] Call frames, functions, and return values
- [x] Runtime limits for program size and execution steps
- [ ] Runtime limit for stack depth
- [x] Per-instance virtual filesystem with guest path containment and quotas
- [x] Guest processes with deterministic round-robin scheduling
- [x] Ownership-scoped in-memory VM manager API
- [ ] VM manager with authenticated ownership and lifecycle controls
- [ ] Host sandbox worker for native programs and external capabilities
- [ ] Small source language with lexer, parser, AST, and compiler
- [x] Browser API for loading arbitrary validated programs
- [ ] LLVM backend for optimized native and WebAssembly execution

### P2: Sessions and recovery

- [ ] Stable session IDs
- [ ] Save and resume conversations
- [ ] Session listing and inspection
- [ ] Corrupt-session recovery
- [ ] Prompt history persistence
- [ ] Transcript export
- [ ] Tool-call replay and deterministic VM replay
- [ ] Private local state separated from repository files
- [ ] `--no-save` or equivalent ephemeral mode

### P2: Agent-aware VM experience

- [x] `run_program` tool with validated assembly input
- [x] `compile_program` tool for validated assembly input
- [x] `step_vm` tool with instruction-pointer and stack result
- [x] `inspect_vm` tool for stack, locals, frames, and memory
- [x] `disassemble_program` tool
- [x] `reset_vm` tool
- [ ] LLM explanations grounded in actual VM tool results
- [ ] LLM-generated programs validated before execution
- [ ] Program history and named experiments in the terminal

### P3: Extensibility

- [ ] Skills discovered from project and user roots
- [ ] Explicit skill loading and scoped instructions
- [ ] MCP client with lazy tool discovery
- [ ] MCP resources, prompts, completion, progress, cancellation, and
  subscriptions where supported
- [ ] MCP trust boundaries and permission checks
- [ ] One-off subagents
- [ ] Persistent subagents with independent sessions
- [ ] Parent-child lifecycle, cancellation, and message routing
- [ ] Bounded tool and workspace capabilities for child agents

### P3: Protocol and embedding

- [ ] ACP JSON-RPC 2.0 server over stdio
- [ ] Session create, load, resume, close, list, prompt, cancel, and config
  methods
- [ ] Streamed assistant, tool, status, and permission events
- [ ] JavaScript SDK for the headless agent
- [ ] JavaScript SDK for the interactive terminal
- [ ] Browser host adapters for fetch, config, sessions, history, and storage
- [ ] Native CLI and browser Wasm parity where the host permits it

### P4: Product operations and distribution

- [ ] Configuration layers for defaults, workspace settings, environment
  overrides, and process overrides
- [ ] Model catalog and provider status
- [ ] Usage, cost, and credit reporting
- [ ] Diagnostics and private trace generation
- [ ] Feedback flow without automatic secret or transcript upload
- [ ] Upgrade and version channels
- [ ] Release artifacts for native targets and browser Wasm
- [ ] Documentation, examples, and contribution workflow
- [ ] End-to-end fixtures for deterministic agent turns
- [ ] Performance budgets for startup, binary size, memory, and time-to-first
  prompt

## Proposed repository shape

Keep the current single crate until the core contracts are stable. Then split
by responsibility rather than by individual feature:

```text
crates/
  vm/           bytecode, execution, debugger, memory
  language/     lexer, parser, AST, compiler
  agent/        model loop, prompts, context, cancellation
  tools/        filesystem, search, terminal, VM tools
  permissions/  rules, approvals, grants, path policy
  sessions/     persistence, resume, replay, export
  protocol/     events, JSON, ACP, MCP adapters
  cli/          native terminal application
web/            browser terminal and Wasm host
skills/         project-owned skills and examples
```

The composition root should wire these modules together. Product state should
not live in terminal rendering, and gateway transport should not own tool or
workspace policy.

## Delivery sequence

Each line should become one or more focused commits with tests and docs:

1. Define the typed event, tool, session, and permission contracts.
2. Extract the VM into a reusable core and finish the debugger API.
3. Add a model adapter and a server-side streaming agent loop.
4. Add read-only workspace tools, then guarded write and command tools.
5. Add permission modes, allowlists, workspace boundaries, and cancellation.
6. Add sessions, resume, replay, usage, and diagnostics.
7. Add the small language compiler and VM-specific agent tools.
8. Add skills, MCP, and subagents.
9. Add ACP and the JavaScript/browser embedding layer.
10. Add LLVM and optimized execution paths.
11. Harden, document, benchmark, and package native and Wasm releases.

## Definition of parity

A/RVM reaches practical parity when a user can:

1. Start an interactive terminal or send a one-shot prompt.
2. Ask the LLM to inspect and modify a workspace.
3. See streamed reasoning output, tool calls, tool results, and approvals.
4. Resume or replay a prior session.
5. Run foreground and background commands safely.
6. Configure models, permissions, workspaces, skills, and MCP servers.
7. Delegate isolated work to subagents.
8. Connect an editor through ACP.
9. Embed the agent or terminal in a JavaScript/Wasm host.
10. Ask the agent to compile, execute, inspect, and explain A/RVM programs.

A/RVM will use Rust and should add value through deterministic bytecode,
debugging, language tooling, and LLVM-backed execution. The implementation
should preserve clear ownership boundaries between the agent, tools, policy,
sessions, terminal, and VM.
