# swole

Benchmarks as plain functions marked `#[bench]`, which [swole](../../README.md) counts in
instructions under Valgrind or with the host's perf counters, each alone in its own process, in
parallel, and times by wall clock, alone or paired with a base commit's, for Kixel Quest's `metrics.json` or a local compare. A
benchmark written for it needs neither Criterion nor CodSpeed's crates.

```toml
[dev-dependencies]
swole = { git = "https://github.com/kixel-ai/swole" }

[[bench]]
name = "varint"
harness = false
```

```rust
use std::time::Duration;

use swole::{Iterations, bench};

#[bench]
fn empty() -> Vec<u8> {
	Vec::new()
}

#[bench(args = [1, 300, 70000])]
fn encode(n: u64) -> Vec<u8> {
	varint::encode(n)
}

fn frames(size: usize) -> Vec<Frame> {
	(0..100).map(|_| Frame::zeroed(size)).collect()
}

#[bench(args = [100, 16384], setup = frames, bytes = |size| 100 * size)]
fn write(frames: Vec<Frame>) {
	for frame in frames {
		sink(frame);
	}
}

fn relay() -> Relay {
	Relay::with_paths(10_000)
}

#[bench(setup = relay, before = Relay::touch)]
fn tick(relay: &mut Relay) -> Bytes {
	relay.tick()
}

#[bench]
fn busy(iterations: Iterations) -> Duration {
	let mut pair = Pair::connected();
	let start = pair.server.busy();
	for _ in iterations {
		pair.exchange();
	}
	pair.server.busy() - start
}

fn main() {
	swole::main();
}
```

```bash
cargo bench                                    # each benchmark once, natively
nix run github:kixel-ai/swole -- sim           # each one counted under Valgrind
nix run github:kixel-ai/swole -- sim main      # each one's change from main
nix run github:kixel-ai/swole -- quick         # each one counted with this host's perf counters
nix run github:kixel-ai/swole -- bench         # each one timed, with every counter
nix run github:kixel-ai/swole -- bench main    # each one timed beside main's
```

## What a benchmark is

A function, its body the only thing counted or timed. A count calls it once; a time calls it
many times, in samples of as many calls as fill a millisecond. It takes one input at most:

- With neither key, none.
- `args = <expr>`, an expression a `for` loop takes, such as an array or a `const`, declares
  one case an argument, named `[<arg>]` by the argument's `Display`. Without `setup` the
  function takes the argument itself, moved in: each call a clone of it, so it must be
  `Clone`. The expression is evaluated once, so an iterable of unstable order, such as a
  `HashSet`, still runs each case the argument it is named for; its items must own what they
  hold, or borrow only from statics.
- `setup = <path>` names a function that builds the benchmark's input before the count or the
  clock starts, taking the case's argument when there are args. The benchmark takes the input
  by `&`, `&mut`, or by value, as its parameter's type says. Borrowed, the input is built once
  and kept across every call, so a `&mut` benchmark sees what the calls before it left;
  moved in, each call gets one of its own, built just before it outside the clock, and drops
  it where the benchmark does, so only one is live at a time.

## Throughput

A benchmark may declare what each call processes, for swole to divide its measures by:
`bytes = <expr>` or `items = <expr>`, one of them, once. Without args it is the count, any
integer; with args, a function of the case's argument, such as a closure `|n| n` or a path,
called with a clone of it as `setup` is, so each case scales by its own. A count that is zero,
negative, or over a `u64` fails the run, since such a call processes nothing to divide by.
swole then reports each count its mode measured per byte or item, such as `instructions/byte`
and `estimated cycles/byte` under `sim` and each perf counter's under `quick` and `bench`, the
allocations and allocated bytes per byte or item where swole counts them, and under `bench`, which alone
times natively, a `throughput` in `bytes/s` or `items/s` over its calls' wall clock; a benchmark that times itself
returns a time that may not be one, so it gets the per-byte or per-item counts and no rate. A benchmark that declares neither
reports as it would without.

A borrowed input may take steps between calls that neither the count nor the clock sees:
`before = <path>` names a function run on it before each call, and `after = <path>` one run
after each, such as a step that undoes what the call did. Each takes the input as the benchmark
borrows it, `&` or `&mut`. A step makes the clock read around each call rather than around a
sample's, which costs a few dozen nanoseconds a call, as does an input or a return value that
needs dropping; neither steps, inputs built between calls, nor drops count against the
millisecond a sample fills, up to 50 ms a sample.

A benchmark whose time is not its calls' wall clock, such as a server's own busy time, a thread's
CPU time, or a fixture sized by how many calls it serves, times itself: its last parameter is
`Iterations`, the calls it is to make, which a `for` loop takes, and it returns the
`std::time::Duration` they took, after its one input if it has one. A count calls it for one
iteration and counts all of it, untimed parts included.

