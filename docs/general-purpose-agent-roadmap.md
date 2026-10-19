# General-purpose autonomous agent roadmap

## Goal and first workload

Build an agent that can take a bounded task, inspect the relevant workspace,
choose tools, perform authorized actions, verify the result, and leave a
reviewable record that survives interruption. The first workload is coding and
file work because A/RVM already has tools and tests for it. The same task,
artifact, runner, and capability contracts must also support document, data,
research, and connected-service work without turning the bytecode VM into a
general-purpose operating system.

The model proposes actions. The control plane owns identity, policy, task
state, budgets, and artifact history. Runners execute declared work with only
the inputs and capabilities granted to that task. The existing integer VM is
one deterministic runner, useful for validation and tracing; it is not the
execution environment for arbitrary programs.

An initial useful task should be able to: inspect files; edit or create an
artifact; run a bounded verification command; explain the result and evidence;
pause for approval before an external or destructive effect; resume after a
restart. Later task classes add document/data processing and connected
services through the same contract.

## Starting point (October 2026)

| Existing capability | Gap to close |
| --- | --- |
| Validated bytecode VM, guest filesystem/process quotas, traces, snapshots | Bytecode cannot run general language runtimes or process documents itself. |
| Native model loop, coding tools, MCP client, permissions, cancellation, sessions | Host command execution is not an OS sandbox; long tool results and context are not retrievable/compacted. |
| Browser guest tools, uploads, jobs, approvals, optional filesystem snapshots | Browser agent turns start fresh; task plans, conversations, and approvals are not one durable cross-client work record. |
| Artifact, content-store, and control-plane modules | They are not yet the sole path for agent inputs, outputs, and provenance. |
| Scripted CLI integration tests | There is no repeatable real-model task benchmark or release gate for autonomous completion. |

This roadmap extends [ROADMAP.md](../ROADMAP.md) and follows the accepted
runner and artifact decisions in [ARCHITECTURE.md](../ARCHITECTURE.md) and
[ADR 0002](adr/0002-capability-gated-runners.md). It does not mark their
planned capabilities as shipped.

## Delivery sequence

Each numbered slice should be a small, independently reviewed change with a
focused test. Move to the next milestone only after its exit gate passes.

### G0. Measure complete tasks

1. Add `scripts/bench` with fixed, disposable workspaces and machine-checked
   outcomes. Start with 12 tasks: four coding, four document/data, two
   multi-step recovery, and two approval or adversarial-input cases.
2. Run the same task set with a scripted model in CI and a pinned real model on
   demand. Record task completion, incorrect or unauthorized effects, model
   steps, tool calls, tokens, cost, elapsed time, and approval count.
3. Save sanitized task traces and expected output hashes so a regression can
   be reproduced without keeping secrets or private prompts.

**Exit:** The current implementation has a published baseline. Every later
milestone can compare task completion and cost against it. All scripted
authorization and interruption cases pass.

In progress: `scripts/bench` has four disposable cases and records a scripted
baseline. The full 12-case set and an explicitly requested live-model baseline
are still open.

### G1. Make a task durable and reviewable

1. Define versioned `Task`, `TaskEvent`, `Plan`, and `Checkpoint` contracts.
   A task has an owner, workspace, selected input artifact versions, budget,
   status, intended outputs, and an append-only event log.
2. Route CLI and browser turns through one task service. Persist conversation,
   tool results, decisions, and current position; resume idempotently after a
   process restart. A cancelled or timed-out task has an explicit terminal
   state.
3. Put agent-created files through artifact versions and content storage.
   Preserve input versions; show output provenance and a diff before a user
   accepts a replacement.

**Exit:** A task can pause, restart, resume, and be inspected from either
client without repeating a material effect. Its final answer links to the
exact output versions and verification events.

### G2. Execute useful work inside real boundaries

1. Implement the accepted runner contract: declared inputs, resource limits,
   cancellation, capabilities, bounded output, and structured errors. Adapt
   the VM and existing deterministic file jobs first.
2. Add an isolated Linux command runner for coding work. Enforce workspace
   mounts, process and memory limits, timeouts, and default-denied network at
   the OS boundary. The current host `run_command` path must not be the
   autonomous execution path.
3. Add a WASI runner for portable user code when a concrete task needs it.
   Pass directories and other imports explicitly. Keep arbitrary native
   workloads in the isolated command tier.
