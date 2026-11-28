# [M] Fingerprint the environment a sim count reads

## Goal

A `sim` count's fingerprint covers every environment variable that can move it, without a consumer listing any: the variables the counted process reads at run time, and those a build script told Cargo it depends on. Two runs that differ in such a variable land in different series instead of silently shifting one, as AGENTS.md requires of anything that can move a `sim` count. Variables a benchmark never reads stay out, so series still match across hosts whose `PATH`, `HOME`, or user differ.

## Plan

Decided 2026-11-28 (maintainer, `/quest-run`): ✅ plan this in m0 (rejected: dropping it). Found in [kixel-ai/rind#1133](https://github.com/kixel-ai/rind/pull/1133), which made rind's counts repeat by setting `OPENSSL_ia32cap` (read by aws-lc at run time) and `AWS_LC_SYS_NO_JITTER_ENTROPY` (read by aws-lc-sys's build script) in its bench shell, and noted that swole's fingerprint covers neither. Today swole passes the caller's whole environment to every counted process (`environment` in `crates/cargo-swole/src/lib.rs`), and only `GLIBC_TUNABLES` enters the fingerprint.

Recommendations for the implementer:

- Run time: have the preload record each variable the counted process reads, by name and value, through `getenv` and `secure_getenv` (Rust's `std::env::var` calls `getenv` on Unix). Code that walks `environ` directly escapes this; say so in the README rather than guess at it. Hash the names and values read into that benchmark's fingerprint. A variable read only by swole's own preload or harness stays out. The recorder runs inside the count, as the preload's clock and randomness shims already do (`fingerprint.rs` documents the preload as code that runs inside each count), so keep its in-run work to a fixed few instructions per read, such as appending the name and the pointer `getenv` returned to a fixed table (failing the run when it fills), which preserves the value seen at that lookup across a later `setenv` or `unsetenv`, since glibc never frees an environment string it replaces; intercept `putenv` too, and fail the run, naming the variable once the process exits, when a measured call reads a variable `putenv` installed, since its caller owns that buffer and may rewrite it in place (Rust's `set_var` uses `setenv`, so only C code reaches this); hash those names and values once the process exits, outside any measured call; a recorder change moves the preload's hash, which starts a new series.
- Build time: read the `cargo:rerun-if-env-changed=NAME` lines from each build script's output under the build's `target/` directory, and hash those variables' values, unset included, into the fingerprint. Cargo already treats them as build inputs.
- Fingerprinting the whole environment would split every host into its own series (rejected); clearing it would break consumers that set a variable on purpose, as rind does (rejected). The two reads above are what the benchmark depends on.
- Keep the probe cheap: the read set comes from the counted run itself, so no extra Valgrind run is needed.
- Verify with a fixture benchmark that branches on an environment variable: two `sim` runs with different values get different fingerprints, an unrelated variable such as `HOME` changes nothing, and a build script with `rerun-if-env-changed` changes the fingerprint when its variable does. Check rind's bench shell gets one stable fingerprint across hosts.

## Hosts

- linux - `sim` runs under Valgrind on Linux x86_64
