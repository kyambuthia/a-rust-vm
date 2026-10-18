# A/RVM

A terminal-first Rust VM with a provider-neutral LLM agent boundary.

> **Deployment:** see [`infra/README.md`](infra/README.md) for the ECS/Fargate + ALB + WAF packaging slice. The service intentionally runs a single Fargate task (`desiredCount=1`, min/max 1) because anonymous sessions and VM state are process-local; horizontal scaling waits for durable shared session state.

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
same anonymous guest session, so it can inspect uploaded files without
receiving a host filesystem path.

Use `/files` to list the uploaded guest files independently of the agent.
Browser requests retain access only to the guest-prefixed A/RVM tools.

### Browser server OpenRouter configuration

The hosted browser server calls OpenRouter directly from the server process
via `POST https://openrouter.ai/api/v1/chat/completions` using a cloned,
server-side adapter per request. It does not require the `opencode`
executable and does not shell out to `curl`.

```bash
export OPENROUTER_API_KEY=sk-or-v1-...
export OPENROUTER_MODEL=~openai/gpt-latest
cargo run -- serve
```

- `OPENROUTER_API_KEY` (required): read from the server process environment only. Never put the key in browser responses, source, logs, command arguments, or persisted files.
- `OPENROUTER_MODEL` (optional): explicit model id; defaults to `~openai/gpt-latest` when unset.
- `OPENROUTER_SITE_URL` / `OPENROUTER_SITE_TITLE` (optional): attribution headers sent as `HTTP-Referer` / `X-OpenRouter-Title`; default to `https://asiliano.online` / `A/RVM` when unset.
- `OPENROUTER_TIMEOUT_SECS` (optional): bounded request timeout 1-120s, default 30s with a 10s connect timeout; no unsafe automatic retries.
- `OPENROUTER_MAX_TOKENS` (optional): bounded completion budget 128-4096, default 2048.

If the key is missing or the provider is unavailable, the server still serves
`/web/` and the static Wasm artifact, but `POST /api/agent` returns a bounded
`503` with a sanitized error and no secret leakage.

Uploaded CSV and TSV files can be processed through the browser terminal with
`/tabulate <filename>`, for example `/tabulate sales.csv`. The built-in,
deterministic tabulation job validates a rectangular UTF-8 table, reports row,
column, and numeric-cell counts, and writes its JSON summary to
`/workspace/output/<filename>.table.json`. Use `/jobs` to inspect the job
records. The agent can invoke the same bounded operation with
`guest_tabulate_file` after inspecting an upload. Jobs and output artifacts
currently live only for the local server process; they are deliberately not a
host-shell or arbitrary-executable facility.

Use `/pdf <filename>` to validate that an uploaded file has a PDF header and
create a metadata artifact at `/workspace/output/<filename>.pdf.json`. It
reports the PDF version and a best-effort count of page dictionaries without
parsing document streams. It does **not** extract text: PDF parsing and OCR
remain future isolated job runners, rather than running untrusted document
parsers in the browser host.

The local browser server creates one guest VM per anonymous session and binds
to loopback by default. Container deployments set `A_RVM_BIND_ADDRESS=0.0.0.0`
behind the private task security group. Authenticated multi-user VM selection
is not wired into the browser protocol yet. Host workspace tools remain
available to native coding workflows, but are not registered with the browser
guest agent.

### Anonymous browser sessions

The browser host assigns each visitor an opaque, in-memory session cookie. Each
session owns its guest VM, uploads, jobs, artifacts, and pending approvals.
Sessions expire after 30 minutes of inactivity and the process evicts old
sessions after reaching its bounded session limit. Data is not durable and is
lost when the server restarts or the session expires; do not upload sensitive
information to a public deployment.

State-changing browser requests require the configured same-origin `Origin`
header and an existing anonymous session cookie (the browser obtains it from
the initial page request). For local development the default origin is
`http://127.0.0.1:<port>`. A
deployment behind an HTTPS edge must set both of these variables explicitly:

```bash
A_RVM_ALLOWED_ORIGIN=https://asiliano.online
A_RVM_SECURE_COOKIES=true
```

The anonymous cookie is a session capability, not an identity system. Public
deployments still need an HTTPS edge, WAF or equivalent rate limiting, bounded
container resources, and monitoring before they should be considered safe for
untrusted internet traffic. The current host still binds its application
listener to loopback and uses a process-local session store; multi-instance
durability and managed session storage are future deployment work.

## Run the interactive terminal agent

`arvm agent` and `arvm ask` pick a model in this order:

1. `A_RVM_MODEL_PROGRAM`: an external process (see below).
2. `OPENROUTER_API_KEY`: the OpenRouter API, called directly from `arvm`.
   This is the fast path. `OPENROUTER_MODEL` picks the model.
3. Otherwise the `opencode` bridge, which starts `opencode run` for every
   model step (`openrouter/deepseek/deepseek-v4-flash`). It needs no key in
   `arvm`, but it is much slower: the same two-step task took 44 s through
   the bridge and 8 s natively.

Provide the key through the environment; never place it in browser code,
Git, command arguments, or committed files:

