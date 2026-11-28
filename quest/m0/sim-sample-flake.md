# [XS] Every sim sample counts the same

## Goal

`simulates_caches_and_branches_exactly` in `crates/cargo-swole/tests/run.rs` fails within one run when any `sim` sample of a single-threaded benchmark counts differently from its siblings: every metric's `lower` and `upper` equal its `value`, checked beside the existing run-to-run assertion.

## Plan

Requested by the maintainer 2026-11-27.

Decided 2026-11-27 (maintainer, /quest-run): shrink this quest to the guard (rejected: delete it; keep the full investigation). The only evidence, [run 46003750356](https://github.com/kixel-ai/swole/actions/runs/46003750356) on [kixel-ai/swole#21](https://github.com/kixel-ai/swole/pull/21)'s head `997202ef`, predates [kixel-ai/swole#23](https://github.com/kixel-ai/swole/pull/23), which fixed the flake at its root cause by running the fixture before libtest's `main`. `seq`'s `upper` of 1,402,071 against a `value` of 1,400,068 is exactly the 2,003 instructions of libtest's main thread parking inside the counted window, which #23 traced.

- Add the guard: walk every benchmark and measure of each `sim` report the test writes, and fail when a `lower` or `upper` differs from its `value`. The fixture is single-threaded but runs as an unwatched `--program`, so swole samples it and every measure carries bounds.
- Confirm #23 holds: loop the test under CPU load in the dev shell and record the iteration count in the PR. A failure on current `main` is new evidence; root-cause it as the original quest describes.

## Hosts

- linux - Valgrind and CI's `check` run on Linux x86_64
