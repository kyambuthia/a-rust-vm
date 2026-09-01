# A/RVM Agent Working Agreement

This file defines the working and commit discipline for the A/RVM repository.
Keep changes small, verifiable, and easy to review.

## Start of every task

- Confirm the working directory is `/home/mbuthi/Projects/a-rust-vm`.
- Run `git status --short` before editing.
- Preserve existing user changes. Do not reset, checkout, stash, or overwrite
  unrelated work.
- Read the smallest relevant set of source files and repository instructions
  before changing behavior.

## Scope and safety

- Keep Rust VM code in `src/` and browser code in `web/`.
- Do not commit `target/`, credentials, API keys, tokens, passwords, `.env`
  files, or personal data. Generated build artifacts belong in ignored paths.
- Do not add dependencies when the standard library or existing project code
  is sufficient.
- Do not deploy, publish, or change GitHub repository settings without an
  explicit user request.
- Treat pushes as public external changes. Never force-push or rewrite shared
  history unless the user explicitly requests it.

## Implementation discipline

- Prefer one logical change per task and one focused commit per logical change.
- Keep the VM deterministic. The LLM, browser, and other integrations must not
  become the authority for arithmetic or VM state.
- Validate inputs at the boundary before passing them to the VM.
- Expose errors explicitly instead of relying on silent fallbacks or accidental
  integer wrapping.
- For frontend changes, keep the terminal-first interaction intact and verify
  keyboard access, responsive behavior, and useful error states.

## Required verification

Run the checks relevant to the files changed. For a normal implementation
change, run all of the following from the repository root:

```bash
cargo fmt -- --check
cargo test
cargo build --target wasm32-unknown-unknown --lib
node --input-type=module --check < web/main.js
```

For frontend changes, also confirm that the local preview serves `web/`, its
stylesheet, its module, and the generated Wasm artifact without HTTP errors.
When browser testing is requested, exercise the primary interaction and check
the console for runtime errors.

Before committing:

```bash
git diff --check
git status --short
```

## Commit rules

- Use Conventional Commit prefixes: `feat`, `fix`, `refactor`, `test`,
  `docs`, `chore`, or `build`.
- Use an imperative subject of no more than 72 characters, without a period.
- Make the subject describe the user-visible or architectural change, for
  example: `feat: add bytecode step debugger`.
- Add a commit body for non-trivial changes. Explain the reason for the change
  and list meaningful verification performed.
- Stage named files deliberately. Do not use `git add -A` when unrelated files
  may be present in the worktree.
- Inspect the staged diff before committing:

  ```bash
  git diff --cached --check
  git diff --cached --stat
  git diff --cached
  ```

- After committing, verify the commit and clean state:

  ```bash
  git log -1 --oneline
  git status --short
  ```

- Never amend an existing published commit by default. Create a follow-up fix
  commit unless the user specifically asks for history cleanup.

## Handoff

Every implementation handoff should state:

- What changed
- Which commit contains it, if committed
- Which verification commands passed
- Any known limitation or next step
