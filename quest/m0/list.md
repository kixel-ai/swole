# [M] List benchmarks without running them

## Goal

swole lists a workspace's benchmarks without running any: `swole list [-- <cargo args>]` builds the benchmark targets and prints each benchmark's name a line, as `--benchmarks` reads them, and `quick`, `sim`, and `bench` find their jobs the same way instead of first running every benchmark once natively. A consumer listing rows to then time them, and each CI row, skips that extra native pass.

## Plan

Decided 2026-11-27 (rind maintainer, reviewing [kixel-ai/rind#1141](https://github.com/kixel-ai/rind/pull/1141), which moved rind's benches from Criterion to swole): ✅ plan this. rind's `admin/bench-report names` today runs `cargo bench` with `--cfg=codspeed` and parses the harness's `swole ran ` lines, and the `swole bench` that follows lists by running everything natively again, so each report host and each weekly native row pays two full native passes before timing.

Recommendations for the implementer:

- The harness learns a list request (an environment variable beside `SWOLE_BENCH`) on which it evaluates each benchmark's args, applies the name checks `cases` already makes, prints the names, and exits without running a setup or a call.
- `cargo-swole`'s `list` in `crates/cargo-swole/src/cargo.rs` uses that request for swole targets; a target of CodSpeed's crates keeps listing by running until [Drop CodSpeed compatibility](/quest/m0/drop-codspeed.md) deletes that path.
- A benchmark that panics in setup then fails in its measured run instead of in the listing, which still fails the run.
- Measure the native pass it removes on rind's `rind-bench` (1,924 benchmarks with `__bench_openssl`) and give the time saved in the PR.

## Related

- [A benchmark with empty args lists nothing](/quest/m0/empty-args.md) - rind's rows a host lacks list as nothing once args may be empty
- [Diff-scoped benchmark selection](/quest/m0/diff-selection.md) - selects among the benchmarks a listing finds
