# [M] Name where a benchmark's allocations come from

## Goal

Beside each benchmark's heap allocation counts, `swole` names the call sites or functions that make them (the top few by allocations and by bytes), so an allocation regression in a `<base>` compare arrives with where the extra allocations came from, in the CLI and in full in `--report`.

## Plan

Decided 2026-11-27 (maintainer, /quest-run): keep.

Split out of the allocation counters' quest, whose recommendations listed per-site attribution beside the three scalar counts; the counts landed without it.

Recommendations for the implementer:

- Attribute from `sim`'s Callgrind dumps, which already hold each caller's calls to `malloc`, `calloc`, `realloc`, and `posix_memalign` at no extra runtime, building on `callgrind.rs`'s per-function parser (merged; it reads each function's self cost by name and skips the call lines this needs); use `--separate-callers` only if the direct callers (`__rust_alloc`, `alloc::raw_vec::*`) hide the benchmark's own frames, and measure what it costs a count first.
- Avoid unwinding inside the allocation interposer: a backtrace taken in `malloc` can itself allocate or deadlock, and Rust's release builds keep no frame pointers.
- Diff each benchmark's top allocating functions between `<base>` and head, as Callgrind attribution diffs instructions.