```bash
export OPENROUTER_API_KEY=sk-or-v1-...
export OPENROUTER_MODEL=deepseek/deepseek-v4-flash   # optional
cargo run -- agent
```

Responses stream token by token. Two settings trade speed against cost and
depth; the defaults favour a quick interactive loop:

- `OPENROUTER_REASONING` (`none`, `minimal`, `low`, `medium`, `high`,
  `default`; default `low`). Reasoning tokens are generated before the first
  visible token, so higher effort means a longer wait.
- `OPENROUTER_PROVIDER_SORT` (`throughput`, `latency`, `price`, `default`;
  default `throughput`). OpenRouter's own default favours price; in our
  measurements throughput routing finished turns about twice as fast for a
  few hundredths of a cent more.

After each turn `arvm` prints what it cost, for example
`[usage] 2 model steps · 2,291 in (1,925 cached) · 91 out · $0.000085 · 7.7 s`;
`/usage` in the REPL shows the session total, and `ask --json` emits a
`usage` event per model step with the provider and first-token time.

Optional limits: `OPENROUTER_MAX_TOKENS` (128-32768, default 8192) and
`OPENROUTER_TIMEOUT_SECS` (1-600, default 120).

The hosted browser service must load its OpenRouter key from an AWS-managed
secret or server process environment and must call the provider from the
server side. The hosted runtime does not need OpenCode or any other agent
CLI.

## Permission rules

Guarded tools (`write_file`, `run_command`, MCP tools) ask for approval unless
a rule decides first. Rules take the form
`<allow|ask|deny> <tool|*>[:<target-prefix>]`; the first matching rule wins,
checked in this order:

1. `--rule` flags on the command line.
2. The workspace configuration, `.arvm/config.json`.
3. The user configuration, `config.json` in the config directory
   (`$A_RVM_CONFIG_DIR`, else `$XDG_CONFIG_HOME/a-rust-vm`, else
   `~/.config/a-rust-vm`).
4. Rules saved with `arvm permission add`.

Rules are matched so that an unusual spelling of a target can only make the
outcome more cautious:

- Paths are normalized (`./src//main.rs` is `src/main.rs`). An `allow` prefix
  covers whole components only (`src` does not cover `src_old.rs`); `deny` and
  `ask` prefixes also match as plain string prefixes (`secrets` covers
  `secrets.txt`) and match absolute paths at any component.
- Commands are compared by whole words. An `allow` rule never approves a
  command line containing `;`, `&`, `|`, `$`, backticks, parentheses, or
  redirects. A `deny` or `ask` rule matches any command in such a line, so
  `deny run_command:rm` also blocks `ls; /bin/rm -rf x`.
- `deny` rules override approvals remembered earlier in the session.

### Configuration files

Both configuration files use the same format. Unknown fields and invalid
entries are errors, so a typo never drops a rule silently.

```json
{
  "rules": ["deny run_command:rm", "ask write_file"],
  "mcpServers": {
    "files": { "command": "mcp-files", "args": ["--root", "."] }
  }
}
```

Commands the agent runs, and MCP servers, see only a short allow list of
environment variables (`PATH`, `HOME`, `USER`, `LANG`, `LC_*`, `TERM`,
`TMPDIR`, toolchain homes such as `CARGO_HOME`), never provider keys, cloud
credentials, or tokens. To pass more, list the names in the user
configuration:

```json
{ "envPassthrough": ["GITHUB_TOKEN"] }
```

Tool output is also scanned before it reaches the model, the screen, or a
saved transcript: known token formats (`sk-...`, GitHub, GitLab, Slack, AWS
key IDs, Google API keys), `Bearer` tokens, private-key blocks,
`UPPER_CASE_SECRET=value` lines, and the values of secret-named variables
in your environment are replaced with `[redacted]`. This is a safety net,
not a guarantee; unknown secret formats still get through.

A command runs in its own process group: when it finishes or hits the 30 s
limit, anything it started in the background is ended too.

A workspace configuration ships with the project, so it can only tighten
permissions (and cannot set `envPassthrough`): an `allow` rule there is refused. Put `allow` rules in the user
configuration or pass them with `--rule`. `arvm config show [--json]` prints
both files and what they declare.

### MCP servers

`arvm agent` and `arvm ask` start the configured MCP servers and offer their
tools as `mcp_<server>_<tool>`. Every MCP tool call is guarded: it asks for
approval unless a rule such as `allow mcp_files_read` decides first, and
one-shot `ask` denies it without `--auto`.

Servers from the user configuration start automatically. A server declared
in `.arvm/config.json` could run anything, so it starts only after you trust
it for that workspace:

```bash
arvm mcp list            # servers, their scope, and whether they are enabled
arvm mcp trust files     # approve the workspace server 'files' as written
arvm mcp tools files     # start it and list its tools
arvm mcp untrust files
```

Trust is pinned to the exact entry, so changing its command, arguments,
environment, or timeout disables the server until you trust it again. A
trusted workspace server replaces a user server of the same name. A server
that fails to start is reported and skipped. Each tool call currently starts
a fresh server process.

Command deny rules are a convenience, not a sandbox: a determined command can
still disguise what it runs. The built-in dangerous-command refusal and
workspace containment apply regardless of rules.

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
