# A/RVM browser-workspace architecture

A/RVM is a browser-native, agentic workspace for files and the outputs derived
from them. The bytecode interpreter is one deterministic executor inside that
workspace; it is not the product boundary or the primary user experience.

## Product model

An A/RVM workspace owns:

- durable artifacts, artifact versions, jobs, and audit records;
- an isolated runtime with bounded compute, filesystem, and process resources;
- explicit capabilities for every external resource;
- deterministic workflows that invoke a model only for judgment;
- browser, native CLI, and automation clients using the same protocol.

The composition root selects concrete storage, model, executor, and capability
adapters. Core state and policy must never live in terminal or browser rendering.

The browser is a workspace shell, not a remote terminal. It lets users navigate
files, inspect and compare versions, preview artifacts, review agent plans,
approve effects, and retrieve generated outputs. The terminal remains a useful
client surface, but it does not define the data model.

## Artifact model

The artifact graph is the system's durable kernel. A logical artifact is a
stable workspace-visible object such as `budget.xlsx` or `summary.pdf`. An
artifact version is immutable and identifies the exact bytes, MIME type, size,
creator, and declared provenance for one revision of that object.

Every job consumes explicit input versions and emits new output versions. It
never silently overwrites an input. Replacing a file creates a successor
version; restoring an older version creates a new version with that older
version as provenance. This gives the workspace review, comparison, recovery,
reproducibility, and a defensible audit trail.

An artifact record must distinguish at least:

- user-owned source files from generated outputs;
- logical artifact identity from immutable content versions;
- content storage references from user-visible names and locations;
- parent versions, job inputs, and external observations that explain
  provenance;
- retained content from derived, rebuildable previews, indexes, and metadata.

Artifact bytes belong in content storage. Metadata, ownership, version links,
capability decisions, and audit records belong in the durable control-plane
store. Search indexes and previews are derived data and may be rebuilt without
changing artifact identity.

## Planes and trust boundaries

```text
native CLI ─┐
browser ────┼── versioned API/event stream ── workspace control plane
automation ─┘                                  │       │       │
                                                │       │       ├─ scheduler
                                                │       │       ├─ audit/provenance
                                                │       │       └─ artifact metadata
                                                │       └─ capability gatekeepers
                                                │               └─ external systems
                                                └─ runner manager
                                                        ├─ document/sheet workers
                                                        ├─ bytecode VM
                                                        ├─ Wasm/WASI
                                                        └─ native sandbox
```

The protocol authenticates a principal and resolves a workspace. The control
plane authorizes the operation, records observations and effects, and dispatches
to a bounded executor. Credentials stay inside gatekeepers and are represented
to guest code by opaque, least-privilege capabilities.

Runners receive a declared job specification, materialized input versions, and
only the capability subset required for that job. They return bounded outputs
and structured observations; they do not own workspace state, issue credentials,
or make policy decisions.

## Agent work contract

Models propose and coordinate work, but do not receive ambient authority. An
agent request resolves into a plan containing selected artifact versions, the
intended outputs, proposed runners and tools, resource budget, and requested
capabilities. The control plane validates that plan before scheduling work.

Material effects require an explicit approval state when policy says so:

- overwriting or deleting a logical artifact;
- accessing an artifact outside the selected scope;
- sending data to an external service;
- spending a metered resource above the workspace policy;
- creating a public link, sending a message, or otherwise acting outside the
  workspace.

After approval, the scheduler records a job with an idempotency key and runs it
against pinned input versions. The job record, artifact versions, capability
uses, and user-visible result form one traceable work transaction.

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
8. **Artifacts are append-only.** A user can create a successor or restore an
   earlier state, but neither a runner nor a model can silently rewrite history.
9. **Runners are interchangeable.** Document readers, the Rust VM, Wasm/WASI,
   and native sandboxes conform to one bounded job contract.

## Delivery order

1. Artifact and version contracts: stable IDs, ownership, provenance, retention,
   quotas, and content-storage references.
2. Durable workspace metadata with schema versions and crash-safe migrations.
3. Versioned API and event stream shared by browser, CLI, and automation clients.
4. Cancellable jobs and deterministic workflows with idempotency keys.
5. Runner contract, then migrate the Rust VM and existing format readers behind
   that contract.
6. Agent plans, approval gates, and capability delegation.
7. Browser workspace shell: files, previews, artifact history, job activity, and
   reviewable outputs.
8. Wasm/WASI executor with preopened directories and capability-mediated I/O.
9. Native sandbox executor, then skills, MCP, and subagents with delegated
   capability subsets and independent budgets.
10. Packaging, signed releases, benchmarks, fuzzing, tracing, and upgrade policy.

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
