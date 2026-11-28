# [M] macOS test preload for deterministic RNG, clock, and sleep abort

## Goal

Native macOS runs of `swole test` preload a separate embedded `test-interpose.dylib` built from `interpose.rs` (leaving benchmark `heap-threads.dylib` untouched) that provides the same deterministic SplitMix64 RNG, virtual clock, immediate abort on `nanosleep` > 0, abort on `kevent`/`poll`/`pthread_cond_timedwait` timeout expiry, and workspace file-read tracing (`open`/`openat`/`stat`) as Linux's `test-preload.so`, giving macOS developers and runners the same wall-clock flake detection and cross-host `mbx-cache` participation (respecting `--real-time` and `--no-cache`, and auto-disabling on foreign runners like `xcrun simctl spawn`).

## Plan

Settled in [/quest/m1/test/README.md](/quest/m1/test/README.md): keep `swole`'s benchmark interposer untouched while bringing the strict test-preload contract to macOS once Linux proves the design.

Recommendations for the implementer:

- Add a separate `--cfg=test_shim` build (`test-interpose.dylib`) in `crates/cargo-swole/build.rs` using Mach-O `__DATA,__interpose` (following `crates/cargo-swole/interpose.rs`), leaving `heap-threads.dylib` byte-for-byte unchanged.
- Interpose `getentropy`, `arc4random_buf`, `clock_gettime`, `gettimeofday`, `mach_absolute_time` (or `mach_continuous_time` where `std::time::Instant` reads it), `nanosleep`, `kevent` / `kevent64`, `poll`, `pthread_cond_timedwait`, and `open`/`openat`/`stat` for workspace input tracing.
- Note SIP restrictions on `DYLD_INSERT_LIBRARIES`: test binaries built by Cargo are unsigned/ad-hoc signed user binaries and accept `DYLD_INSERT_LIBRARIES`, whereas platform tools like `/usr/bin/xcrun` strip it; `swole test` already auto-disables the preload on foreign runners (`simctl`), so only native macOS test executables load it.
- Verify on macOS with the same fixture suite as Linux: `tokio::time::pause()` and fast loopback I/O pass, `thread::sleep` and un-paused `tokio::time::sleep` abort loudly naming the test, `--real-time` opts out cleanly, and traced fixture edits invalidate cached results.

## Hosts

- macos - builds and verifies the `DYLD_INSERT_LIBRARIES` test interposer

## Required

- [Hermetic sandbox and strict test preload](/quest/m1/test/sandbox-and-time.md) - defines the test-preload contract, CLI flags, and runner integration
