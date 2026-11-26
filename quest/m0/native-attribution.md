# [M] Native per-function attribution via AMD IBS and Intel PEBS in bench

## Goal

When the setup probe detects L-1 `perf` access (`perf_event_paranoid <= -1` or `CAP_PERFMON`), `swole bench` runs a dedicated hardware-sampling attribution pass (AMD IBS `ibs_op` or Intel PEBS / `cycles:pp`), resolves sampled instruction pointers (including inlined frames) against the benchmark ELF and loaded DSOs, and prints per-function native diffs in `swole bench <base>` just as Callgrind attribution does for `sim`.

## Plan

Decided 2026-11-26 (maintainer, `/quest-plan`, splitting the former `quest/m0/deep-perf.md`):

- ✅ Split native per-function attribution out of `deep-perf` into its own m0 quest so it lands independently of off-CPU contention and whole-socket uncore/RAPL leases (rejected: keeping all three bundled in one `[L]` quest).

Recommendations for the implementer:

- Probe for AMD `ibs_op` or Intel `cycles:pp` (PEBS) at setup and sample into a perf ring buffer during a dedicated attribution pass so sampling overhead never disturbs timed samples or exact counter passes.
- Resolve instruction pointers against `/proc/<pid>/maps` (`PERF_RECORD_MMAP2`) and ELF/DWARF symbol tables (using `blazesym` or `gimli`/`object` + `rustc-demangle`), preserving inlined frames so monomorphized Rust hot loops attribute to readable functions.
- Format concise top-contributor diffs (3 to 5 functions explaining the net delta) in `swole bench <base>` Markdown output and write the full per-function breakdown to `--report`.
- Verify on a real Linux host (`fedora` / `nixos`, AMD Zen 5 or Intel Core Ultra): adding extra work to one helper function in a compare fixture surfaces that exact function at the top of `swole bench <base>`'s native attribution diff.

## Hosts

- linux - AMD IBS / Intel PEBS sampling and perf ring buffer
