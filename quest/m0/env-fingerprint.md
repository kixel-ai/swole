# [L] Fingerprint the environment a sim count reads

## Goal

A `sim` count's fingerprint covers every environment variable that can move it, without a consumer listing any: the variables the counted process reads at run time, and those a build script told Cargo it depends on. Two runs that differ in such a variable land in different series instead of silently shifting one, as AGENTS.md requires of anything that can move a `sim` count. Variables a benchmark never reads stay out, so series still match across hosts whose `PATH`, `HOME`, or user differ.

## Plan

Decided 2026-11-28 (maintainer, `/quest-run`): ✅ plan this in m0 (rejected: dropping it). Found in [kixel-ai/rind#1133](https://github.com/kixel-ai/rind/pull/1133), which made rind's counts repeat by setting `OPENSSL_ia32cap` (read by aws-lc at run time) and `AWS_LC_SYS_NO_JITTER_ENTROPY` (read by aws-lc-sys's build script) in its bench shell, and noted that swole's fingerprint covers neither. Today swole passes the caller's whole environment to every counted process (`environment` in `crates/cargo-swole/src/lib.rs`), and only `GLIBC_TUNABLES` enters the fingerprint.

Recommendations for the implementer:

- Run time: have the preload record each variable the counted process reads, by name and value, through `getenv` and `secure_getenv` (Rust's `std::env::var` calls `getenv` on Unix). Code that walks `environ` directly escapes this; say so in the README rather than guess at it. A variable read only by swole's own preload or harness stays out. Since measured calls may not read the environment (next item), every recorded read happens outside the count, so copy the name and value into recorder-owned storage at the read: a caller may free its name buffer or rewrite a `putenv` value afterwards. Hash them once the process exits. A recorder change moves the preload's hash, which starts a new series.
- Lookup cost: glibc's `getenv` scans `environ` in order, so a lookup inside a measured call costs more when an unrelated variable sits before it, and the count moves under the same fingerprint. Fail an exact `sim` run whose measured calls read the environment, naming the variable, the way swole fails other unpinned inputs; reads in setup or before the first measured call cost nothing in the count. Check whether a lazy read, such as aws-lc's `OPENSSL_ia32cap` on first use, lands in a measured call, and if so let the harness's first, uncounted call absorb it rather than loosening the rule.
- Build time: hash, unset included, the variables the benchmark's own build reads: each build script's `rerun-if-env-changed` directives, in both the `cargo:` and `cargo::` spellings, and rustc's `# env-dep:` lines in the dep-info of every compilation unit linked into the benchmark, its dependencies included, which record `env!` and `option_env!` reads such as rind's `VALGRIND_BUG_494162`. Find the units and build scripts this build used from Cargo's JSON messages (`compiler-artifact`, `build-script-executed`), not by scanning `target/`, so a warm build doesn't pick up stale outputs.
- Scope: the environment a benchmark reads is that benchmark's, not the host's. Keep the host fingerprint as it is and carry each benchmark's environment hash beside it through `sim <base>` comparison, `--out` metrics, and `--report`, so a benchmark that starts reading a variable starts its own new series without splitting unaffected ones or failing the whole comparison. Build-time variables apply to every benchmark of that build target.
- Fingerprinting the whole environment would split every host into its own series (rejected); clearing it would break consumers that set a variable on purpose, as rind does (rejected). The reads above are what a benchmark depends on.
- Keep the probe cheap: the read set comes from the counted run itself, so no extra Valgrind run is needed.
- Verify with fixture benchmarks: one that branches on a variable read in setup gets a different environment hash per value while an unrelated variable such as `HOME` changes nothing and the other benchmarks' series stay put; one that reads a variable inside its measured call fails the run; a build script with `cargo::rerun-if-env-changed` and an `option_env!` read each change the hash when their variable does. Check rind's bench shell gets one stable fingerprint across hosts.

## Hosts

- linux - `sim` runs under Valgrind on Linux x86_64
