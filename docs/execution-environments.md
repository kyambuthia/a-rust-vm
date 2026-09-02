# A/RVM execution environments

This document records the execution boundary for browser apps before code is
added. It is deliberately short: an execution environment is a product and
security contract, not just a process launcher.

## Current boundary

The public browser host owns an anonymous, in-memory `VmInstance` per session.
That instance has a bounded virtual filesystem and deterministic A/RVM bytecode
processes. It has no access to the host filesystem, host shell, credentials, or
ambient network access.

The Docs and Sheets apps use this boundary in their first version. They are
built-in, deterministic application runtimes operating only on the guest
filesystem. They do not evaluate user-provided JavaScript, Python, binaries,
macros, formulas, or document parsers.

## App-runtime contract

- An app has a stable identifier and a fixed guest root under `/workspace/apps`.
- Inputs are validated at the API boundary and bounded by the existing guest
  byte and inode quotas.
- Results are plain text or JSON artifacts written in the guest filesystem.
- Apps have no host capability by default. Network, shell, service ports, and
  package installation are not app capabilities in this phase.
- The server records app operations in the session-local job store so users can
  inspect completed work without trusting an LLM claim.

## Why arbitrary code is a later phase

WebAssembly runtimes can use a capability model: a module receives only the
filesystem directories, clocks, randomness, and network capabilities that a
host explicitly grants. Wasmtime documents this model and its memory isolation
properties in its security guide. That is the right next boundary for
user-provided WASI modules, but it requires explicit module validation,
preopened-directory policy, CPU/memory/fuel limits, output limits, and a clear
artifact format.

For arbitrary native workloads in a public multi-tenant service, a process in
the browser host is not an acceptable boundary. Firecracker documents the
defense-in-depth requirements for microVM workloads: a per-tenant VM, cgroup
quotas, syscall filtering, dropped privileges, and host-level egress filtering.
That is the eventual execution tier for language runtimes and services.

Cloudflare's Sandbox SDK is a useful reference for the operations an execution
environment eventually needs—isolated files, processes, output capture,
sessions, and exposed ports—but it targets Workers Containers. A/RVM currently
uses an AWS-oriented Rust host, so it is reference material rather than a
dependency in this repository.

## Sources

- Wasmtime security guide: <https://docs.wasmtime.dev/security.html>
- Firecracker design and sandboxing: <https://github.com/firecracker-microvm/firecracker/blob/main/docs/design.md>
- Firecracker production-host guidance: <https://github.com/firecracker-microvm/firecracker/blob/main/docs/prod-host-setup.md>
- Cloudflare Sandbox SDK overview: <https://developers.cloudflare.com/sandbox/>
- Cloudflare Sandbox SDK API reference: <https://developers.cloudflare.com/sandbox/api/>
