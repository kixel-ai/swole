# [S] Adaptive early-stopping budget for swole bench

## Goal

`swole bench` adapts its per-benchmark sampling duration to statistical convergence instead of spending a fixed 1 s `BUDGET` on every benchmark, stopping early once a paired 95% confidence interval is tight (or clearly separated from 0%) and spending slightly longer on borderline benchmarks, cutting `bench` wall-clock time 3 to 5x on large suites without sacrificing statistical confidence.

## Plan

Decided 2026-11-26 (maintainer, `/quest-plan`):

- ✅ Plan adaptive stopping for `swole bench` in m1 so large suites like `moq`'s 1,200+ benchmarks finish in minutes rather than 20+ minutes while keeping paired statistical rigor.

Recommendations for the implementer:

- Keep a minimum sample/pair floor (at least `MIN_SAMPLES` and a short minimum wall window such as 100 ms so drift and periodic load still have a chance to appear) and a hard upper cap (such as 3 s) so noisy benchmarks cannot stall a run.
- Check convergence using sequentially valid inference for both stopping and reporting: either alpha spending across planned geometric checkpoints (e.g., 20, 40, 80, 160 pairs) or an anytime-valid confidence sequence for the median ratio, never a naive fixed-sample 95% interval peeked repeatedly.
- For standalone `swole bench` (no `<base>`), stop once the median's relative half-width stabilizes below a tight threshold after the minimum window.
- Verify with unit tests over synthetic `Fake` sides (clean constant ratio stops early; noisy borderline ratio runs past 1 s up to the cap) and record the wall-clock speedup on a real multi-benchmark suite.
