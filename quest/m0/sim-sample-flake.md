# [M] Every sim sample counts the same

## Goal

`simulates_caches_and_branches_exactly` in `crates/cargo-swole/tests/run.rs` never fails on a commit that passes it elsewhere: every `sim` sample of a benchmark counts exactly the same, so a report's `lower` and `upper` equal its `value` and two runs write the same report. Reproduce the failure, then fix its root cause in swole. No retry, no looser assertion, and no dropped metric.

## Plan

Requested by the maintainer 2026-11-27.

Evidence: `check` failed in [run 46003750356](https://github.com/kixel-ai/swole/actions/runs/46003750356), attempt 1, on [kixel-ai/swole#21](https://github.com/kixel-ai/swole/pull/21), head `997202ef64f1e9195cc5a113807cd99bd8b70dd8`. That PR changed only quest files. Attempt 2 of the same commit passed, and `main`'s `check` runs are green. The panic:

```
thread 'simulates_caches_and_branches_exactly' panicked at crates/cargo-swole/tests/run.rs:222:5:
assertion `left == right` failed: a second run counts otherwise
```

That is `assert_eq!(text, report("two.json"), "a second run counts otherwise")`. Diffing the two reports in the job log shows every `value` matched. Only spread bounds differed:

- `seq` (the first benchmark) in `one.json` had `upper` above `value` on every metric: I1 and LL instruction misses 198 vs 24, D1/LL read misses 1355 vs 1256, D1/LL write misses 43 vs 9, conditional mispredicts 104 vs 7, indirect mispredicts 26 vs 3, instructions 1402071 vs 1400068, estimated cycles 1580231 vs 1542058. In `two.json`, `upper` equaled `value`.
- `stride` in `two.json` had `lower` *below* `value`: conditional mispredicts 4 vs 5, estimated cycles 2504328 vs 2504348.

So one sample within a run counted differently from its siblings. That breaks AGENTS.md's promise that `sim` counts are bit-exact, and the run-to-run assertion only caught it. Hypotheses to test first:

- A sample in which `seq` runs cold: the first measured iteration in that process pays I-cache, branch-predictor, and data-cache warm-up that later samples do not. The extra I1 misses and mispredicts point here. Find what decides whether `seq`'s sample comes first or warm (sample order, process reuse, warm-up skipped for one sample), and make every sample see the same simulated state, for example by resetting or zeroing Callgrind's simulation state at the bracket, or by running an identical warm-up before each one.
- Something unpinned that leaks into the simulated path: the environment, the stack or heap layout (ASLR, `auxv`, the launch's padding), or thread scheduling under Valgrind. If it is unpinned input, pin it or add it to the fingerprint, as AGENTS.md requires.

Reproduce:

- Loop the test under load in the dev shell until it fails, with the CPU contended the way a shared CI runner is: `nix develop --command bash -c 'cargo test -p cargo-swole --test run --no-run && for i in $(seq 200); do cargo test -p cargo-swole --test run simulates_caches_and_branches_exactly -- --exact || break; done'`, with a CPU hog beside it such as `stress-ng --cpu $(nproc)` or a parallel `cargo build` of another workspace. Run it on Linux x86_64, as CI does (`ubuntu-24.04`).
- Faster signal: run `swole sim --report` directly on the fixture with many samples, and fail as soon as any metric's `lower` or `upper` differs from its `value`. That catches a divergent sample in one run, without waiting for two runs to disagree.
- Once it reproduces, keep Callgrind's per-sample output, or dump each sample's counts, to see which sample diverges and how.

Validation: after the fix, the loop passes a few hundred iterations under load (record the count in the PR). Add a check that a `sim` report's `lower` and `upper` equal its `value` for each metric, so a divergent sample fails within a single run, not only when two runs disagree. Keep the existing run-to-run assertion.

[kixel-ai/swole#20](https://github.com/kixel-ai/swole/pull/20) drops `check`'s `pull_request` trigger. If it lands first, this flake would fail `main`'s post-merge `check` instead of a PR, so the work still matters.

## Hosts

- linux - Valgrind and CI's `check` run on Linux x86_64
