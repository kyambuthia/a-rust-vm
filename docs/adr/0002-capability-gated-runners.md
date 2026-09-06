# ADR 0002: Run file work through capability-gated runners

## Status

Accepted

## Context

A/RVM will process documents, spreadsheets, images, code, and VM programs. A
model or a browser client cannot safely hold ambient filesystem, network,
credential, or process authority while doing that work.

## Decision

Every executor implements one runner contract. A runner receives a declared job
specification, materialized immutable input versions, bounded resource limits,
and an explicit subset of capabilities. It returns structured observations and
bounded output artifacts.

The workspace control plane, not the runner, resolves principals, checks policy,
issues opaque capabilities, records effects, and owns durable state. The Rust
VM, deterministic format readers, Wasm/WASI modules, and future native
sandboxes are peers behind this contract.

## Consequences

- New file-processing features do not gain host authority by default.
- Runners can be replaced or isolated more strongly without changing browser
  or agent contracts.
- Resource accounting and cancellation are consistent across execution types.
- Existing in-memory guest filesystems need an adapter before they can become
  durable workspace workers.
