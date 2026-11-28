# [L] Core swole test runner: sharded batches, clippy fusion, timeouts, and foreign runners

## Goal

`swole test` (`cargo swole test`) builds a Cargo workspace's test targets in one pass (with `RUSTC_WORKSPACE_WRAPPER=clippy-driver` and `CARGO_BUILD_WARNINGS=deny` on by default, `--no-clippy` and `--allow-warnings` to opt out), runs test binaries in parallel using sharded `--test-threads=1` batches bounded by `--timeout <dur>` (default `60s`), chains Cargo's `target.<triple>.runner` (`simctl`, `sde`, `qemu`, `wasmtime`, `adb`) and wraps `cross` with PID-1 signal forwarding, resumes remaining tests after a crash or hang, solo-retries batch failures to flag shared-state leaks, and requires zero config files or changes to `crates/swole`.

## Plan

Settled in [/quest/m1/test/README.md](/quest/m1/test/README.md): strictly configless CLI (ignore `.config/nextest.toml`, no `swole.toml`), sharded `--test-threads=1` batches across parallel binaries, fused `clippy-driver` + `CARGO_BUILD_WARNINGS=deny` by default, zero retries, immediate + final failure output, skipping `bench` targets by default, and full support for foreign target runners and `cross`.

Recommendations for the implementer:

- Reuse `cargo.rs`'s `__capture` runner mechanism to record each test binary's exact working directory, `[env]`, `CARGO_MANIFEST_DIR`, and library paths, extending it to read and chain any configured `target.<triple>.runner` or `CARGO_TARGET_*_RUNNER` instead of refusing it (while keeping `sim`/`quick`/`bench`'s refusal intact).
- Support `--runner <cmd>` and wrapping `cross` (passing `--init` or ensuring PID-1 signal forwarding so SIGTERM/SIGKILL reaches container children cleanly, as proven in [kixel-ai/rust2#1079](https://github.com/kixel-ai/rust2/pull/1079) at commit `690109f`).
- Discover tests via `binary --list --format terse` (1 launch per binary) and partition each binary into batches: 1 batch for small binaries, and up to `K` parallel batches for large binaries using `--test-threads=1 --exact <names...>` (falling back to module-prefix grouping if `argv` or `libtest`'s linear filter scan becomes measurable). Because `--test-threads=1` prints `test NAME ... ` immediately as each test starts, enforce sub-60 s `--timeout` bounds directly without waiting for `libtest`'s 60 s warning.
- Run each batch in its own process group. On timeout or abnormal exit (SIGABRT/SIGSEGV), report the exact test name, send SIGTERM then SIGKILL after a short grace period, and automatically launch a fresh process for the remaining tests in that batch so one crash or hang never drops later tests.
- When a test fails inside a multi-test batch, re-run that single test alone once solely for diagnosis: if it passes solo, still fail the run and label it a shared-state leak rather than hiding the failure as a retry.
- Cooperate with `mbx`'s host-wide scheduler permit pool when present (`mbx` sets `[scheduler] tests = true` / `mbx reserve`), and respect `--jobs <n>` and `--changed <base>` (sharing diff-scoped selection once [/quest/m0/diff-selection.md](/quest/m0/diff-selection.md) lands).
- Keep `crates/swole` untouched and benchmark-only, and verify benchmark `preload.so` hashes are unchanged.
- Verify on `swole`'s own workspace and a fixture workspace covering: a hanging test killed at `--timeout 2s` naming the exact test with zero leftover child processes, a crashing (`abort`) test whose remaining batch siblings still run, a state-leaking pair flagged via solo re-run, `--clippy` failing on a lint warning before test execution and `--allow-warnings` / `--no-clippy` relaxing it, and a mock target runner script verifying chaining and signal forwarding.

## Related

- [Diff-scoped benchmark selection](/quest/m0/diff-selection.md) - conservative `cargo metadata` affected-package selection that `swole test --changed <base>` can share
