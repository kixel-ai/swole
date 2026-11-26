swole counts Rust benchmarks' instructions under stock Valgrind and times them by wall clock, alone or paired with a base commit's: `swole` is the harness a benchmark is written with, a plain `#[bench]` function, `swole-macros` its attribute, and `cargo-swole` the runner, installed as `swole` and `cargo swole`. See [README.md](README.md), [crates/swole/README.md](crates/swole/README.md), and the [quest tree](quest/README.md).

# Required

- Everything is configless, with opinionated names and defaults: a benchmark is a plain function, and a consumer needs no config. Each config key a consumer needs is a gap to close.
- A count that cannot be trusted fails the run: an input swole cannot pin, or a benchmark that measured nothing. One that can still move, such as a count other threads ran beside, is labeled so, never reported as exact.
- A count swole reports as exact repeats across runs, checkouts, and environments that share a fingerprint, and whatever can move it either enters the fingerprint or is pinned, as Valgrind's version, the preload, and the launch's padding are. Prove a change keeps that with the tests, which count under the dev shell's Valgrind.
- The harness stays small, since it is in every consumer's `[dev-dependencies]`: what only the runner needs lives in `cargo-swole`.
- moq and rind are the first consumers of every change.

# Development

Run `just ci` before pushing; outside the dev shell, run `nix develop --command just ...`.

Quests: before any work, run `quest context` and follow it, or read https://github.com/kixel-ai/quest/blob/main/docs/context.md where `quest` is not installed, unless this session already holds its rules.
