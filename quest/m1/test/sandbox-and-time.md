# [L] Hermetic Linux sandbox and strict deterministic time/RNG test preload

## Goal

On native Linux runs, `swole test` executes every test batch by default inside an unprivileged hermetic sandbox (`CLONE_NEWUSER | CLONE_NEWNET | CLONE_NEWIPC`, ephemeral `TMPDIR`, Landlock read-only workspace, scrubbed environment) with a separate embedded `test-preload.so` (built from `preload.rs` without changing benchmark `preload.so` bytes or fingerprints) that enforces deterministic SplitMix64 RNG, a deterministic virtual clock, immediate abort on `nanosleep`/`clock_nanosleep > 0` and `timerfd_settime`/`timer_settime`, and abort when `epoll_wait`/`epoll_pwait`/`poll`/`pthread_cond_timedwait`/`pthread_cond_clockwait`/`SYS_FUTEX` waits actually expire (`ready == 0` or `ETIMEDOUT`), plus `--time-warp` flake hunting, `--shuffle <seed>` batch ordering, and `--no-sandbox` / `--real-time` opt-out flags.

## Plan

Settled in [/quest/m1/test/README.md](/quest/m1/test/README.md): strict sandbox and time/RNG preload enabled by default on day one on native Linux (auto-disabled for foreign runners, `cross`, and non-Linux hosts), global CLI opt-outs (`--no-sandbox`, `--real-time`), and abort on `nanosleep`/`timerfd` immediately plus on `epoll`/`poll`/`cond`/`futex` timeout expiry so `#[tokio::test(start_paused = true)]` and fast loopback I/O wrapped in `tokio::time::timeout` pass cleanly while un-paused `tokio::time::sleep` and wall-clock timeout waits fail loudly.

Recommendations for the implementer:

- Add a separate `--cfg=test_shim` build target (`test-preload.so`) in `crates/cargo-swole/build.rs` from `preload.rs` so `preload.so`, `threads.so`, `heap.so`, and `heap-threads.so` remain byte-for-byte identical (add a unit test asserting `preload::hash()` did not move, or keep benchmark preload code paths gated).
- Build `test-preload.so` for both `x86_64-unknown-linux-gnu` and `aarch64-unknown-linux-gnu` (since `moq` CI runs on aarch64 Linux), gating the x86 Valgrind client-request inline asm out of `test_shim` and defining arch-specific `SYS_CLOCK_GETTIME`, `SYS_FUTEX`, and `SYS_GETRANDOM` constants.
- In `test-preload.so`:
  - Keep `preload.rs`'s deterministic SplitMix64 RNG (`getrandom`, `getentropy`, `SYS_GETRANDOM`) and virtual clock (`clock_gettime`, `gettimeofday`, `time`, `SYS_CLOCK_GETTIME`).
  - When `--real-time` is not set: write a clear stderr diagnostic naming the offending call and abort immediately on `nanosleep` / `clock_nanosleep` with a non-zero duration, `timerfd_settime` / `timer_settime` with a non-zero expiration, and whenever `epoll_wait` / `epoll_pwait` / `poll` returns `0` after a positive timeout or `pthread_cond_timedwait` / `pthread_cond_clockwait` / `SYS_FUTEX` (`FUTEX_WAIT*`) returns `ETIMEDOUT`. Have the runner surface that diagnostic beside the aborted test name.
  - Support `--time-warp` (seeded clock-step jitter + periodic `sched_yield` around locks/waits to expose concurrency races) and `--shuffle <seed>` (enable within-batch in-process shuffling via `libtest`'s `--shuffle --shuffle-seed` only on `libtest` builds that support it, and document that stable `libtest` builds shuffle only batch membership and dispatch order since stable `libtest` sorts `--exact` matches by name).
- Implement the Linux sandbox in the batch child pre-exec hook with zero `ptrace` overhead:
  - Enter unprivileged user, network, and IPC namespaces (`CLONE_NEWUSER | CLONE_NEWNET | CLONE_NEWIPC`) and bring up loopback `lo` inside the namespace so every batch gets a private `127.0.0.1` (zero port collisions across agents, immediate `ENETUNREACH` on accidental internet access). Fail loudly with an actionable error if the host kernel disables unprivileged user namespaces instead of silently running unsandboxed.
  - Give each batch a fresh ephemeral `TMPDIR`, scrub ambient host env vars beyond Cargo's captured launch env and allowlisted essentials, and apply Landlock rules granting read-only access to the workspace/system libraries and read-write access only to that batch's `TMPDIR`. Because tests that write to Cargo's compile-time `env!("CARGO_TARGET_TMPDIR")` (such as `swole`'s integration tests and `rind`'s OpenSSL tests) resolve paths outside the sandbox's runtime `TMPDIR`, migrate `swole`'s suites (and `rind`'s during adoption) to runtime sandbox scratch (`TMPDIR`) before enabling the default sandbox for them.
- Prove with fixture tests: two parallel batches binding the same loopback port succeed; outbound network fails immediately; writing outside `TMPDIR` fails with `EACCES`; `#[tokio::test(start_paused = true)]` and `tokio::time::timeout(5s, fast_loopback_io)` pass; `thread::sleep(1ms)` and un-paused `tokio::time::sleep(1ms)` abort with a clear diagnostic naming the test; `--real-time` and `--no-sandbox` allow both to pass; and `cargo test` verifies `swole`'s own Valgrind/perf benchmark tests pass cleanly with `--no-sandbox --real-time`.

## Hosts

- linux - user/network/IPC namespaces, Landlock, and Linux `test-preload.so`

## Required

- [Core swole test runner](/quest/m1/test/runner.md) - provides the sharded batch launcher that wraps each test batch process
