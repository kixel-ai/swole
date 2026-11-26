# [M] Off-CPU wait and futex lock contention time in bench

## Goal

When the setup probe detects L-1 `perf` / tracepoint access (`perf_event_paranoid <= -1` or `CAP_PERFMON`), `swole bench` measures per-benchmark off-CPU blocked time and futex lock-contention wait time scoped to the benchmark's threads, separating on-CPU execution from blocked/contended time in tables and `--report`.

## Plan

Decided 2026-11-26 (maintainer, `/quest-plan`, splitting the former `quest/m0/deep-perf.md`):

- ✅ Split off-CPU and futex contention out of `deep-perf` into its own m0 quest (rejected: keeping bundled with native IBS/PEBS attribution and whole-socket uncore/RAPL).

Recommendations for the implementer:

- Note that plain tracepoint event counting (`perf stat`) only yields event counts, not wait durations. Measure actual wait time via `sched:sched_stat_sleep` / `sched:sched_stat_blocked` (which report nanoseconds when `kernel.sched_schedstats=1`), or by pairing timestamped `sched:sched_switch` and `syscalls:sys_enter_futex` / `sys_exit_futex` records from a perf ring buffer scoped to the benchmark's threads (failing loudly at setup if neither path is available rather than silently reporting zero wait time).
- Report off-CPU time and futex wait time per call alongside wall-clock time in `--report` and changed-benchmark details, and integrate with [derived diagnostics](/quest/m0/diagnostics.md)'s zero-privilege `(user_ns + system_ns) / wall_ns` off-CPU ratio.
- Verify on a real Linux host with a mutex-contended benchmark and a sleeping benchmark, confirming futex contention time moves only on the contended case and off-CPU time matches the blocked duration.

## Required

- [Three modes (quick, sim, bench), capability probe, CAP_PERFMON, L2/L1 perf, and OS rusage](/quest/m0/modes-and-counters.md) - builds on the setup capability probe and `swole bench`

## Hosts

- linux - kernel sched and futex tracepoints
