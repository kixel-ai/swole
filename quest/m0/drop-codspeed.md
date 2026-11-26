# [S] Drop CodSpeed compatibility

## Goal

swole runs only its own harness's benchmarks and programs that bracket theirs with Callgrind's client requests: it drops what it keeps for CodSpeed's crates and instrument hooks, `--cfg codspeed`, `CODSPEED_ISOLATED_BENCHMARK`, the `Metadata: ...` dump rule, and the `ADD_DESC` note, and its README and the harness's stop describing them.

## Plan

Decided 2026-11-25 (maintainer, `/quest-plan` in kixel-ai/rust2, [kixel-ai/rust2#1031](https://github.com/kixel-ai/rust2/pull/1031)): ✅ no CodSpeed service, app, or code remains in any moq-dev or kixel-ai repository. This is swole's part of [rust2's CodSpeed epic](https://github.com/kixel-ai/rust2/blob/main/quest/m1/codspeed/README.md), moved here with the runner.

- A base before a consumer's rewrite still has only CodSpeed's benchmarks, which `time <base>` times as none today; once this lands such a base fails instead, so land it after both consumers' nightly bases are past their rewrites.

## Waiting

- moq-dev/moq: its Criterion benches rewritten to swole's `#[bench]`, its `quest/m9/bench-harness.md`; check `codspeed-criterion-compat` and `criterion` are gone from `Cargo.lock` on moq's `main`.
- kixel-ai/rind: its Criterion benches rewritten to swole's `#[bench]`, its `quest/m2/bench/harness.md`; check `codspeed-criterion-compat` and `criterion` are gone from `Cargo.lock` on rind's `main`.
