# [L] L0 uncore/RAPL energy and L-1 off-CPU, futex contention, and IBS/PEBS native attribution

## Goal

When the setup probe detects L0 or L-1 `perf` access (`perf_event_paranoid <= 0/-1` or `CAP_PERFMON`), `swole bench` collects uncore L3 and memory-controller/data-fabric bandwidth, RAPL energy (joules per benchmark), `sched`/`futex` off-CPU wait and lock-contention time, and native per-function attribution via AMD IBS / Intel PEBS sampling, reporting them alongside core PMU and wall-clock results.

## Plan

Decided 2026-11-25 (maintainer, `/quest-plan` in kixel-ai/rust2):

- ✅ Include L0 uncore/RAPL and L-1 tracepoint/IBS-PEBS metrics in the `swole` epic. Our Linux hosts run with `perf_event_paranoid = -1` (or `CAP_PERFMON`), and `moq`'s async/contention and memory-bound benchmarks directly benefit from off-CPU wait time, lock contention, memory bandwidth, energy, and native per-function attribution.

Decided 2026-11-26 (maintainer, `/quest-run`, [kixel-ai/swole#6](https://github.com/kixel-ai/swole/pull/6)): ✅ require exclusive-cores, take a host-enforced whole-socket lease for the uncore/RAPL pass, and report bandwidth, L3 misses, and energy as socket-wide, timing-like measurements that include device I/O and DMA (rejected: also fail the pass unless the host keeps device traffic out, which no host setting can guarantee; report socket-wide readings on shared hosts without a lease, which other agents' traffic would swamp; drop uncore/RAPL).

Recommendations for the implementer:

- **L0 uncore & energy** (active only when the host exposes L0 and the pass holds a whole-socket lease through [exclusive cores](/quest/m0/exclusive-cores.md)' lease mechanism, since uncore and RAPL counters are socket/package-wide; a pass that cannot take a host-enforced lease fails the run rather than reporting socket-wide counts on a shared host):
  - Probe `/sys/bus/event_source/devices/` for `power` / `power_core` (RAPL `energy-pkg`, `energy-cores` with their `*.scale` and `*.unit` sysfs files) and uncore memory/L3 PMUs (`amd_l3`, `amd_df`, `amd_umc_*` on AMD; `uncore_imc_*` on Intel), failing loudly if a probed sysfs event cannot open.
  - Report energy in joules (or microjoules) per iteration, and memory bandwidth / L3 misses as socket-wide counts for the pass. The lease excludes other CPU work but not device I/O and DMA, which reach the memory controllers and package power, so label all three as socket-wide measurements that include device traffic, timed like wall clock rather than trusted as exact counts.
- **L-1 off-CPU & contention**:
  - Open `sched:sched_switch` and `syscalls:sys_enter_futex` / `sys_exit_futex` (or `sched:sched_stat_sleep` / `sched:sched_stat_blocked` when available) scoped to the benchmark's threads to separate on-CPU execution from blocked/contended time.
- **L-1 native per-function attribution**:
  - Use AMD IBS (`ibs_op`) or Intel PEBS / `cycles:pp` sampling into a perf ring buffer during a dedicated attribution pass, resolve instruction pointers against the benchmark ELF + loaded DSOs (symbol table + DWARF/symtab), and print per-function native diffs in `swole bench <base>` just as [Callgrind attribution](/quest/m0/callgrind-attribution.md) does for `sim`.
- Verify each tier on a real Linux host (`fedora` / `nixos`, AMD Zen 5 with `amd_l3`, `amd_df`, `amd_umc_*`, `ibs_op`, `power`, `tracepoint`): a memory-streaming benchmark moves DRAM bandwidth and energy; a mutex-contended benchmark moves futex/off-CPU wait time; a regression in one helper function shows up in IBS/PEBS attribution.

## Required

- [Exclusive pinned cores for bench mode](/quest/m0/exclusive-cores.md) - its lease mechanism grants the whole-socket lease the uncore/RAPL pass needs
- [Three modes (quick, sim, bench), capability probe, CAP_PERFMON, L2/L1 perf, and OS rusage](/quest/m0/modes-and-counters.md) - builds on the setup capability probe and multi-pass perf collector

## Hosts

- linux - L0/L-1 perf PMUs, uncore, RAPL, tracepoints, and IBS/PEBS
