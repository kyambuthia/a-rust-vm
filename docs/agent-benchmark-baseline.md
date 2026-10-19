# Agent benchmark baseline

## Scripted CLI run

Recorded from clean commit `3c52ec4863748c85aa016e1d98b11379705ee3f9`
on 19 October 2026 with:

```bash
scripts/bench --mode scripted --output target/bench/baseline-3c52ec4.json
```

| Task class | Passed | Model process steps | Tool calls |
| --- | ---: | ---: | ---: |
| Coding | 4/4 | 9 | 6 |
| Document and data | 4/4 | 12 | 8 |
| Recovery | 2/2 | 8 | 6 |
| Policy | 2/2 | 5 | 3 |
| **Total** | **12/12** | **34** | **23** |

The run took 8.3 seconds on this machine and requested zero approvals because
the disposable cases supplied explicit allow or deny rules. The script checked
exact output bytes, unchanged inputs, absence of extra files or symlinks, a
final assistant response, and the expected tool errors in recovery and denial
cases. The result JSON includes hashes of expected and actual files, but lives
under ignored `target/` rather than in the repository.

This baseline predates task records. Current benchmark results use schema 2
and also require a valid task record with a checkpoint to the saved session.

This run checks the CLI, tool loop, permission path, and benchmark grader with
prewritten model decisions. It does not measure autonomous model reasoning.
The scripted model reports no token or provider cost usage; its wall time is
local process and machine dependent. A live-model baseline needs an explicit,
pinned `OPENROUTER_MODEL` and a provider key, and is run only on demand.

## Next measurement

Run the same 12 tasks in live mode and record the model ID, provider settings,
task outcomes, steps, tokens, cost, and elapsed time. Review exact-byte
failures to distinguish wrong work from harmless formatting differences.
Treat the recovery and policy cases as separate gates: a model need not make
the scripted recovery mistake, but it must preserve the protected file and
ignore instructions embedded in workspace content.
