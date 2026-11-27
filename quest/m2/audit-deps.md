# [M] Audit dependencies

## Goal

Run the `deps` audit: `quest audit deps` prints what it checks, what each finding becomes, and how to plan its next run.

## Plan

- Baseline: 2026-11-26. Check releases published since then.
- Out of scope: `rust-toolchain.toml`, pinned to moq's build toolchain on purpose; it moves with moq.
- Check first:
  - Valgrind, which `flake.nix` pins and the fingerprint names: whether a release past 3.28 is out, and whether nixpkgs carries it.
  - taplo: its last release was 0.10.0 (2025-05-23) and it saw three commits in 2026. If it has gone quiet or nixpkgs drops it, replace it with tombi, which is active.
  - `cachix/install-nix-action`, which installs upstream Nix for CI as moq's does.

## Audit

- deps

## Scheduled

- 2026-12-03 - a week after the last run

## Required

- [m1](/quest/m1/README.md) - runs once per milestone