4. Test escapes through shell interpreters, symlinks, subprocesses, network
   calls, oversized output, and cancellation. A denial must leave the task
   state and artifact history consistent.

**Exit:** A routine edit-build-test loop runs without repeated prompts inside
the sandbox, while attempts to access ungranted host paths, credentials, or
network fail in integration tests.

### G3. Make the agent finish and verify work

1. Use a short, revisable plan for multi-step tasks: intended outputs,
   checkpoints, evidence needed, and effects requiring approval. The model
   may revise the plan, but the control plane validates each action.
2. Add token-aware context management and retrievable tool results. Keep
   source content addressable; summaries must point back to the original
   artifact or result rather than become the only evidence.
3. Add explicit verification steps and a completion contract: claimed files,
   commands, and external actions must match successful tool events. On
   failure, the agent can retry within its budget or stop with a useful
   partial-result report.
4. Wire bounded subagents only after child tasks inherit a narrower capability
   set, their own budgets, shared cancellation, and visible parentage.

**Exit:** On the G0 task set, the agent completes at least 8 of 10 non-policy
tasks in two independent runs, with no unauthorized effects in any case.
This is a release target, not a measured result; keep the full 12-task suite,
including the two policy cases.

### G4. Add external reach through capabilities

1. Define typed capability requests for network reads, service reads, writes,
   messages, and spending. Scope grants by principal, workspace, destination,
   operation, data selection, and expiration. Store credentials only in host
   gatekeepers; never pass them to a runner or model.
2. Supervise persistent MCP connections and support a pinned protocol version.
   Validate tool schemas and return sizes, apply the local capability policy
   to every call, and treat server output as untrusted data. Review the current
   MCP specification before implementation rather than assuming the existing
   stdio client covers remote authorization or task semantics.
3. Start with one read-only connector and one effectful connector. Require a
   reviewable preview, idempotency key, and approval for the effectful path.

**Exit:** An agent can research from an approved source and perform one
approved external action exactly once. A changed destination or broader data
scope requires a new decision. Denied calls do not leak data or credentials.

### G5. Ship a trustworthy workspace experience

1. Give users a task queue with plan, progress, evidence, cost, approvals,
   outputs, and a clear cancel/resume path in the browser and CLI.
2. Connect the existing ALB/Cognito identity path to durable workspace
   ownership and a shared store before scaling beyond one service instance.
   Keep task state separate from disposable runner processes.
3. Emit request, model, tool, job, and capability events with correlation IDs.
   Record operational metrics without raw prompts, file contents, or secrets by
   default. Add migration, backup/restore, and failure-injection checks.

**Exit:** A pilot user can complete, inspect, interrupt, and recover a task
across clients and service restarts. Operators can identify failed steps and
resource use without reading private task contents.

## Priority and release rules

- **Critical path:** G0 -> G1 -> G2 -> G3. G4 and G5 expand reach and users
  once the agent can reliably finish bounded local work.
- **Ship vertical slices:** For example, one coding task should create a task
  record, use the sandbox, produce an artifact version, run tests, and show
  evidence before adding another tool family.
- **Keep the VM focused:** Extend its instruction set for VM/debugger use
  cases, not to imitate a shell, Python runtime, or browser.
- **Compare outcomes:** A new feature is valuable when it improves completed
  tasks per unit of time/cost without weakening policy or recovery. Tool count
  and model step count alone are not success metrics.

## First three implementation slices

1. Finish G0's benchmark harness and record today's scripted and live-model
   baseline. This resolves which failures cost the most user time.
2. Add a versioned task/event record and run one existing CLI coding task
   through it, with cancellation and resume tests.
3. Adapt `run_command` to the runner contract and implement the isolated
   command tier behind it; move the benchmark's edit-build-test case onto that
   tier.

## External references to check at implementation time

- [Wasmtime security model](https://docs.wasmtime.dev/security.html) documents
  explicit WASI filesystem capabilities and sandbox limits. Pin and review the
  chosen Wasmtime version before adding the runner.
- [MCP 2026-07-28 release](https://blog.modelcontextprotocol.io/posts/2026-07-28/)
  describes the current protocol's stateless core, task extension, and
  authorization changes. Select a supported revision before changing the MCP
  client.
- [OpenTelemetry semantic conventions](https://opentelemetry.io/docs/specs/semconv/)
  provide a starting vocabulary for tracing. Review the GenAI convention
  version and its content-privacy guidance before instrumenting model calls.
