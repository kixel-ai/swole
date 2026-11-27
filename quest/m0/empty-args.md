# [S] A benchmark with empty args lists nothing

## Goal

A `#[bench(args = ...)]` whose args come out empty at run time has no cases: its target lists, counts, and times its other benchmarks, rather than failing with `<name> has no args`. A consumer drops a row its build or host lacks by returning no args from the expression, with no build flag and no panic in setup.

## Plan

Decided 2026-11-27 (rind maintainer, reviewing [kixel-ai/rind#1141](https://github.com/kixel-ai/rind/pull/1141), which moved rind's benches from Criterion to swole): ✅ plan this. rind then drops its `cfg(graviola)` build flag (set by `rind-bench/build.rs` so a host whose CPU Graviola refuses, such as one without ADX, has no Graviola rows), and its OpenSSL post-quantum rows return no args on OpenSSL before 3.5 instead of panicking in setup.

Recommendations for the implementer:

- Remove the empty check in `crates/swole/src/lib.rs`'s `cases`, and its test, and say in `crates/swole/README.md` that empty args mean no cases.
- A target whose benchmarks all have empty args still fails, since a target that measures nothing is the run swole cannot trust; reword `no function is marked #[bench]` for that case so it names the empty args.
- A name `--benchmarks` gives that a host's empty args removed is still a missing name: `bench` fails on it, and `quick` and `sim` tolerate it only under `--allow-missing`, as today.

## Related

- [List benchmarks without running them](/quest/m0/list.md) - a listing evaluates args but runs no setup, so an empty row costs nothing to list
