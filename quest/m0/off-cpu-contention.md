# [M] Off-CPU blocked time and futex wait time in bench

## Goal

When the setup probe detects L-1 `perf` / tracepoint access (`perf_event_paranoid <= -1` or `CAP_PERFMON`), `swole bench` measures per-benchmark off-CPU blocked time and futex wait time scoped to the benchmark's threads (distinguishing general blocked/parked time from identified lock contention), separating on-CPU execution from waiting time in tables and `--report`.

## Plan

Decided 2026-11-26 (maintainer, `/quest-plan`, splitting the former `quest/m0/deep-perf.md`):

- ✅ Split off-CPU and futex wait measurement out of `deep-perf` into its own m0 quest (rejected: keeping bundled with native IBS/PEBS attribution and whole-socket uncore/RAPL).

Recommendations for the implementer:

- Note that plain tracepoint event counting (`perf stat`) only yields event counts, not wait durations, and scheduler sleep/blocked records alone lack futex attribution. Measure total off-CPU blocked time from scheduler records (`sched:sched_stat_sleep` / `sched:sched_stat_blocked` when `kernel.sched_schedstats=1`, or paired `sched:sched_switch` timestamps from a perf ring buffer), correlate `syscalls:sys_enter_futex` / `sys_exit_futex` operations with those intervals to measure futex wait time, and reserve any lock-contention label for waits identified as lock acquisitions rather than async workers parked idle on a futex waiting for work. Fail loudly at setup if the required tracepoint path is unavailable rather than silently reporting zero wait time.
- Report off-CPU blocked time and futex wait time per call alongside wall-clock time in `--report` and changed-benchmark details, and integrate with [derived diagnostics](/quest/m0/diagnostics.md)'s zero-privilege `(user_ns + system_ns) / wall_ns` off-CPU ratio.
- Verify on a real Linux host with a mutex-contended benchmark, an idle-parked async worker, and a sleeping benchmark, confirming blocked time matches the sleep, futex wait captures futex waits, and only lock acquisitions carry a lock-contention label.

## Required

- [Three modes (quick, sim, bench), capability probe, CAP_PERFMON, L2/L1 perf, and OS rusage](/quest/m0/modes-and-counters.md) - builds on the setup capability probe and `swole bench`

## Hosts

- linux - kernel sched and futex tracepoints
