# [M] Host-isolated cores for bench

## Goal

`swole bench` times on a core the kernel keeps every other process off, wherever a host sets one aside, so a loaded shared host no longer adds noise to a time through a neighbor scheduled on the leased core or its SMT sibling. Where no host isolation exists, `bench` keeps its cooperative per-core lease and says the core is shared, as it does today.

## Plan

The cooperative lease from [kixel-ai/swole#14](https://github.com/kixel-ai/swole/pull/14) keeps other `swole bench` runs off a core, but not the host's other processes. Its A/A run under load 30–38 on a 32-CPU Ryzen 9 9950X narrowed the median single-threaded 95% interval only from 0.94 to 0.61 percentage points. Some benchmarks also slowed by half on the leased core while a neighbor shared it, and an unpinned process would have migrated away from that neighbor.

Recommendations for the implementer:

- Prefer CPUs in `/sys/devices/system/cpu/isolated` (`isolcpus=` or a cgroup v2 `cpuset` isolated partition), leasing those first, since an unprivileged process may still set its affinity onto them. The PMU probe's CPU list currently filters by `sched_getaffinity`, which leaves isolated CPUs out, so the candidate list must read them separately.
- Fail loudly when a host isolates CPUs that `bench` cannot use, such as CPUs the run's own cpuset excludes, rather than silently falling back to a shared core.
- Verify with the same A/A `swole bench HEAD` on the fixture, under load, isolated against shared, and record the interval change in the PR.

## Waiting

- Maintainer: How should benchmark hosts set cores aside for `swole bench`, given that unprivileged swole cannot evict other processes and the user manager on this host delegates no `cpuset` controller?
  - `isolcpus` on the kernel command line (Recommended) - for example the last two physical cores (`isolcpus=domain,managed_irq,14,15,30,31`); the scheduler places nothing there unless asked, and swole needs no privilege to pin onto them; changing it takes a reboot, and those cores are lost to builds
  - cgroup v2 isolated partition - `systemctl set-property` with `AllowedCPUs` on `system.slice` and `user.slice`, plus a root-owned slice for benchmarks; this needs a privileged helper to place `swole bench` in that slice, but can change without a reboot
  - Keep the cooperative lease alone - no host setup, at the cost of the residual noise measured above
