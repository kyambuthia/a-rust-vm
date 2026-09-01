# A/RVM

A terminal-first Rust VM with a provider-neutral LLM agent boundary.

## Run the VM demo

```bash
cargo run
```

## Run the offline agent demo

```bash
cargo run -- agent-demo
```

To exercise the agent's compile and trace workflow offline:

```bash
cargo run -- runtime-demo
```

To exercise isolated guest filesystems and guest processes:

```bash
cargo run -- guest-demo
```

The guest tool adapter is available to embedders through
`guest_tools::guest_tool_registry`. It exposes guest-prefixed filesystem and
process tools backed by an in-memory `VmInstance`; these tools do not read the
host workspace or invoke a host shell. The host must create the instance for
the authenticated user and session before handing the registry to an agent.

To exercise the coding-agent tools against this workspace:

```bash
cargo run -- coding-demo
```

## Run the browser agent

Build the Wasm runtime and start the local browser host:

```bash
cargo build --target wasm32-unknown-unknown --lib
cargo run -- serve
```

Open `http://127.0.0.1:8080/web/`. Natural-language input goes to the
server-side agent; VM expressions and slash commands remain available
alongside it. Agent events arrive as newline-delimited JSON while the turn is
running. Use the `upload` control to copy files into the guest VM at
`/workspace/uploads/<filename>`. Uploads are binary-safe, limited to 1 MiB per
file, and accept only a single filename component. The browser agent uses the
same process-lifetime guest VM, so it can inspect uploaded files without
receiving a host filesystem path.

The local browser server currently creates one guest VM per server process and
binds to loopback. Authenticated multi-user VM selection is not wired into the
browser protocol yet. Host workspace tools remain available to native coding
workflows, but are not registered with the browser guest agent.

## Run the interactive terminal agent

With the local model runner installed and authenticated, the interactive agent
starts without extra bridge configuration:

```bash
cargo run -- agent
```

The built-in bridge uses `openrouter/deepseek/deepseek-v4-flash` as its fixed
model. It only forwards requests and structured responses; credentials remain
in the runner's protected configuration.

## Connect an external model process

The native agent command can launch an external executable named by
`A_RVM_MODEL_PROGRAM`. It sends one JSON request to the child process on stdin.
The child writes one JSON event per line to stdout.

```bash
A_RVM_MODEL_PROGRAM=/path/to/model-bridge \
A_RVM_MODEL_ARGS="--profile local" \
cargo run -- agent
```

The request contains `prompt`, `tool_results`, and the available `tools`.
It also contains a `route` object describing the requested model profile and
required capabilities. The interactive command supports `/model <name>`,
`/fast`, `/strong`, `/default`, and `/status` route controls. Set
`A_RVM_MODEL_NAME` when the process should be addressed by a specific route
name; it defaults to `configured`.
Native agent requests also include the baseline system contract, the contents
of a non-empty workspace `AGENTS.md` when present, and a bounded transcript of
earlier turns. The transcript is kept in memory for the current process and
does not write additional files.
Supported response events are:

```json
{"type":"text_delta","text":"hello"}
{"type":"tool_call","id":"call-1","name":"inspect_vm","arguments":{}}
{"type":"error","message":"request failed"}
{"type":"done"}
```

The process is launched directly without a shell. Keep credentials in the
model bridge's protected credential store or environment, never in this
repository or in command arguments.

Type `/exit` or `/quit` to leave the interactive agent.
