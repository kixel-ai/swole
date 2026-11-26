swole measures Rust benchmarks from one compiled binary across three roles: `quick` (fast local native spot-checks), `sim` (deterministic Valgrind Callgrind merge gating), and `bench` (paired wall-clock and full hardware-counter validation). `swole` is the harness a benchmark is written with, a plain `#[bench]` function, `swole-macros` its attribute, and `cargo-swole` the runner, installed as `swole` and `cargo swole`. See [README.md](README.md), [crates/swole/README.md](crates/swole/README.md), and the [quest tree](quest/README.md).

# Required

- Everything is configless, with opinionated names and defaults: a benchmark is a plain function, and a consumer needs no config. Each config key a consumer needs is a gap to close.
- A measurement that cannot be trusted fails the run: a multiplexed counter, a benchmark that measured nothing, or an unpinned input in a mode that promises pinned inputs (`sim` and `quick`; native `bench` keeps the host clock and randomness unshimmed). One that can still move, such as a count other threads ran beside or native timing, is labeled so, never reported as exact.
- `sim` alone owns cross-host bit-exact repeatability: a `sim` count swole reports as exact repeats across runs, checkouts, and environments that share a fingerprint, and whatever can move it either enters the fingerprint or is pinned (Valgrind's version, the preload, and the launch's padding). `quick` optimizes for fast whole-workspace local feedback, and `bench` for real hardware behavior. Prove a change keeps `sim`'s repeatability with the tests, which count under the dev shell's Valgrind.
- The harness stays small, since it is in every consumer's `[dev-dependencies]`: what only the runner needs lives in `cargo-swole`.
- moq and rind are the first consumers of every change.

# Development

Run `just ci` before pushing; outside the dev shell, run `nix develop --command just ...`.

Quests: before any work, run `quest context` and follow it, or read https://github.com/kixel-ai/quest/blob/main/docs/context.md where `quest` is not installed, unless this session already holds its rules.
