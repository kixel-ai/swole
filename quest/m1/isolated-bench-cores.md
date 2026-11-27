# [M] Host-isolated cores for bench

## Goal

`swole bench` times on a core the kernel keeps every other process off, wherever a host sets one aside, so a loaded shared host no longer adds noise to a time through a neighbor scheduled on the leased core or its SMT sibling. Where no host isolation exists, `bench` keeps its cooperative per-core lease and says the core is shared, as it does today.

## Plan

The cooperative lease from [kixel-ai/swole#14](https://github.com/kixel-ai/swole/pull/14) keeps other `swole bench` runs off a core, but not the host's other processes. Its A/A run under load 30–38 on a 32-CPU Ryzen 9 9950X narrowed the median single-threaded 95% interval only from 0.94 to 0.61 percentage points. Some benchmarks also slowed by half on the leased core while a neighbor shared it, and an unpinned process would have migrated away from that neighbor.

Recommendations for the implementer:

- Prefer CPUs in `/sys/devices/system/cpu/isolated` (`isolcpus=`), leasing those first, since an unprivileged process may still set its affinity onto them. Check whether the run's `sched_getaffinity` and cpuset include them rather than assuming either way, since the PMU probe's CPU list filters by affinity.
- Fail loudly when a host isolates CPUs that `bench` cannot use, such as CPUs the run's own cpuset excludes, rather than silently falling back to a shared core.
- Verify with the same A/A `swole bench HEAD` on the fixture, under load, isolated against shared, and record the interval change in the PR.

Decided 2026-11-27 (maintainer, /quest-run): ✅ keep this follow-up (rejected: drop).

Decided 2026-11-27 (maintainer, /quest-run): hosts set cores aside with ✅ `isolcpus` on the kernel command line, for example the last two physical cores (`isolcpus=domain,managed_irq,14,15,30,31`) (rejected: a cgroup v2 isolated partition with a privileged helper; the cooperative lease only). Discover them from `/sys/devices/system/cpu/isolated`; a cgroup partition, which that file does not list, is out of scope.
