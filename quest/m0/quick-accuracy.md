# [M] Make quick's counts exact for calls longer than the timer's tick

## Goal

`swole quick`, an agent's local spot-check, gives the same `instructions:u` count on every run for a benchmark of any length on one host, not only for calls far shorter than the timer's tick, where an interrupt is unlikely. Today each hardware interrupt during a call adds one instruction and one branch to the count, as x86's counters count the return to user space. On one host, an Intel Core Ultra 7 270K Plus at `CONFIG_HZ=1000`, a 10 ms loop counted 2 to 7 over its 40,000,092 across 15 runs ([kixel-ai/swole#5](https://github.com/kixel-ai/swole/pull/5)). `sim` stays what gates a merge and `bench` what gates a release; this quest only sharpens `quick`.

## Plan

Decided 2026-11-26 (maintainer, on [kixel-ai/swole#5](https://github.com/kixel-ai/swole/pull/5)): keep `quick` as the spot-check, documented as exact for calls far shorter than the tick, where an interrupt is unlikely, with `sim` the mode that repeats exactly, and make it more accurate here.

Ideas, in the order to try them:

- Subtract counted interrupts (recommended first): open an event counting the interrupts the calling thread takes, such as Intel's `HW_INTERRUPTS.RECEIVED` or AMD's `ls_int_taken`, in the same group as `instructions:u`, and subtract it from `instructions:u` and `branches:u`.
  - Prove the overcount is exactly one per interrupt on each PMU first, with the spin loop above, over thousands of runs under load. Some models also overcount on a page fault, so check those too.
  - The probe picks the event per PMU. A PMU without one has no exact long count; decide its path at setup, never at runtime.
- Take the least of repeated counts (recommended fallback, and a differential check of the first idea): interrupts only ever add, so counting a long call several times and keeping the least converges on the exact count once one run sees no interrupt. It costs a run per repeat. It suits a PMU without an interrupt event, and checks the subtraction where there is one.
- Flag a count that took an interrupt rather than correct it, marked `~` like a sampled count: cheap, but it leaves the spot-check inexact. Take it only if neither idea above holds up.
- Make retired conditional branches the headline, as rr does, since interrupts do not move them (rejected unless the others fail): they are not comparable with `sim`'s instructions, though they may serve as a second exact measure beside them.

As a spot-check, also measure an agent's loop on a real consumer once one runs swole's harness: edit, build, then `quick <base> --benchmarks <suspects>`. Keep only what shortens that loop.

## Hosts

- linux - perf counters
