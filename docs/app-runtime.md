# Built-in application runtime

The first app surface is intentionally narrower than arbitrary code execution.
It makes two useful apps executable now while preserving the guest boundary:

| App | Guest root | Input | Deterministic output |
| --- | --- | --- | --- |
| Docs | `/workspace/apps/docs` | bounded text, RTF, DOCX/DOCM, or ODT upload | normalized text and document metadata |
| Sheets | `/workspace/apps/sheets` | CSV/TSV or supported Excel/OpenDocument upload | validated table summary and JSON artifact |

The browser can select either app from the bottom navigation deck. Selection
uses server endpoints that act only on the caller's anonymous session; no app
selection is global. The terminal remains the command surface for this first
release so the `/try` layout stays unchanged.

The public API must never accept a command line, executable path, module URL,
or host path as an app operation. Each app operation resolves a fixed guest
path, validates text and size limits, and writes only beneath its own guest
root. Binary upload readers are host-only pure functions that receive copied
guest bytes and return normalized text or cached cell values; they never accept
a command line, host path, module URL, capability, or VM handle.

Supported extensions and the explicit refusals (legacy DOC, encrypted files,
macro/formula execution, and binary write-back) live in
[`format-reader-roadmap.md`](format-reader-roadmap.md).

Future execution tiers:

1. Built-in deterministic apps (this release).
2. Signed or user-uploaded WASI modules with explicit capability grants.
3. Per-session microVMs for arbitrary language runtimes and HTTP services.
