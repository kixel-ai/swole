# swole: configless Rust benchmarking with full host counters

## Goal

`swole` (`kixel-ai/swole`) is a configless Rust benchmark tool (`swole` harness + `cargo-swole` CLI) whose single compiled binary serves three distinct roles: `swole quick` for fast local spot-checks across whole workspaces, `swole sim` for deterministic cross-host merge gating with simulated cycles and per-function attribution (replacing CodSpeed), and `swole bench` for paired hardware validation with full host counters and root-cause diagnostics, so developers and agents both catch regressions and immediately see why code is slow, published on crates.io.

Non-goals: per-benchmark configuration knobs; delaying `moq`'s and `rind`'s CodSpeed removal; chasing cross-host bit-exactness in `quick` or `bench` (`sim` alone owns cross-host exact repeatability); full randomized stack/env layout across paired samples (Mytkowicz/Stabilizer) and a macOS `kpc` privileged helper (dropped until a consumer hits them; macOS gets paired wall-clock, `rusage`, and `preload` heap allocation counters).

## Plan

Decided 2026-11-25 (maintainer, `/quest-plan` in kixel-ai/rust2):

- ✅ Goal: configless, standalone-branded Rust benchmark tool with deterministic simulation (replacing CodSpeed) and native full-counter benchmarking probing the host once at setup, plus per-function attribution (rejected: keeping `rust2-bench` name; incremental upgrades over `run`/`--perf`/`time`; dropping).
- ✅ Name: `swole` with `swole` (tiny harness lib + `swole-macros`) and `cargo-swole` (CLI runner shipping `swole` and `cargo-swole`), published to crates.io (rejected: `gains`, `barpath`, `tonnage`, `deadlift`; single combined crate; no crates.io publish).
- ✅ Repository: split into public `MIT OR Apache-2.0` `kixel-ai/swole` right after [kixel-ai/rust2#1033](https://github.com/kixel-ai/rust2/pull/1033) merges (rejected: keeping in `rust2`; creating before it; `GPL-3.0-only`).
- ✅ Three modes on one compiled binary: `swole quick [<base>]` (1× native single-shot perf + heap allocs, host-fingerprinted), `swole sim [<base>]` (Valgrind Callgrind + cache/branch cycle simulation + per-function attribution, CodSpeed replacement, reserving exact repeatability for workloads that demonstrate it and reporting repeat-run spread for threaded or unverified cases), and `swole bench [<base>]` (paired multi-iteration native + all probed host counters), plus an m1 research spike on faster deterministic simulation (QEMU TCG / DynamoRIO) (rejected: 3 modes without the m1 fast-sim spike; 2 modes where `quick` defaults to Valgrind with `--native`).
- ✅ Scope in this epic: capability probe + `CAP_PERFMON` + L2/L1 perf + OS `rusage`, `preload` heap allocation counters, Callgrind per-function attribution, deterministic simulated cycles in `sim`, L0 uncore/RAPL energy + L-1 off-CPU/futex/IBS-PEBS, diff-scoped benchmark selection, exclusive cores for `bench`, and first publish to crates.io.
- ✅ Deferred to m1: [scaling/complexity detection](/quest/m1/scaling.md), [learned A/A noise floors](/quest/m1/noise-floors.md), and [faster simulation spike](/quest/m1/fast-sim.md).
- ✅ Dropped: randomized stack/env layout across paired samples (Mytkowicz/Stabilizer) and macOS `kpc` privileged helper.

m0: `moq` and `rind` are the named consumers, and neither blocks its CodSpeed removal on these upgrades beyond the repository move. (rust2 numbered these milestones m2 and m3; here they are m0 and m1.)

Decided 2026-11-25 (maintainer, [kixel-ai/rust2#1041](https://github.com/kixel-ai/rust2/pull/1041)): ✅ swole moves from `kixel-ai/rust2`'s `bench/runner/` to this repository, keeping its history, filtered to the runner's and these quests' paths, with bare `#N` references rewritten to `kixel-ai/rust2#N`.

Decided 2026-11-26 (maintainer, `/quest-plan` while taking over [kixel-ai/swole#5](https://github.com/kixel-ai/swole/pull/5)):

- ✅ Clarify the root goal and `AGENTS.md` around the three-role model (`quick` local whole-workspace spot-check, `sim` deterministic merge gate, `bench` paired hardware validation + active root-cause diagnosis) with `sim` alone owning cross-host bit-exactness (rejected: keeping current goal with `quick` pursuing exactness; dropping).
- ✅ Drop `quick-accuracy` (1-in-40M interrupt subtraction in `quick`) and expand threaded inherited counters to both `quick` and `bench` so `swole quick` runs whole workspaces without failing on threaded/async benches (rejected: deferring `quick-accuracy` to m1; keeping PR #5's follow-ups as written).
- ✅ Split `deep-perf` into three quests: [native attribution](/quest/m0/native-attribution.md) and [off-CPU/futex contention](/quest/m0/off-cpu-contention.md) stay in m0, while [uncore/RAPL energy](/quest/m1/uncore-rapl.md) moves to m1 (rejected: keeping all three in m0; keeping `deep-perf` combined).
- ✅ Plan new m0 capabilities: derived diagnostics & bottleneck summary (done), [hot-loop inspect mode + function `.text` size](/quest/m0/inspect.md), optional `#[bench]` throughput + `--report` schema version before 0.1.0 (done), and a self-hosted PMU CI runner (deleted 2026-11-27, below). Plan [warm-worktree local bisect](/quest/m1/local-bisect.md) and [adaptive `bench` stopping](/quest/m1/adaptive-bench.md) in m1 (rejected: tail latency `p50`/`p95`/`p99` + `/proc/self/io` counters for now).

Decided 2026-11-27 (maintainer, `/quest-run`): ✅ delete the self-hosted PMU CI runner quest (rejected: provisioning a `swole-ci` self-hosted runner). Public repositories run no PR checks, since agents run them locally; the only synchronous gate is kixel.quest's self-hosted merge queue, and GitHub-hosted runners serve async work such as nightlies and releases.
