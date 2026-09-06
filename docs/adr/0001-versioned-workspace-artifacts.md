# ADR 0001: Version workspace artifacts immutably

## Status

Accepted

## Context

A/RVM must let users and agents work across source files and generated outputs
without making a model claim the source of truth. Process-local guest files are
useful for bounded execution, but they cannot provide durable review, recovery,
or provenance for a browser workspace.

## Decision

The workspace will model a user-visible file or output as a logical artifact
with immutable artifact versions. A version records its content reference, MIME
type, byte size, creator, creation time, and provenance links to input versions
and the job that created it.

Jobs consume pinned versions and create new versions. Replacing a logical file
creates a successor version. Restoring a prior version creates another new
version; history is never rewritten.

Raw bytes are held by content storage. The control-plane database owns artifact
identity, version metadata, ownership, retention, provenance, and audit links.
Previews, extracted text, indexes, and thumbnails are derived data and may be
rebuilt.

## Consequences

- Users can inspect lineage, compare outputs, and recover safely.
- Agent work is reproducible from declared inputs and job specifications.
- Storage garbage collection must respect version retention and provenance.
- Mutable guest filesystems remain runner-local implementation details rather
  than the workspace's authoritative file store.
