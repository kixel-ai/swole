# [M] Derived diagnostics, bottleneck summary, ref-cycles, Level-2 topdown, and compact diff output

## Goal

`swole` computes actionable derived diagnostics from its counters and `rusage` (MPKI for L1d/L1i/LLC/dTLB/iTLB/branches, branch miss rate, off-CPU/parallelism ratio, kernel share, `ref-cycles` turbo/throttling ratio, and Level-1/Level-2 top-down breakdowns on both Intel P-cores and AMD Zen 4/5), prints a concise one-line root-cause summary beside regressions (including throttling and likely code-alignment shifts), and keeps stdout compact on large suites by sorting by impact and omitting unchanged rows from the details table.

## Plan

Decided 2026-11-26 (maintainer, `/quest-plan`):

- ✅ Add derived diagnostics, `ref-cycles`, Level-2 topdown, bottleneck callouts, and compact diff-first stdout output in m0 so developers and agents immediately see why a benchmark is slow or regressed without manually reading 25 raw rows per benchmark.

Recommendations for the implementer:

- **Zero-overhead derived metrics** (from existing counters + `rusage`, written to `--report` and shown in details):
  - MPKI (`misses * 1000 / instructions`) for L1d, L1i, LLC, dTLB, iTLB, and branches; branch misprediction rate (`branch-misses / branches`).
  - Off-CPU / parallelism ratio from `rusage` + wall clock: `(user_ns + system_ns) / wall_ns` (works on Linux and macOS with zero privileges; `< 0.9` exposes blocking/contention, `> 1.1` reports multi-core parallelism).
  - Kernel share: `instructions:k / (instructions:u + instructions:k)` and `system_ns / (user_ns + system_ns)`.
- **Frequency / throttling detection (`ref-cycles` / reference clock)**:
  - Probe and collect a frequency-independent reference counter where the PMU exports one (Intel `ref-cycles` on `cpu_core`/`cpu_atom`, or AMD `ls_not_halted_p0_cyc` / `msr/mperf/`, never `ls_not_halted_cyc` which tracks actual core cycles) so `cycles / ref-cycles` exposes exact turbo throttling or frequency drift between `<base>` and head; where no frequency-independent reference event exists, compute and clearly label `cycles:u / user_ns` as a frequency estimate.
- **Intel P-core & AMD Zen 4/5 Top-Down (L1 + L2)**:
  - On Intel P-cores (`cpu_core`), collect `slots` + Level-1 (`topdown-retiring`, `topdown-bad-spec`, `topdown-fe-bound`, `topdown-be-bound`) and Level-2 (`topdown-fetch-lat`, `topdown-br-mispredict`, `topdown-mem-bound`, `topdown-heavy-ops`) via a dedicated `read()` pass (since topdown is a percentage breakdown, not an instruction-exact count) or `rdpmc(1 << 29)`.
  - On AMD Zen 4/5, probe `de_no_dispatch_per_slot.*` + `ex_ret_ops` sysfs events for Level-1 top-down percentages.
- **Automatic bottleneck tag & compact stdout**:
  - Tag regressions with concise root-cause hints: e.g. memory/cache-bound (LLC MPKI / `topdown-mem-bound` jump), branch-bound, frontend/likely code-alignment shift (instructions, branches, and cache misses unchanged while time and FE-bound moved 1 to 3%), throttling (`cycles / ref-cycles` dropped), or off-CPU blocked.
  - Keep `--report` complete, while sorting comparison tables by largest absolute change and printing details only for moved benchmarks by default (with `--all-details` or small-suite threshold so a 1,200-benchmark `moq` run does not flood 30,000 rows to stdout). Also format `0 -> N` deltas explicitly instead of leaving the change cell blank.
- Verify with unit tests for every derived ratio, bottleneck classifier rule (including alignment-shift and throttling detection), and compact table formatting, plus an integration test on a real Linux PMU confirming `ref-cycles` and top-down percentages.

## Required

- [Three modes (quick, sim, bench), capability probe, CAP_PERFMON, L2/L1 perf, and OS rusage](/quest/m0/modes-and-counters.md) - builds on the perf probe, counter passes, and `--report`/table output

## Hosts

- linux - `ref-cycles` and Intel/AMD top-down PMU events
