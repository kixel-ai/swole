# [M] Count every thread of a threaded benchmark in quick and bench

## Goal

`swole quick` and `swole bench` count a threaded benchmark's perf counters across every thread it runs, labeled statistical (`~`) like wall-clock time instead of failing `quick` or leaving counters out in `bench`, while single-threaded benchmarks in `quick` and `bench` keep the `rdpmc`-read calling-thread path.

## Plan

Decided 2026-11-26 (maintainer, on [kixel-ai/swole#5](https://github.com/kixel-ai/swole/pull/5)):

- ✅ Count a threaded benchmark through inherited counters, started and stopped with `prctl(PR_TASK_PERF_EVENTS_ENABLE/DISABLE)` and read with `read()`, chosen per benchmark at setup from thread detection, and labeled statistical like time (rejected: per-CPU counters on CPUs the calling thread avoids; building it in #5).
- ✅ Expand this quest from `bench` alone to cover both `quick` and `bench` (maintainer, 2026-11-26, `/quest-plan`, dropping `quick-accuracy.md`), so `swole quick` runs whole workspaces containing threaded/async benchmarks without aborting (rejected: keeping `quick` refusing threaded benchmarks).

Recommendations for the implementer:

- Decide each benchmark's path before counting, from thread observation: a threaded benchmark's counter passes open the counters with `inherit`, the rest without, so no count falls back mid-run.
- An inherited count holds the `prctl` pair's own kernel work, some 18,000 `instructions:k` a call on an Intel E-core; say so beside its `:k` measures rather than subtract a floor, which subtracts concurrent threads' work.
- Report the threaded measures as statistical (`~`, with a spread from repeated counter passes as `sim` repeats threaded benchmarks five times), and compare base and head against that spread rather than as exact counts.
- Verify with the fixture's `handed` and `spawned` benchmarks: both `swole quick` and `swole bench` succeed on the whole fixture, their threaded counts include the other thread's spins, and `alone` stays on the `rdpmc` path.

## Hosts

- linux - perf counters
