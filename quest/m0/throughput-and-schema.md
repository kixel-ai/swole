# [S] Optional #[bench] throughput and --report schema version

## Goal

`#[bench]` supports optional throughput annotations (`bytes = <expr>` or `items = <expr>`, zero-config when omitted) so `swole` reports normalized throughput (`GiB/s` or `items/s`, `cycles/byte`, `instructions/byte`) in tables and `--report`, and `--report` JSON carries an explicit `"schema_version": 1` field before the `0.1.0` crates.io publish.

## Plan

Decided 2026-11-26 (maintainer, `/quest-plan`):

- ✅ Settle optional throughput on `#[bench]` and version `--report` JSON before `0.1.0` so publishing `swole` and `swole-macros` does not require an immediate breaking release once codec/parser benchmarks (`moq`, `rind`) want throughput metrics.

Recommendations for the implementer:

- Keep `#[bench]` configless by default: when neither `bytes` nor `items` is given, nothing changes.
- Support `#[bench(bytes = <expr>)]` and `#[bench(items = <expr>)]` (including functions of the case's `args` parameter, e.g. `#[bench(args = [100, 16384], bytes = |n| n)]` or a plain constant/path, matching `setup`'s ergonomics) so parameterized benchmarks scale their throughput automatically.
- Emit throughput metadata from the harness once per case (e.g., `swole throughput bytes <n> <name>`) and have `cargo-swole` emit each count-per-byte/item metric (`instructions/byte`, `cycles/byte`, or `items` equivalents) only when its source count was actually measured in that run, and per-second rates (`bytes/s` formatted as `MB/s` / `GiB/s`, or `items/s`) only when the mode measures native elapsed time for the benchmark window (`bench`, never `sim`'s emulated runtime).
- Add `"schema_version": 1` to the top-level `--report` JSON object and document its stability contract in `README.md`.
- Verify with unit/compile tests in `swole-macros` and an end-to-end fixture benchmark checking `instructions/byte` in `sim` (and `quick` on PMU hosts), `bytes/s` in `bench` on any host, and `cycles/byte` in `bench` only on hosts that supply cycle counts.

## Required

- [Three modes (quick, sim, bench), capability probe, CAP_PERFMON, L2/L1 perf, and OS rusage](/quest/m0/modes-and-counters.md) - builds on the unified harness protocol and `--report` JSON
