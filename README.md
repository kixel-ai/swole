# swole

Measures each benchmark in one of three modes over the same compiled benchmark targets, and
writes the measures as Kixel Quest's `metrics.json`, or compares a base commit with the working
tree:

- `swole sim`, what gates a merge, counts its instructions under stock Valgrind, one Valgrind process a job, in
  parallel across the host's cores, the same on any x86_64 Linux host that shares its
  [fingerprint](#fingerprints): hosted CI, a self-hosted runner, or an agent's own, with no
  third-party service in the loop. It replaces CodSpeed's runner over the benchmarks as they are
  written for it.
- `swole quick`, an agent's local spot-check, counts them [natively](#quick), once each, with
  the host's Linux perf counters, 20 to 50 times faster, comparable only on hosts like this one,
  and exact for a call far shorter than the timer's tick, where an interrupt is unlikely; `sim`
  is the mode that repeats exactly.
- `swole bench`, what gates a release, [times](#bench) them by wall clock on any Linux or macOS host, alone or paired
  with a base commit's, beside what each process used and every perf counter the host's
  [capabilities](#capabilities) allow.

Benchmarks are plain `#[bench]` functions written with [its harness](crates/swole/README.md),
or for `sim`, CodSpeed's compatibility crates.

Three crates:

- [`swole`](crates/swole/README.md), the harness a benchmark target puts in its `[dev-dependencies]`;
- `swole-macros`, its `#[bench]` attribute;
- `cargo-swole`, the runner, installed as both `swole` and `cargo swole`.

```bash
nix run github:kixel-ai/swole -- sim --out metrics.json   # or, without Nix:
cargo install --locked --git https://github.com/kixel-ai/swole cargo-swole   # Valgrind from PATH
swole sim [<base>] [--benchmarks <file> [--allow-missing]] [--out metrics.json] [--report <file>] [--jobs <n>] [-- <cargo bench args>]
swole quick [<base>] [--benchmarks <file> [--allow-missing]] [--out metrics.json] [--report <file>] [--jobs <n>] [-- <cargo bench args>]
swole bench [<base>] [--benchmarks <file>] [--out metrics.json] [--report <file>] [-- <cargo bench args>]
swole fingerprint sim|quick|bench
```

With `<base>`, a commit, each mode measures `<base>` as well as the working tree and prints
each change, as [Compare](#compare) says; `--out` is then `bench`'s alone, and `--allow-missing`
none's.

`cargo swole <command>` is the same as `swole <command>`.

## What it counts

`swole sim` counts, in a Cargo workspace, its benchmark targets, written with [swole](crates/swole/README.md)
or with CodSpeed's compatibility crates (`codspeed-criterion-compat` 6); `quick` and `bench`
measure only swole's. It builds them once
with `--cfg codspeed`, which CodSpeed's crates take, as `cargo codspeed build` does, passing any arguments after `--` to Cargo, such as `-p moq-net --bench
varint`. Cargo takes rustflags from one source and ignores the rest, so the cfg
joins `RUSTFLAGS` or `CARGO_ENCODED_RUSTFLAGS` when the environment sets them, and is
otherwise the only flags. A Cargo config that sets its own, `build.rustflags` or a
`target.*.rustflags` (or the unstable `host` config's) in a config file or one it includes,
the environment, or a `--config`,
fails the run before anything builds: set `RUSTFLAGS` to the flags the build needs instead.
Each target lists its benchmarks by running once natively, then each benchmark runs alone in
its own Valgrind process, through the harness's `SWOLE_BENCH` or CodSpeed's
`CODSPEED_ISOLATED_BENCHMARK`. A target that runs no benchmark through either fails the run,
as does one that runs some through both: leave it out with `--bench`.
It builds them with `cargo bench`, with itself as the targets' runner, which records how Cargo
launches each target, its directory and whole environment (`[env]`, `CARGO_MANIFEST_DIR`,
the library path), instead of running it; every listing and count launches the target the
same way. Those records live in a directory only the user can read, removed once read. A
runner a config or the environment sets, for any target, fails the run before anything
builds, as does, once built, an `[env]` that sets `GLIBC_TUNABLES`, which the fingerprint
reads from the environment, and an argument for the targets after Cargo's own `--`, which
neither harness takes: `--benchmarks` chooses benchmarks. Every count, like the
fingerprint, runs the Valgrind on swole's own `PATH`, whatever `PATH` a target gets.

Elsewhere, `sim` counts the programs `--program` names, each a shell command whose program brackets its
benchmarks itself, with swole's harness or CodSpeed's
[instrument hooks](https://github.com/CodSpeedHQ/instrument-hooks), after `--build`'s command
builds them. Each program is one Valgrind process, which
follows each `exec`, so a command may go through a shell or `just`.
[`just ffi count`](https://github.com/kixel-ai/rust2/blob/main/ffi/justfile) counts rust2's [binding benchmark](https://github.com/kixel-ai/rust2/blob/main/bench/ffi/README.md)
this way.

Each benchmark is the Callgrind dump its hooks name after it, between `ZERO_STATS` and
`START_INSTRUMENTATION` before it and `STOP_INSTRUMENTATION` and `DUMP_STATS_AT` after it,
with Valgrind's `--instr-atstart=no`, so nothing outside a benchmark counts. Its count is the
dump's `totals` in `Ir`, instructions executed, on every thread. The run fails on a dump with
an unknown trigger, a benchmark without `totals` or with zero instructions, two benchmarks of
one name, a job that fails, and a name `--benchmarks` lists that no benchmark has, so a
bisect step never reads a suspect it could not count as clean. With `--allow-missing`, `sim` or `quick`
instead names each such name on stderr, and in the job summary under GitHub Actions, counts
the rest, and fails only when none is there: a bisect step at a commit before a suspect was
added, or after a rename, keeps its other suspects' counts, and Kixel Quest reads one
`metrics.json` lacks as unmeasured. rust2's [Valgrind workflow](https://github.com/kixel-ai/rust2/blob/main/.github/workflows/valgrind.yml)
passes it with the benchmarks a bisect dispatches it with. A compare fails on such a
name only when neither side has it, since a change may add or remove a benchmark. A program's benchmarks all
run even when `--benchmarks` names only some; a Cargo benchmark runs only when named.
A dump named `Metadata: ...` is the hooks' own description of their integration, never a
benchmark, so no benchmark may take a name with that prefix: it would go uncounted.

A harness benchmark beside which the process had another thread, one there when its call
started or ended, or one its call started, which the [preload](#randomness-and-the-clock) sees
even when it is gone by the call's end, counts what that thread ran meanwhile, which the
scheduler decides. So `sim` counts each such benchmark's job five times in all, and records the
median of the five, with their least and most beside it in the table and as `lower_value` and
`upper_value` in `metrics.json`. So does every benchmark whose threads swole cannot see: those of
CodSpeed's crates and of a `--program`, whose harness runs every benchmark in one process, and of
a harness too old to say. A harness benchmark that ran alone counts once: under the preload's
randomness and clock it repeats exactly, but for an input the host decides, such as the order a
directory lists its entries in, which no repeat on one host would show.

The instrument hooks' `ADD_DESC` after each benchmark, a request only CodSpeed's Valgrind
fork knows, leaves an `unknown callgrind client request` warning in the job's Valgrind log,
under `target/swole/jobs-*`, beside its profiles and output. Each run takes a directory of
its own there, so runs in one tree never read each other's profiles, and removes it when it
succeeds; a failed run keeps it and names it.

## Randomness and the clock

A count follows everything before it in the process, down to the allocator's state, so a seed
or a clock read that differs from run to run moves it. So every process a job runs preloads a
library, [preload.rs](crates/cargo-swole/preload.rs), built with swole and kept under
`~/.cache/swole` (or `$XDG_CACHE_HOME`), which replaces glibc's:

- randomness (`getrandom`, `getentropy`, and `syscall(SYS_getrandom)`) with one fixed-seed
  stream, the same in every process, so std's `HashMap` seeds and the `getrandom` crate's
  bytes repeat;
- every clock (`clock_gettime`, `gettimeofday`, `time`) with one that starts at the same
  instant in every process, a day of uptime and 2026-01-01 on the wall, and advances 1 µs a
  read;
- `pthread_create` with one that also counts the threads the process started, which the
  harness reads around each call;
- and timed waits (`epoll_wait`, `poll`, `nanosleep`, `clock_nanosleep`, a timed condition
  variable, futex, semaphore, mutex or rwlock lock, thread join, or message queue call) with
  ones that still wait as long as they ask, on the host's clock, and move this clock by what
  they waited when they expire. A timer armed at an absolute time (`timerfd_settime` or
  `timer_settime` with its `ABSTIME` flag) aborts the process, naming the call in its Valgrind
  log, since the kernel would read the deadline on the host's clock.

Each process logs that it loaded the library to its Valgrind log, and a process that measured
a benchmark without it fails the run: a static executable never loads it, nor does one whose
environment drops `LD_PRELOAD`. A process that reaches the kernel without glibc, as Go's
runtime does, reads the host's randomness and clock for what it does so. Load still decides
whether a real timeout or I/O comes first, so a benchmark that races them can still move.

## Paths and the environment

The same binary counts differently when its process starts with a longer path or a larger
environment, since both sit on the initial stack and in the heap, and move the addresses, and
so the copy loop glibc picks, after them. So every job's arguments and environment are padded
to 1 MiB with `SWOLE_PAD_00` to `_15`, which puts its initial stack at the same place
however long its checkout's path or however large the host's environment, such as CI's
`GITHUB_REF` on another branch. A job whose own take more fails the run, as does a stack limit
under 4.25 MiB (`ulimit -s 4352`), since Linux takes a quarter of it for them. A `--program`
job pads its shell's launch: the program the shell runs keeps that environment but gets its
own arguments, so a command that expands a checkout's path, such as `"$PWD/target/probe"`,
still moves with that path's length. And CodSpeed's crates get their workspace relative to its
Git repository, from which they name each benchmark's file, so they never resolve the
checkout's path; a benchmark package at the repository's root still does, as does every
benchmark outside a repository, which CodSpeed names by its absolute path, so their counts
still move with that path's length. A path a build compiles in, such as a panic's location in a
dependency outside the workspace, is the build's to keep fixed across checkouts; a compare
counts both its sides from one directory, so it never moves between them.

## Fingerprints

Counts compare only within one environment. Valgrind presents a CPU derived from the host's,
and a binary that picks a path by CPU feature, such as glibc's string functions or Rust's
runtime detection, counts differently on another; so can another Valgrind, and glibc's
tunables, which mask features under the same CPU; so can another preload, whose code runs
inside a count and whose seed and clock steer it. So each run fingerprints its environment:
the architecture, every CPUID leaf a probe reads under the same Valgrind, `GLIBC_TUNABLES` as
the benchmarks inherit it, `valgrind --version`, and the preload's bytes, hashed into a label such as
`x86_64-3059cd1d9e65`. The measure every count records names it, `instructions
(x86_64-3059cd1d9e65)`, so counts from two environments never share a Kixel Quest series, and
a hardware change starts a new series instead of reading as a regression. `swole
fingerprint sim` prints the label, then its inputs on stderr.

A native measure, `quick`'s count or `bench`'s time, follows the host's own CPU, and the
counters it reads follow what the host lets swole count. So `quick` and `bench` fingerprint the
architecture, the CPU's model as `/proc/cpuinfo` or macOS's `machdep.cpu.brand_string` names
it, how many CPUs the run may use, and everything the [capability probe](#capabilities) finds:
`kernel.perf_event_paranoid`, whether swole holds `CAP_PERFMON`, the PMUs the kernel lists, the
core PMU and its counters, and the passes the probe packed them into; on Linux, the loaded libc
and `GLIBC_TUNABLES`, which pick glibc's string and memory routines; and for `quick`, the preload. `swole fingerprint quick` and `swole fingerprint bench` print each label, then its
inputs on stderr.

Valgrind 3.27.1 on x86_64 presents one fixed Intel CPU, not the host's: an AMD Ryzen 9 9950X
reads `GenuineIntel` family 6 model 0x3c, Haswell, through it. So hosts that support the
same features share a fingerprint.

The flake pins Valgrind: a nixpkgs bump that moves it fails evaluation until `flake.nix`'s
version moves too, since a new Valgrind can count differently and starts new series.

## Compare

`swole sim <base>` and `swole quick <base>` count `<base>` and the working tree in one worktree kept under
`~/.cache/swole` (or `$XDG_CACHE_HOME`), each from the directory there that matches where
it runs, and print a Markdown table of each benchmark's base, head, and change, then, for `quick`, one
of every other measure. `swole bench <base>` builds both sides in the same worktree and times
them [paired](#bench). One directory
counts both sides, so a path the build compiles in, such as a panic's location, is the same in
each and moves neither count. The worktree sits outside the repository, so no
`.cargo/config.toml` but each side's own configures its build, and is kept so its build stays
warm for the next side and the next compare: the head rebuilds only what differs from the base.
It first snapshots the working tree as a commit on `HEAD`, through an index of its own, so the
caller's index and stash stay as they were: every tracked file as it is on disk, staged or not,
and every untracked one Git does not ignore. An ignored file the caller has, such as a local
`.env`, is on neither side. An ignored one a build leaves in the worktree stays for the next
side, but for a `Cargo.lock` the side's commit does not track, which is removed before each side
builds, so each resolves its own dependencies. A submodule with uncommitted changes fails the run, since the snapshot holds
only its commit. A clean working tree counts `HEAD` itself. It refuses two fingerprints. A side
with no benchmark at all, as a base before the first, counts as none. An
absolute `--manifest-path` in the repository counts the same manifest in the base's worktree,
and one outside it is refused, since both sides would count it.
A change may add or remove a package or benchmark target, so each side builds only the `-p`
(`--package`) and `--bench` selectors its own revision has, from its `cargo metadata` after its
checkout, leaving out each `--features package/feature` whose package is neither one it builds nor one
of their dependencies, and prints each it left out; a `--bench` is looked for among the packages
Cargo builds, every member `--workspace` does not `--exclude`, which overrides `-p`, else those
`-p` selects, else the default ones. A side left with none of the packages, or under
`--workspace` none `--exclude` leaves, or none of the targets, unless beside `--benches` or
`--all-targets`, or without the directory compare runs from or the manifest `--manifest-path`
names, counts none, and its benchmarks read as added or removed. A selector neither side has
fails, as does one compare cannot resolve by name alone, such as `name@1.0` or a glob, and a
`--features package/feature` under Cargo's resolver 1, which also applies it to the current
package.
Another compare of the same repository waits, on a lock beside the worktree, until this one
has counted both sides.

## Quick

`swole quick`, an agent's local spot-check, counts each benchmark of
[swole's harness](crates/swole/README.md) natively,
without Valgrind's 20 to 50 times slowdown, with Linux perf counters: its user-space
instructions (`instructions:u`), the headline, and its cycles, branches, branch mispredicts, and
L1 data and last-level cache misses, each where the PMU counts it, beside what the process used
over the call, as [bench](#bench) reports it. A target of CodSpeed's crates, or a `--program`,
fails the run, as does a host that lets no process count its own instructions, such as a VM
without its PMU passed through, a `kernel.perf_event_paranoid` above 2 without `CAP_PERFMON`, or
GitHub's hosted runners: swole probes the counters before it builds anything, and names why,
pointing at `sim` and `bench`. `--out` writes the instructions as
`instructions:u (x86_64-...)` under the native [fingerprint](#fingerprints), so they share a
series only with hosts of the same CPU and counters, never Valgrind's.

Each job runs as a count does, with the preload, which the harness fails a process for not
having loaded, and a padded launch, but natively, without address randomization, which the copy
loop glibc picks by alignment would follow, and pinned to the CPUs of one core PMU. On a host
whose cores are of two kinds, such as Intel's P- and E-cores, each kind has its own PMU, which
counts only on its own cores and whose cores report their own cache sizes, from which glibc
sizes its copies; so every job runs on the kind with the most cores, and `--jobs` defaults to
how many there are. A job takes one process for each pass of counters the probe packed.

swole opens each job's counters itself, in the job's process just before it `exec`s the target,
each counting the thread that runs the benchmark. The harness starts them after the case's setup
and then reads each with `rdpmc`, in user space, through the page the kernel maps for it, and
reads them again right after the call, before stopping them, so the system calls that start and
stop counting stay out of a count, which holds the call and the reads' own few dozen instructions,
the same for every call. A counter that did not run the whole time it was enabled fails the run
rather than be scaled up: another user of the host's counters multiplexed it, or the thread left
the PMU's cores. The kernel maps no page for a counter a process's threads inherit, so a benchmark
beside which the process had another thread fails `quick`, naming it, since `sim` counts every
thread, and keeps only its time in `bench`. Run under Valgrind, the same benchmark counts within those few dozen instructions.
So `quick` is exact for a call far shorter than the timer's tick, where an interrupt is
unlikely, and `sim` is the mode that repeats exactly: each hardware interrupt that lands during a
call, the tick's or a device's, adds an instruction and a branch to its count, as x86's counters
count the return to user space, so a call's count moves with how many interrupts it took, which
a longer call takes more often. On one host, an Intel Core Ultra 7 270K Plus at `CONFIG_HZ=1000`, a 10 ms
loop counted 2 to 7 over its 40,000,092 across 15 runs.

## Capabilities

`quick` and `bench` probe once, before anything builds, what the host lets swole count, and
print it: `kernel.perf_event_paranoid`, whether swole holds `CAP_PERFMON` (or `CAP_SYS_ADMIN`),
the PMUs under `/sys/bus/event_source/devices`, the core PMU, how many general-purpose counters
it schedules at once, and the counters it found, in passes. The probe opens each counter as a
job will, on a thread pinned to the PMU's CPUs, with swole's own privileges, so what it finds is
what each job gets; the native [fingerprint](#fingerprints) holds all of it.

- Tier L2, `perf_event_paranoid` at most 2: user-space instructions, cycles and instructions a
  cycle, branches and mispredicts, L1 data and instruction, last-level, and data and instruction
  TLB misses, the top-down breakdown's first level where the PMU exports it (retiring, bad
  speculation, frontend and backend bound) where it counts them as events, rather than as Intel's
  top-down metrics, which a read in user space gets as fractions, frontend and backend stall
  cycles where the PMU counts them, and the kernel's `page-faults` taken in user space. The
  thread's time on a CPU, `task-clock`, is left out: the reads would count in it.
- Tier L1, `perf_event_paranoid` at most 1 or `CAP_PERFMON`: each hardware counter in the kernel
  as well (`:k` beside `:u`), so a syscall's or `io_uring`'s cost shows apart from user space's,
  `page-faults` in both, and `context-switches` and `cpu-migrations`, which the scheduler counts
  in the kernel.
  A count holds none of the kernel's work to start and stop counting, only what the call made
  it do, and the interrupts that landed meanwhile.
  Counting starts around each call, or each sample of a benchmark that times itself, whose
  count holds the reads' few dozen user-space instructions once a sample rather than once a
  call.
  `quick` counts user space alone at any tier.
- No tier, above 2 without `CAP_PERFMON`, or a host without a PMU, or one that does not let a
  process read its counters in user space (`cap_user_rdpmc` off), or a CPU other than x86_64, or
  macOS: `bench` times by wall clock with rusage alone, saying why, and `quick` fails.

A PMU holds only so many counters at once, and the kernel shares more among them by turns,
scaling each up by a guess. swole never lets it: the probe packs the counters into groups that
each fit the PMU, by opening them together and checking every one counted the whole time, and
a job runs one pass for each group, so no counter shares the PMU with another of the run's. A
counter some other user of the PMU still crowded out fails the run.

### Privileges

On a host whose `perf_event_paranoid` stays at its default, grant `CAP_PERFMON` to swole alone,
so its counters reach tier L1 while every benchmark target runs as an ordinary program: swole
opens each job's counters before the target `exec`s, which drops the capability, and clears
the ambient capabilities a wrapper may have raised, so no target inherits it. Anyone who can
run the granted binary can count what swole launches.

```bash
sudo setcap cap_perfmon=ep "$(command -v swole)"   # a cargo-installed binary
```

On NixOS, a wrapper holds the capability, since the store is read-only:

```nix
security.wrappers.swole = {
  source = "${swole}/bin/swole";   # swole = github:kixel-ai/swole's packages.<system>.default
  capabilities = "cap_perfmon+ep";
  owner = "root";
  group = "root";
};
```

`cargo swole` runs `cargo-swole` from `PATH`, which holds no capability: run `swole` instead.
A `swole` given the capability itself runs its helpers through the `cargo-swole` installed beside
it, since Valgrind refuses an executable that holds capabilities.

glibc starts a program that holds a capability in its secure mode, which drops `LD_LIBRARY_PATH`,
`GLIBC_TUNABLES`, and the loader's other variables from its environment, so a granted swole and
everything it runs never see them. A count's fingerprint reads the environment swole sees, so it
records the tunables as unset.

## Bench

`swole bench` builds the workspace's benchmark targets as a count does, then times each
benchmark of swole's harness by wall clock, one at a time, each in its own process, which
sets it up once and then runs a sample for each request: as many calls as fill a millisecond of
its timed part, found by doubling from one, or of 50 ms of the whole sample, when steps or
inputs built between calls take far longer. A benchmark takes samples for a second, and ten at
least, after a first call that waits out its setup, and its time is their median per call, which the samples a preemption
spoiled cannot move. It has no interval: load that slows many samples in a row makes them no
independent evidence of it, so one would look far tighter than a repeat run bears out. A target
written with CodSpeed's crates fails the run: only a count measures those. The run's processes are neither preloaded nor padded, so their randomness,
clock, and addresses are the host's own.

`swole bench <base>` builds `<base>` in the worktree a compare keeps, then the working tree,
and times each benchmark both have in two processes, the base's and the head's, alive together.
It takes their samples in pairs, one of each side, in an order drawn anew for each pair, so load
that drifts or recurs on a shared host biases neither side, and load common to both samples of a
pair cancels in its ratio. Load that slows one side more than the other, such as a neighbor
contending for memory against a memory-bound side, still widens the change's interval, which
pairing cannot remove, only balance; the interval takes the ratios as independent, so a run
that such load spans for long holds less than it says. Two processes of one build still differ by a few tenths of
a percent, by where each lands in memory and on the CPU, which no interval holds: a change that
small is one a pair of processes cannot tell. Each side's time is its own samples' median, and the change
is the median of each pair's ratio, head over base, with a 95% confidence interval from the
order statistics around it, whatever the ratios' distribution, printed as a Markdown
table. A benchmark only one side has times alone, as added or removed, and a base's benchmarks
of CodSpeed's crates time as none, as does a base's target of CodSpeed's crates whose every
benchmark CodSpeed skips, such as one of only `iter_custom`'s, so a base from before a rewrite
to the harness still has its head's times reported. A base builds into its worktree's own
`target/`, in place of any target directory the run names. Each side builds only the Cargo
selectors its own revision has, as [compare](#compare) selects them, the working tree's from its
files whether committed or not, so a `-p` or `--bench` naming a new or removed package or target
times its benchmarks as added or removed, and a side left with none of them times none. A
selector neither side has fails before either side builds.
The base's worktree stays locked until the last benchmark is timed.

Times compare only on one kind of host, so the measure every time records names the native
[fingerprint](#fingerprints), `time (x86_64-1a2b3c4d5e6f)`, in `ns` a call, so times from two
kinds of CPU never share a Kixel Quest series. Instruction counts need no pairing, since load
never moves them: timing is for what counts miss, such as syscalls, I/O, and contention.

Beside each time, `bench` reports what the process used over its samples, from `getrusage` on
Linux and macOS: user and system CPU time, minor and major page faults, and voluntary and
involuntary context switches, each a call's share, and the process's peak resident memory.
Each covers a sample's whole, steps and inputs built between calls included, which the clock
leaves out. Where the host has perf counters, `bench` pins both the timing and counting processes
to the probed core PMU's CPUs so times and counters measure the same cores on hybrid hosts, and
counts each side's benchmark with every counter the [probe](#capabilities) found, one process for
each pass, each calling the benchmark as many times as a sample did and counting a deterministic
stratified subset of up to 64 of those calls (the same calls in every pass and on both sides), a
counter's share of a call. Those counter processes run under the preload built to count the threads
a process starts and nothing else (without 1 MiB stack padding), so their clock and randomness stay
the host's, as the timing's do; its count (and the timing process's thread check) tells whether
another thread ran beside a call: such a benchmark keeps its time but has no counters, which read
the calling thread alone, and a note under the table names it. A table after the times
prints every measure of each benchmark, beside the base's and its change when paired, and
`--report` writes them all. A side whose harness predates counters, as a base from before an
upgrade, says so and has none; one whose harness predates rusage has none of that.

## Output

`--out` writes one measure of each benchmark, its headline, so a run of a large workspace stays
well within the 1 MiB Kixel Quest reads. `sim --out` and `quick --out` write one sample of the
run's commit, `quick`'s measure `instructions:u (...)`, and a benchmark counted several times,
its median with the least and most of its samples:

```json
{"samples":[{"metrics":{"benches/varint.rs::varint::encode[1]":{"instructions (x86_64-3059cd1d9e65)":{"value":12345}},
                        "benches/pool.rs::spawn":{"instructions (x86_64-3059cd1d9e65)":{"value":402614,"lower_value":402137,"upper_value":403005}}}}],
 "units":{"instructions (x86_64-3059cd1d9e65)":"instructions"}}
```

`bench --out` writes the working tree's times as the run's own sample, and with a base, the
base's as a sample naming its commit:

```json
{"samples":[{"commit":"<the base's full commit ID>","metrics":{"benches/varint.rs::encode[300]":{"time (x86_64-1a2b3c4d5e6f)":{"value":20.3}}}},
            {"metrics":{"benches/varint.rs::encode[300]":{"time (x86_64-1a2b3c4d5e6f)":{"value":19.2}}}}],
 "units":{"time (x86_64-1a2b3c4d5e6f)":"ns"}}
```

`--report <file>` writes every measure of every benchmark, by side, as JSON: the mode, the
fingerprint and its inputs, the base's commit when there is one, and for each benchmark, each
side's measures by name, each a `value` and a `unit` (a call's share, for `bench`), whether
another thread ran beside it, and for a paired `bench`, the change and its interval:

```json
{"mode":"bench","fingerprint":"x86_64-1a2b3c4d5e6f","host":["arch: x86_64","..."],"base":"<the base's full commit ID>",
 "benchmarks":{"benches/varint.rs::encode[300]":{
   "base":{"time":{"value":20.3,"unit":"ns"},"instructions:u":{"value":81.0,"unit":"events"},"user time":{"value":20.1,"unit":"ns"}},
   "head":{"time":{"value":19.2,"unit":"ns"},"instructions:u":{"value":77.0,"unit":"events"},"user time":{"value":19.0,"unit":"ns"}},
   "change":{"value":0.9458,"lower":0.9412,"upper":0.9501,"unit":"head/base"}}}}
```

A benchmark's name is the one its hooks dump, which for CodSpeed's crates is the file
relative to the repository, then its group, function, and parameter: the same name CodSpeed
gave it, so only the measure differs. The harness names its own the same way, by
[file, module, function, and argument](crates/swole/README.md#names).
