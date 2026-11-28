# [M] L0 uncore memory/L3 bandwidth and RAPL energy in bench

## Goal

When the setup probe detects L0 `perf` access (`perf_event_paranoid <= 0` or `CAP_PERFMON`) and a host-enforced whole-socket lease is available, `swole bench` collects socket-wide uncore L3 and memory-controller/data-fabric bandwidth plus RAPL package/core energy (joules per call), labeled as socket-wide timing-like measurements that include device I/O and DMA.

## Plan

Decided 2026-11-26 (maintainer, `/quest-run`, [kixel-ai/swole#6](https://github.com/kixel-ai/swole/pull/6)): ✅ require exclusive-cores, take a host-enforced whole-socket lease for the uncore/RAPL pass, and report bandwidth, L3 misses, and energy as socket-wide, timing-like measurements that include device I/O and DMA (rejected: also fail the pass unless the host keeps device traffic out, which no host setting can guarantee; report socket-wide readings on shared hosts without a lease, which other agents' traffic would swamp; drop uncore/RAPL).

Decided 2026-11-26 (maintainer, `/quest-plan`, splitting the former `quest/m0/deep-perf.md`): ✅ defer uncore/RAPL to m1 while keeping native attribution and [off-CPU/futex contention](/quest/m0/off-cpu-contention.md) in m0, since a whole-socket lease cannot run on shared dev machines or busy CI runners (rejected: keeping uncore/RAPL in m0; keeping `deep-perf` combined).

Recommendations for the implementer:

- Extend `swole bench`'s core lease (`crates/cargo-swole/src/cores.rs`, a cooperative lock per physical core in `/dev/shm/swole`) with a host-enforced whole-socket lease, active only when the host exposes L0; a pass that cannot take a host-enforced whole-socket lease fails loudly rather than reporting socket-wide counts on a shared host (a cooperative lockfile cannot keep an unrelated process, such as a Nix build, off the socket). A host with more than one package fails the pass too: moving a task's CPUs leaves its memory on the measured socket's controllers, and no host here has two sockets to justify isolating NUMA memory.
- Probe `/sys/bus/event_source/devices/` for `power` / `power_core` (RAPL `energy-pkg`, `energy-cores` with their `*.scale` and `*.unit` sysfs files) and uncore memory/L3 PMUs (`amd_l3`, `amd_df`, `amd_umc_*` on AMD; `uncore_imc_*` on Intel), failing loudly if a probed sysfs event cannot open.
- Report energy in joules (or microjoules) per iteration, and memory bandwidth / L3 misses as socket-wide counts for the pass, labeled clearly as socket-wide measurements that include device traffic (timed like wall clock rather than trusted as exact counts).
- Verify on a real Linux host (`fedora` / `nixos`, AMD Zen 5 or Intel Core Ultra) that a memory-streaming benchmark moves DRAM bandwidth and energy cleanly.

## Hosts

- linux - L0 uncore memory/L3 PMUs and RAPL energy counters
