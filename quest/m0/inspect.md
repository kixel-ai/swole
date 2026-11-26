# [M] Hot-loop source and disassembly inspection (swole inspect) and function code size

## Goal

`swole inspect <bench> [<base>]` runs a targeted benchmark under Callgrind with line and instruction granularity (`--dump-line=yes --dump-instr=yes`, plus cache/branch simulation) and prints the top hot source lines, annotated disassembly diff, and ELF `.text` code size of the hot functions, so developers and agents can immediately diagnose missed auto-vectorization, inlining changes, bounds checks, and instruction-cache bloat.

## Plan

Decided 2026-11-26 (maintainer, `/quest-plan`):

- ✅ Add `swole inspect` + ELF function `.text` size in m0 so active optimization loops (human and agent) can drill from a function-level regression straight into the hot loop's source lines and assembly in one command.

Recommendations for the implementer:

- Reuse the Callgrind parser from [per-function attribution](/quest/m0/callgrind-attribution.md), enabling `--dump-line=yes --dump-instr=yes` only in `swole inspect` (leaving bulk `swole sim` fast with `--dump-line=no`).
- Resolve symbol addresses and `.text` byte sizes from the compiled benchmark ELF binary (and disassemble the top 1 to 3 hot functions via `objdump` / `llvm-objdump` or an embedded decoder), aligning instruction-level costs (`Ir`, cache misses, branch mispredicts) beside source lines and assembly instructions.
- With `<base>`, diff the top hot function's `.text` size, instruction/cycle breakdown, and changed hot assembly blocks between base and head.
- Also record target/function `.text` code size in `sim` / `bench` `--report` where the symbol resolves cleanly, at zero runtime cost.
- Verify on a fixture with two revisions of a loop (e.g., adding a branch or breaking vectorization) confirming `swole inspect` names the changed source lines, assembly instructions, and `.text` size delta.

## Required

- [Per-function attribution from Callgrind dumps in every sim compare](/quest/m0/callgrind-attribution.md) - extends the Callgrind profile parser with line and instruction attribution

## Hosts

- linux - Valgrind Callgrind `--dump-line=yes --dump-instr=yes` and ELF inspection
