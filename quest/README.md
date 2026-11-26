# swole: configless Rust benchmarking with full host counters

## Goal

`swole` (`kixel-ai/swole`) is a configless Rust benchmark tool (`swole` harness + `cargo-swole` CLI) that compiles every benchmark binary once and runs it across three modes—`swole quick` (1× native single-shot perf + allocs), `swole sim` (deterministic Valgrind Callgrind + simulated cycles + attribution, replacing CodSpeed), and `swole bench` (paired multi-iteration native timing + every hardware counter and OS metric the host exposes)—so performance regressions surface with their root cause instead of bare wall-clock time, published on crates.io.

Non-goals: per-benchmark configuration knobs; delaying m1's CodSpeed removal; randomized stack/env layout across paired samples (Mytkowicz/Stabilizer) and a macOS `kpc` privileged helper (dropped until a consumer hits them; macOS gets paired wall-clock, `rusage`, and `preload` heap allocation counters).

What stays here once every child has merged: `swole`'s top-level README and docs covering all three modes, capability tiers, and counter definitions.

## Plan

Decided 2026-11-25 (maintainer, `/quest-plan` in kixel-ai/rust2):

- ✅ Goal: configless, standalone-branded Rust benchmark tool with deterministic simulation (replacing CodSpeed) and native full-counter benchmarking probing the host once at setup, plus per-function attribution (rejected: keeping `rust2-bench` name; incremental upgrades over `run`/`--perf`/`time`; dropping).
- ✅ Name: `swole` with `swole` (tiny harness lib + `swole-macros`) and `cargo-swole` (CLI runner shipping `swole` and `cargo-swole`), published to crates.io (rejected: `gains`, `barpath`, `tonnage`, `deadlift`; single combined crate; no crates.io publish).
- ✅ Repository: split into public `MIT OR Apache-2.0` `kixel-ai/swole` right after `#1033` merges (rejected: keeping in `rust2`; creating before `#1033`; `GPL-3.0-only`).
- ✅ Three modes on one compiled binary: `swole quick [<base>]` (1× native single-shot perf + heap allocs, host-fingerprinted), `swole sim [<base>]` (Valgrind Callgrind + cache/branch cycle simulation + per-function attribution, CodSpeed replacement, reserving exact repeatability for workloads that demonstrate it and reporting repeat-run spread for threaded or unverified cases), and `swole bench [<base>]` (paired multi-iteration native + all probed host counters), plus an m3 research spike on faster deterministic simulation (QEMU TCG / DynamoRIO) (rejected: 3 modes without the m3 fast-sim spike; 2 modes where `quick` defaults to Valgrind with `--native`).
- ✅ Scope in this epic: capability probe + `CAP_PERFMON` + L2/L1 perf + OS `rusage`, `preload` heap allocation counters, Callgrind per-function attribution, deterministic simulated cycles in `sim`, L0 uncore/RAPL energy + L-1 off-CPU/futex/IBS-PEBS, diff-scoped benchmark selection, exclusive cores for `bench`, and first publish to crates.io.
- ✅ Deferred to m3: [scaling/complexity detection](/quest/m1/scaling.md), [learned A/A noise floors](/quest/m1/noise-floors.md), and [faster simulation spike](/quest/m1/fast-sim.md).
- ✅ Dropped: randomized stack/env layout across paired samples (Mytkowicz/Stabilizer) and macOS `kpc` privileged helper.

m2: `moq` and `rind` are the named consumers, and neither blocks its CodSpeed removal on these upgrades beyond the repository move. These were rust2's milestones; in this tree, rust2's m2 is m0 and its m3 is m1.

Decided 2026-11-25 (maintainer, [kixel-ai/rust2#1041](https://github.com/kixel-ai/rust2/pull/1041)): ✅ swole moves from `kixel-ai/rust2`'s `bench/runner/` to this repository, keeping its history, filtered to the runner's and these quests' paths, with bare `#N` references rewritten to `kixel-ai/rust2#N`.
