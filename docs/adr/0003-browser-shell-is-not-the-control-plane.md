# ADR 0003: Make the browser a workspace shell

## Status

Accepted

## Context

The current browser experience is terminal-first and uses an anonymous,
process-local session. That is useful for a contained demo but does not define a
multi-file browser workspace with durable outputs, approvals, and history.

## Decision

The browser will be a client of the versioned control-plane protocol. It will
render workspace navigation, file and artifact previews, agent plans, approval
requests, job activity, and version history. It must not own authoritative
workspace state, permission policy, job lifecycle, or model credentials.

The native CLI and automation clients use the same protocol. Terminal commands
remain supported where useful, but they are alternate views and controls over
the same workspace objects.

## Consequences

- Browser restarts and multiple browser instances cannot lose or fork
  authoritative state.
- The server must replace anonymous process-local sessions with authenticated,
  durable workspace selection before multi-user deployment.
- The UI can evolve from a terminal toward file-focused applications without
  changing runner or policy behavior.
