# A/RVM production architecture

A/RVM is an agent execution platform. The bytecode interpreter is one executor
inside a larger control plane; it is not the product boundary.

## Product model

An A/RVM workspace owns:

- an isolated runtime with bounded compute, filesystem, and process resources;
- durable sessions, files, outputs, jobs, and audit records;
- explicit capabilities for every external resource;
- deterministic workflows that invoke a model only for judgment;
- native CLI, browser, and automation clients using the same protocol.

The composition root selects concrete storage, model, executor, and capability
adapters. Core state and policy must never live in terminal or browser rendering.

## Planes and trust boundaries

```text
native CLI ─┐
browser ────┼── protocol/API ── workspace control plane ── runtime manager
automation ─┘                         │       │                  │
                                     │       │                  ├─ bytecode
                                     │       │                  ├─ Wasm/WASI
                                     │       │                  └─ native sandbox
                                     │       ├─ durable state
                                     │       └─ jobs/workflows
                                     └─ capability gatekeepers ── external systems
```

The protocol authenticates a principal and resolves a workspace. The control
plane authorizes the operation, records observations and effects, and dispatches
to a bounded executor. Credentials stay inside gatekeepers and are represented
to guest code by opaque, least-privilege capabilities.

## Design principles

1. **Deny by default.** New workspaces and generated code have no host filesystem,
   network, credential, or external-service access.
2. **One contract, many clients.** CLI and browser are clients of a versioned,
   typed protocol. Neither contains authoritative policy or runtime state.
3. **Deterministic where possible.** Known steps are code or workflows; model
   calls are explicit, metered steps used only where judgment adds value.
4. **Policy follows data.** Reads produce observation records. Outputs and
   outbound actions retain provenance so restricted input cannot be laundered
   through a generated artifact.
5. **Durable control plane, disposable executors.** Workspaces and job records
   survive process restarts; executor instances can be recreated from declared
   inputs and state.
6. **Bound every resource.** Instructions, wall time, memory, files, processes,
   output bytes, model steps, network destinations, and concurrency have explicit
   limits with structured errors.
7. **Observable by construction.** Every request has an ID; every state change,
   capability use, model call, job transition, and external effect emits an audit
   event without recording secrets.

## Delivery order

1. Workspace control-plane contract: stable IDs, lifecycle, ownership, quotas,
   capabilities, observations, audit events, and health/readiness state.
2. Versioned JSON protocol shared by the native CLI and browser host.
3. Durable storage with schema versions and crash-safe migrations.
4. Cancellable jobs and deterministic workflows with idempotency keys.
5. Wasm/WASI executor with preopened directories and capability-mediated I/O.
6. Native sandbox executor behind the same interface.
7. Skills and MCP through gatekeepers, then subagents with delegated capability
   subsets and independent budgets.
8. Packaging, signed releases, benchmarks, fuzzing, tracing, and upgrade policy.

## Literature and source architecture

- Cloudflare OS treats the workspace as sessions, persistent state, files,
  outputs, resource access, and an isolated runtime. It uses deny-by-default
  capabilities, gatekeepers, observation tracking, deterministic workflows, and
  separate core/deployment repositories:
  <https://blog.cloudflare.com/cloudflare-os/>.
- WebAssembly separates validation from execution and defines the portable
  execution semantics that the future Wasm/WASI executor should preserve:
  <https://webassembly.github.io/spec/core/>.
- WASI capability-oriented interfaces model filesystem and other host access as
  explicit imports instead of ambient authority:
  <https://wasi.dev/>.
- The Rust Reference defines the obligations around unsafe and FFI boundaries;
  unsafe code should remain isolated in executor adapters:
  <https://doc.rust-lang.org/stable/reference/unsafe-keyword.html>.
- *Crafting Interpreters* remains useful for bytecode representation, compiler,
  and garbage-collector mechanics, but it is executor literature rather than the
  product architecture: <https://craftinginterpreters.com/>.
