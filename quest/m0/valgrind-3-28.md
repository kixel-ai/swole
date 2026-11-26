# [S] Count under Valgrind 3.28

## Goal

swole counts under Valgrind 3.28.0 (released 2026-10-23): `flake.nix` pins `3.28.0`, and the README's description of the CPU Valgrind presents matches what 3.28 reports.

## Plan

- The nix group's Dependabot PR fails evaluation once nixpkgs moves Valgrind, since `flake.nix` throws on any version but its pin; take this quest in that PR's place, or on that PR.
- 3.28 advertises LZCNT on amd64 through its CPUID emulation, so code that picks an implementation at runtime may count differently: the fingerprint's Valgrind version starts new series, as intended. Re-check the README's `GenuineIntel` family/model paragraph against a run under 3.28.
- Its new `VALGRIND_RUNNING_VERSION` client request adds nothing swole needs: the fingerprint already reads `valgrind --version`.

## Waiting

- NixOS/nixpkgs: [NixOS/nixpkgs#579391](https://github.com/NixOS/nixpkgs/pull/579391) (`valgrind: 3.27.1 -> 3.28.0`, against `staging`) merged and reached `nixpkgs-unstable`; check with `nix eval --raw github:NixOS/nixpkgs/nixpkgs-unstable#valgrind.version`.
