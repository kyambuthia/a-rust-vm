# Agent Loop Hardening

Status: proposed design
Scope: bounded, deterministic agent turns that end with text, not by burning steps

## Problem

The streaming agent loop (`src/agent.rs`, `run_streaming_with_approval`) runs up to
`max_steps` model/tool iterations. Observed failure: with Muse via OpenRouter, the
model repeatedly invoked `guest_list_files` with the identical argument and the
identical empty result until the step limit fired. The loop cannot distinguish "new
information" from "no state change," cannot be cancelled early, and has no
turn-level time budget.

## Design

### 1. Repeated identical tool-call guard

Track a per-turn signature for each executed tool call:

- tool name
- canonical JSON arguments (parsed `Value`, so key order does not matter)
- result content
- result `is_error`

If a new call is identical to the immediately previous executed call (same name,
same args, same result content, same error flag), executing it again cannot change
state. Stop the turn before executing, emit a dedicated event
(`RepeatedToolCall`), and finish gracefully rather than looping to the step limit.

Complementary system-prompt guidance: if a tool result is empty or unchanged,
answer directly; do not re-invoke the same tool with the same arguments.

Follow-up slices (not in this change): distinct-call cap for non-identical loops,
turn deadline, cancellation, bounded retries.

### 2. Turn-level time budget

Per-request timeout already exists (`OPENROUTER_TIMEOUT_SECS`, default 30s). Add a
turn deadline on the `Agent` (configurable), checked before each model request and
after each tool execution. Expiry produces a distinct timeout event.

### 3. Cancellation

Add an `Arc<AtomicBool>` cancel flag checked between steps. Wire it from:
- stream write failure (client disconnect)
- a session-scoped cancel endpoint

Mid-request abort of the blocking provider call is a documented limitation in the
first phase; cancellation is honored between steps and waits at most the request
timeout during an in-flight model call.

### 4. Bounded retries

Provider-neutral `RetryingModel` wrapper. Retry only transient failures (network,
408, 429, 5xx) with bounded backoff; never retry 4xx model errors or mid-stream
failures after partial output.

## Verification

- Unit tests for the guard with a scripted model: duplicate call is not executed
  twice and produces `RepeatedToolCall`; differing args/results are not flagged.
- Existing discipline: `cargo fmt -- --check`, `cargo test`,
  `cargo build --target wasm32-unknown-unknown --lib`,
  `node --input-type=module --check < web/main.js`.
- One focused commit per slice.