What the function returns goes through `black_box` and drops after the count or the call's
clock stops, before the next call, so returning a value keeps its computation and its free out
of both, and a returned guard never blocks the next call; it must own what it holds. The argument or input goes through `black_box`, and the function is called
through a pointer the optimizer cannot see through, so it compiles as written rather than
folded into its caller. A function that is async, unsafe, generic, or a method, or that takes
more than that one input, fails to compile, saying why: an async benchmark builds its runtime
in setup, or in a `static` when its input is moved in, and blocks on its future inside it.

## Names

A benchmark is named by its file, relative to the workspace root, then its module path within
the crate, its function, and its argument, such as `rs/net/benches/varint.rs::encode[300]`
or `ffi/moq/bench/export.rs::rust::call`, as CodSpeed's crates name theirs by file, group, and
function. Two of one name fail the run, as does a name holding a control character, such as
an argument that shows a line break: swole reads names a line each. Cargo names a member's file relative to the
workspace root; a file it names by an absolute path, as for a target whose path lies outside
its package, fails to compile, since a name holding a host's path would never match another
host's: remap it with `--remap-path-prefix`, as [`just ffi bench-build`](https://github.com/kixel-ai/rust2/blob/main/ffi/justfile)
does.

## How it runs

`swole::main()` runs every benchmark once, sorted by name, setting up each case
and bracketing its call with Callgrind's client requests, then prints `swole ran <name>`
after it, starting its records on a fresh line, as the benchmark's own output may not have ended one,
and before that `swole throughput <bytes|items> <n> <name>` for one that declares it. Natively the requests are instructions that do nothing, so `cargo bench` and
`cargo test --benches` run each case once as a check. With `SWOLE_BENCH` set to a name, it
runs that benchmark alone, which swole sets on each job. It takes no arguments but
Cargo's own `--bench` and `--quiet`; swole's `--benchmarks` chooses benchmarks.

With `SWOLE_TIME=1` as well, it sets that benchmark up and then times it, a sample for
each iteration count a line of stdin holds, printing its `swole throughput` line, if any, before
the first, and `swole sample <nanoseconds>` on a
line of its own after each, until stdin ends, after a `swole rusage` line of what the process
used over the sample: user and system CPU nanoseconds, peak resident bytes, minor and major page
faults, and voluntary and involuntary context switches. swole drives it so, and reads a
benchmark's own output on stdout around its samples.

Under Valgrind with `--instr-atstart=no`, as swole runs it, each case's count is
everything its call executes on every thread, between `ZERO_STATS` and
`START_INSTRUMENTATION` before it and `STOP_INSTRUMENTATION` and `DUMP_STATS_AT <name>` after:
the body, its call, and the requests' own few dozen instructions at most. A program whose benchmarks swole runs as a
`--program` runs every case in one process, each dumped under its own name.

The requests are x86_64's and aarch64's; elsewhere the harness requests nothing, and
swole fails a job that measured no benchmark.

Run by swole, it prints `swole threaded <name>` for a case beside which the process had another
thread: one there when its call started or ended, or, under swole's preload or its macOS
interposer, one its call started;
and `swole alone <name>` for any other, so swole counts a case again under Valgrind when a harness
too old to watch says neither.

With `SWOLE_COUNTERS` set to how many Linux perf counters swole opened on the process before it
ran the target, as `<hardware>+<software>`, it starts them all after the case's setup and reads
each hardware one in user space with `rdpmc`, then reads them again after its call and stops
them, so a count holds no system call. It counts only under swole's preload, or its build that counts threads alone, whose thread count
tells swole whether another thread, whose work the counters cannot see, ran beside a call. It
then prints a `swole rusage` line of what the process used over the call and
`swole counted <value>... <name>`, each counter's value in the order swole opened them, before
`swole ran <name>`. The benchmark itself needs no privilege: swole opens the counters with its
own. `swole quick` sets `SWOLE_COUNTERS` alone (counting one call without warm-up). `swole bench` sets
both `SWOLE_COUNTERS` and `SWOLE_ITERATIONS`: the harness first replays the uncounted calls `bench`
warms it up with before timing, one call and then batches doubled from one up to that many, then
calls it that many times, counting a deterministic stratified subset of up to 64 of those calls
(while watching every call for worker threads), and prints `swole calls <n> <name>`, how many its
counts cover, or a benchmark that times itself once, for that many iterations. See the
[README](../../README.md#quick).

With `SWOLE_HEAP=1`, under the allocation interposer swole preloads (or on macOS, inserts), it
first allocates once through the program's global allocator to see that the interposer counts
it, then sets the case up and calls it once, the interposer counting only from just before the
call to just after it, and prints `swole allocs <allocations> <bytes> <peak bytes> <name>`, or
`swole allocs bypassed <name>` when that first allocation never reached the interposer. Without
the interposer, as in a static executable, it fails. See the
[README](../../README.md#allocations).
