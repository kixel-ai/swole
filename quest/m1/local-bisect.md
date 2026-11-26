# [S] Warm-worktree local regression bisect (swole bisect)

## Goal

`swole bisect <good> <bad> --benchmarks <name>` (or `<bench>`) automates binary search across git history inside `swole`'s cached warm worktree using `quick` or `sim`, keeping `target/` warm across bisect steps so local regression hunts finish in seconds instead of rebuilding from scratch per commit.

## Plan

Decided 2026-11-26 (maintainer, `/quest-plan`):

- ✅ Plan warm-worktree local `swole bisect` in m1 so developers and agents can pinpoint the exact regressing commit locally in one command using the worktree and `--allow-missing` machinery `swole` already maintains.

Recommendations for the implementer:

- Reuse `compare.rs`'s cached detached worktree and lock so Cargo builds incrementally across bisect steps without touching the caller's working tree or index.
- Support `--mode sim|quick` (defaulting to `sim` for exactness, or `quick` when asked for speed), measure `<good>` and `<bad>` first to verify a clear regression exists, and classify each midpoint relative to the good/bad gap (refusing ambiguous midpoints or failing cleanly when the benchmark did not exist yet).
- Print the first bad commit SHA, subject, and measured progression table at the end.
- Verify with an end-to-end test over a synthetic 8-commit scratch git repository where commit 5 doubles a benchmark's spin loop, confirming `swole bisect` identifies commit 5.

## Required

- [Three modes (quick, sim, bench), capability probe, CAP_PERFMON, L2/L1 perf, and OS rusage](/quest/m0/modes-and-counters.md) - builds on the unified `quick`/`sim` modes and cached compare worktree
