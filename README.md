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

## Connect a model process

The native agent command launches the executable named by
`A_RVM_MODEL_PROGRAM`. It sends one JSON request to the child process on
stdin. The child writes one JSON event per line to stdout.

```bash
A_RVM_MODEL_PROGRAM=/path/to/model-bridge \
A_RVM_MODEL_ARGS="--profile local" \
cargo run -- agent
```

The request contains `prompt`, `tool_results`, and the available `tools`.
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
