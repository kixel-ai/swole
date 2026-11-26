# [M] Count every thread of a threaded benchmark in bench

## Goal

`swole bench` counts a threaded benchmark's perf counters across every thread it runs, labeled statistical as its time is, instead of leaving them out, while `quick` and single-threaded `bench` keep the exact, `rdpmc`-read counts of the calling thread.

## Plan

Decided 2026-11-26 (maintainer, on [kixel-ai/swole#5](https://github.com/kixel-ai/swole/pull/5)):

- ✅ Count a threaded benchmark through inherited counters, started and stopped with `prctl(PR_TASK_PERF_EVENTS_ENABLE/DISABLE)` and read with `read()`, chosen per benchmark at setup from the preload's thread detection, and labeled statistical like time (rejected: per-CPU counters on CPUs the calling thread avoids; building it in #5).
- ✅ Keep this quest as a follow-up of #5 (maintainer, 2026-11-26, [on the PR](https://github.com/kixel-ai/swole/pull/5#issuecomment-7202958517)).

Recommendations for the implementer:

- Decide each benchmark's path before counting, from its counting run's `swole threaded` line: a threaded benchmark's counter passes open the counters with `inherit`, the rest without, so no count falls back mid-run.
- An inherited count holds the `prctl` pair's own kernel work, some 18,000 `instructions:k` a call on an Intel E-core; say so beside its `:k` measures rather than subtract a floor, which subtracts concurrent threads' work.
- Report the threaded measures with a spread from repeated counter passes, as `sim` repeats threaded benchmarks five times, and compare a base and head against it, never as exact counts.
- Verify with the fixture's `handed` and `spawned` benchmarks: their counts include the other thread's spins, and `alone` stays on the exact path.

## Hosts

- linux - perf counters
