# [M] macOS test preload for deterministic RNG, clock, and sleep abort

## Goal

Native macOS runs of `swole test` preload a separate embedded `test-interpose.dylib` built from `interpose.rs` (leaving benchmark `heap-threads.dylib` untouched) that provides the same deterministic SplitMix64 RNG, virtual clock, immediate abort on `nanosleep` > 0, and abort on `kevent`/`poll`/`pthread_cond_timedwait` timeout expiry as Linux's `test-preload.so`, giving macOS developers and runners the same wall-clock flake detection (respecting `--real-time`, and auto-disabling on foreign runners like `xcrun simctl spawn`). Because macOS preload alone does not isolate host network or filesystem state, native macOS runs keep test-cache reads and writes disabled until equivalent sandbox isolation is available.

## Plan

Settled in [/quest/m1/test/README.md](/quest/m1/test/README.md): keep `swole`'s benchmark interposer untouched while bringing deterministic RNG, virtual clock, and sleep/timeout flake detection to macOS once Linux proves the design, and uphold the hermetic-only cache contract by keeping unsandboxed macOS runs out of `mbx-cache`.

Recommendations for the implementer:

- Add a separate `--cfg=test_shim` build (`test-interpose.dylib`) in `crates/cargo-swole/build.rs` using Mach-O `__DATA,__interpose` (following `crates/cargo-swole/interpose.rs`), leaving `heap-threads.dylib` byte-for-byte unchanged.
- Interpose `getentropy`, `arc4random_buf`, `clock_gettime`, `gettimeofday`, `mach_absolute_time` (or `mach_continuous_time` where `std::time::Instant` reads it), `nanosleep`, `kevent` / `kevent64`, `poll`, and `pthread_cond_timedwait`.
- Keep both test-cache reads and writes disabled on macOS (without failing the run) until an equivalent network and filesystem sandbox is added, so port or filesystem differences on macOS cannot pollute or replay stale passes in `mbx-cache`.
- Note SIP restrictions on `DYLD_INSERT_LIBRARIES`: test binaries built by Cargo are unsigned/ad-hoc signed user binaries and accept `DYLD_INSERT_LIBRARIES`, whereas platform tools like `/usr/bin/xcrun` strip it; `swole test` already auto-disables the preload on foreign runners (`simctl`), so only native macOS test executables load it.
- Verify on macOS with the same fixture suite as Linux: `tokio::time::pause()` and fast loopback I/O pass, `thread::sleep` and un-paused `tokio::time::sleep` abort loudly naming the test, `--real-time` opts out cleanly, and native macOS runs neither read from nor write to the test cache.

## Hosts

- macos - builds and verifies the `DYLD_INSERT_LIBRARIES` test interposer

## Required

- [Hermetic sandbox and strict test preload](/quest/m1/test/sandbox-and-time.md) - defines the test-preload contract, CLI flags, and runner integration
