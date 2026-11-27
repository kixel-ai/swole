//! swole end to end, under the Valgrind on PATH: this test binary is also the program it
//! counts, before libtest starts, which brackets each benchmark with Callgrind's client requests
//! as CodSpeed's instrument hooks do.

// Valgrind, and the fingerprint, are x86_64 Linux's here.
#![cfg(all(target_os = "linux", target_arch = "x86_64"))]

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

/// The benchmarks the fixture runs, `name=iterations` comma-separated, when it is the program.
const FIXTURE: &str = "SWOLE_FIXTURE";

/// Callgrind's client request codes, from valgrind/callgrind.h.
const BASE: u64 = (b'C' as u64) << 24 | (b'T' as u64) << 16;
const DUMP_STATS_AT: u64 = BASE + 3;
const ZERO_STATS: u64 = BASE + 1;
const START_INSTRUMENTATION: u64 = BASE + 4;
const STOP_INSTRUMENTATION: u64 = BASE + 5;

/// A Valgrind client request: Valgrind's magic preamble, a no-op on the CPU, then the request.
fn request(code: u64, arg: u64) {
	let args = [code, arg, 0, 0, 0, 0];
	// SAFETY: the rotations sum to 128, leaving rdi as it was, and `xchg rbx, rbx` changes
	// nothing; under Valgrind, the request reads `args` and writes rdx.
	unsafe {
		std::arch::asm!(
			"rol rdi, 3", "rol rdi, 13", "rol rdi, 61", "rol rdi, 51", "xchg rbx, rbx",
			in("rax") args.as_ptr(),
			inout("rdx") 0u64 => _,
			options(nostack),
		);
	}
}

/// A timer setting, as `timerfd_settime` takes it.
#[repr(C)]
struct Itimerspec {
	interval: [i64; 2],
	value: [i64; 2],
}

unsafe extern "C" {
	fn timerfd_create(clock: i32, flags: i32) -> i32;
	fn timerfd_settime(fd: i32, flags: i32, new: *const Itimerspec, old: *mut Itimerspec) -> i32;
}

/// The fixture: runs each benchmark `FIXTURE` names as a loop of its iterations, between
/// the requests CodSpeed's hooks make. One whose name starts with `noisy` runs up to 2,000
/// more, as many as std's random `HashMap` seed and the clock choose. One whose name starts
/// with `abstimer` first arms a timer at an absolute time, repeating every 2^64 ns and more,
/// which the kernel takes though the span overflows a count of nanoseconds. One whose name
/// starts with `cwd` runs as many more as its working directory's path has bytes. One whose name
/// starts with `seq` or `stride` instead reads that many words of a 1 MiB buffer, one after
/// another or a cache line apart, and one whose name starts with `sorted` or `shuffled` branches
/// on that many bits, about half of them set, all set first or in a fixed pseudorandom order, so each
/// pair runs the same instructions but for its caches' or branch predictor's hits.
fn fixture() {
	let Ok(list) = std::env::var(FIXTURE) else { return };
	for bench in list.split(',').filter(|bench| !bench.is_empty()) {
		let (name, iterations) = bench.split_once('=').expect("name=iterations");
		let mut iterations: u64 = iterations.parse().expect("a count");
		if name.starts_with("noisy") {
			use std::hash::BuildHasher;
			let seeded = std::collections::hash_map::RandomState::new().hash_one(name) % 1000;
			let clock = std::time::SystemTime::now()
				.duration_since(std::time::UNIX_EPOCH)
				.unwrap()
				.subsec_nanos() as u64
				% 1000;
			iterations += seeded + clock;
		}
		if name.starts_with("cwd") {
			iterations += std::env::current_dir().unwrap().as_os_str().len() as u64;
		}
		if name.starts_with("abstimer") {
			const CLOCK_MONOTONIC: i32 = 1;
			const TFD_TIMER_ABSTIME: i32 = 1;
			let setting = Itimerspec {
				interval: [18_446_744_074, 0],
				value: [86_400, 50_000_000],
			};
			// SAFETY: a timerfd armed with a setting to read; nothing reads the timer.
			let armed = unsafe {
				timerfd_settime(
					timerfd_create(CLOCK_MONOTONIC, 0),
					TFD_TIMER_ABSTIME,
					&setting,
					std::ptr::null_mut(),
				)
			};
			assert_eq!(armed, 0, "the kernel takes the setting");
		}
		let words = vec![1u64; 1 << 17];
		let step = std::hint::black_box(if name.starts_with("stride") { 8 } else { 1 });
		// xorshift64's top bit, from a fixed seed.
		let mut state = 0x9e37_79b9_7f4a_7c15u64;
		let mut bits: Vec<u8> = (0..iterations)
			.map(|_| {
				state ^= state << 13;
				state ^= state >> 7;
				state ^= state << 17;
				(state >> 63) as u8
			})
			.collect();
		if name.starts_with("sorted") {
			bits.sort_unstable_by(|a, b| b.cmp(a));
		}
		// Read before counting starts: comparing names counts as their addresses fall.
		let reads = name.starts_with("seq") || name.starts_with("stride");
		let branches = name.starts_with("sorted") || name.starts_with("shuffled");
		request(ZERO_STATS, 0);
		request(START_INSTRUMENTATION, 0);
		let mut sum = 0u64;
		if reads {
			for i in 0..iterations as usize {
				sum = sum.wrapping_add(words[(i * step) & (words.len() - 1)]);
			}
		} else if branches {
			for &bit in &bits {
				if std::hint::black_box(bit) != 0 {
					sum = std::hint::black_box(sum + 1);
				}
			}
		} else {
			for i in 0..iterations {
				sum = std::hint::black_box(sum.wrapping_add(i));
			}
		}
		std::hint::black_box(sum);
		request(STOP_INSTRUMENTATION, 0);
		let name = std::ffi::CString::new(name).expect("no NUL");
		request(DUMP_STATS_AT, name.as_ptr() as u64);
	}
}

/// Runs the fixture, then exits, before libtest's `main` when `FIXTURE` is set, so its benchmarks
/// run alone: libtest runs even one test on a thread of its own, beside a main thread that may
/// still be on its way to waiting for it when a benchmark starts, and Callgrind counts both.
#[used]
#[unsafe(link_section = ".init_array")]
static FIXTURE_FIRST: extern "C" fn() = {
	extern "C" fn first() {
		if std::env::var_os(FIXTURE).is_some() {
			fixture();
			std::process::exit(0);
		}
	}
	first
};

/// The shell command that runs the fixture as a program counting `benches`, launched through
/// the command `wrap`, if any.
fn program(benches: &str, wrap: &str) -> String {
	let me = std::env::current_exe().unwrap();
	// Quoted for the shell, which may otherwise split the path.
	let me = me.to_str().unwrap().replace('\'', r"'\''");
	format!("{FIXTURE}={benches} {wrap} '{me}'")
}

/// swole, run in `dir` with `args`, counting the fixture as its one program, launched
/// through the command `wrap`, if any.
fn bench(dir: &Path, benches: &str, wrap: &str, args: &[&str]) -> Output {
	command(dir, benches, wrap, args).output().unwrap()
}

/// The command [`bench`] runs.
fn command(dir: &Path, benches: &str, wrap: &str, args: &[&str]) -> Command {
	let mut command = Command::new(env!("CARGO_BIN_EXE_swole"));
	command
		.arg("sim")
		.args(["--program", &program(benches, wrap)])
		.args(args)
		.current_dir(dir)
		.env_remove("GITHUB_STEP_SUMMARY");
	command
}

fn scratch(name: &str) -> std::path::PathBuf {
	let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join(name);
	let _ = std::fs::remove_dir_all(&dir);
	std::fs::create_dir_all(&dir).unwrap();
	dir
}

#[test]
fn counts_each_benchmark_a_program_brackets() {
	let dir = scratch("counts");
	let out = bench(&dir, "a=1000,b=2000", "", &["--out", "metrics.json"]);
	assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
	let metrics: serde_json::Value =
		serde_json::from_str(&std::fs::read_to_string(dir.join("metrics.json")).unwrap()).unwrap();
	let units = metrics["units"].as_object().unwrap();
	assert_eq!(units.len(), 2);
	let (measure, unit) = units.iter().find(|(name, _)| name.starts_with("instructions")).unwrap();
	assert_eq!(unit, "instructions");
	assert!(measure.starts_with("instructions (x86_64-"), "{measure}");
	let (cycles, unit) = units.iter().find(|(name, _)| name.starts_with("estimated")).unwrap();
	assert_eq!(unit, "cycles");
	assert_eq!(cycles, &measure.replace("instructions", "estimated cycles"));
	let count = |name: &str| {
		metrics["samples"][0]["metrics"][name][measure]["value"]
			.as_u64()
			.unwrap()
	};
	let (a, b) = (count("a"), count("b"));
	// The loop dominates each count, so twice the iterations is near twice the count.
	assert!(a > 1000 && b > 1000, "{a} {b}");
	let ratio = b as f64 / a as f64;
	assert!((1.9..2.1).contains(&ratio), "{a} {b}");
	// The same program counts the same again.
	let again = bench(&dir, "a=1000,b=2000", "", &["--out", "again.json"]);
	assert!(again.status.success(), "{}", String::from_utf8_lossy(&again.stderr));
	assert_eq!(
		std::fs::read(dir.join("metrics.json")).unwrap(),
		std::fs::read(dir.join("again.json")).unwrap()
	);
}

/// Callgrind's cache and branch simulation tells apart what its instructions alone cannot: a pair
/// of benchmarks that run the same instructions, but miss the cache or mispredict a branch on
/// every iteration in one and seldom in the other, and estimates the cycles each takes, the same
/// in every sample and every run.
#[test]
fn simulates_caches_and_branches_exactly() {
	let dir = scratch("simulated");
	const N: f64 = 10_000.0;
	let benches = "seq=10000,stride=10000,sorted=10000,shuffled=10000";
	let report = |name: &str| {
		let out = bench(&dir, benches, "", &["--report", name]);
		assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
		std::fs::read_to_string(dir.join(name)).unwrap()
	};
	let text = report("one.json");
	assert_eq!(text, report("two.json"), "a second run counts otherwise");
	let report: serde_json::Value = serde_json::from_str(&text).unwrap();
	assert!(
		report["host"].as_array().unwrap().iter().any(|line| line
			.as_str()
			.unwrap()
			.starts_with("simulation: --cache-sim=yes --branch-sim=yes --I1=")),
		"{}",
		report["host"]
	);
	let value = |bench: &str, measure: &str| {
		let measured = &report["benchmarks"][bench]["head"][measure];
		// A program's benchmarks count five times, each the same.
		assert_eq!(measured["lower"], measured["upper"], "{bench} {measure}: {measured}");
		assert_eq!(measured["lower"], measured["value"], "{bench} {measure}: {measured}");
		measured["value"]
			.as_f64()
			.unwrap_or_else(|| panic!("{bench} has no {measure}: {report}"))
	};
	let near = |a: f64, b: f64| (a - b).abs() / b < 0.01;
	for (fast, slow, event) in [
		("seq", "stride", "D1 read misses"),
		("sorted", "shuffled", "conditional mispredicts"),
	] {
		let ir = (value(fast, "instructions"), value(slow, "instructions"));
		assert!(near(ir.0, ir.1), "{fast} {slow}: {ir:?} instructions");
		let missed = (value(fast, event), value(slow, event));
		assert!(
			missed.0 < N / 7.0 && missed.1 > N / 3.0,
			"{fast} {slow}: {missed:?} {event}"
		);
		let cycles = (value(fast, "estimated cycles"), value(slow, "estimated cycles"));
		// A debug build's dozens of instructions an iteration dilute what a miss adds.
		assert!(cycles.1 > 1.1 * cycles.0, "{fast} {slow}: {cycles:?} estimated cycles");
	}
}

#[test]
fn counts_only_the_benchmarks_a_list_names() {
	let dir = scratch("listed");
	std::fs::write(dir.join("list"), "b\n").unwrap();
	let out = bench(
		&dir,
		"a=10,b=20",
		"",
		&["--benchmarks", "list", "--out", "metrics.json"],
	);
	assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
	let metrics: serde_json::Value =
		serde_json::from_str(&std::fs::read_to_string(dir.join("metrics.json")).unwrap()).unwrap();
	let names: Vec<&String> = metrics["samples"][0]["metrics"].as_object().unwrap().keys().collect();
	assert_eq!(names, ["b"]);
}

#[test]
fn fails_on_a_listed_benchmark_no_program_has() {
	let dir = scratch("missing");
	std::fs::write(dir.join("list"), "a\nsuspect\n").unwrap();
	let out = bench(&dir, "a=10", "", &["--benchmarks", "list"]);
	assert!(!out.status.success());
	let err = String::from_utf8_lossy(&out.stderr);
	assert!(err.contains("no benchmark is named suspect"), "{err}");
}

#[test]
fn allowed_counts_the_listed_benchmarks_a_program_has_and_reports_the_rest() {
	let dir = scratch("allowed");
	std::fs::write(dir.join("list"), "a\nsuspect\n").unwrap();
	let summary = dir.join("summary.md");
	let out = command(
		&dir,
		"a=10,b=20",
		"",
		&["--benchmarks", "list", "--allow-missing", "--out", "metrics.json"],
	)
	.env("GITHUB_STEP_SUMMARY", &summary)
	.output()
	.unwrap();
	let err = String::from_utf8_lossy(&out.stderr);
	assert!(out.status.success(), "{err}");
	assert!(err.contains("no benchmark is named suspect; counted the rest"), "{err}");
	assert!(std::fs::read_to_string(&summary).unwrap().contains("- `suspect`"));
	let metrics: serde_json::Value =
		serde_json::from_str(&std::fs::read_to_string(dir.join("metrics.json")).unwrap()).unwrap();
	let names: Vec<&String> = metrics["samples"][0]["metrics"].as_object().unwrap().keys().collect();
	assert_eq!(names, ["a"]);

	// None of them there still fails.
	std::fs::write(dir.join("list"), "suspect\n").unwrap();
	let out = bench(&dir, "a=10", "", &["--benchmarks", "list", "--allow-missing"]);
	assert!(!out.status.success());
	let err = String::from_utf8_lossy(&out.stderr);
	assert!(err.contains("no benchmark is named suspect"), "{err}");
}

#[test]
fn fails_on_two_benchmarks_of_one_name() {
	let dir = scratch("twice");
	let out = bench(&dir, "a=10,a=20", "", &[]);
	assert!(!out.status.success());
	let err = String::from_utf8_lossy(&out.stderr);
	assert!(err.contains("two benchmarks are named a"), "{err}");
}

#[test]
fn fails_on_a_program_that_measures_nothing() {
	let dir = scratch("nothing");
	let out = bench(&dir, "", "", &[]);
	assert!(!out.status.success());
	let err = String::from_utf8_lossy(&out.stderr);
	assert!(err.contains("measured no benchmark"), "{err}");
}

/// swole, run in this workspace with `args` on the harness's fixture benchmark target,
/// built in a target directory of its own, since this test's build holds the workspace's.
fn harness(args: &[&str]) -> Output {
	target("fixture", args).output().unwrap()
}

/// swole, run in this workspace with `args` on the harness's benchmark target `bench`.
fn target(bench: &str, args: &[&str]) -> Command {
	let mut command = Command::new(env!("CARGO_BIN_EXE_swole"));
	command
		.args(args)
		.args(["--", "-p", "swole", "--bench", bench])
		.current_dir(env!("CARGO_MANIFEST_DIR"))
		.env(
			"CARGO_TARGET_DIR",
			Path::new(env!("CARGO_TARGET_TMPDIR")).join("harness"),
		)
		.env_remove("GITHUB_STEP_SUMMARY");
	command
}

/// Each of a benchmark's allocation measures in a report's side: its value, and whether it is
/// sampled, its least and most beside it.
fn allocs(side: &serde_json::Value) -> ([u64; 3], bool) {
	let mut values = [0; 3];
	let mut sampled = None;
	for (i, name) in ["alloc_count", "alloc_bytes", "peak_live_bytes"]
		.into_iter()
		.enumerate()
	{
		let measure = &side[name];
		assert_eq!(measure["pass"], "allocation", "{name}: {side}");
		values[i] = measure["value"].as_u64().unwrap_or_else(|| panic!("{name}: {side}"));
		let this = measure["sampled"].as_bool().unwrap();
		assert_eq!(this, measure.get("lower").is_some(), "{name}: {side}");
		assert_eq!(*sampled.get_or_insert(this), this, "{side}");
	}
	(values, sampled.unwrap())
}

/// The interposer counts what a counting `#[global_allocator]` does on every single-threaded
/// workload, exactly; a call beside another thread, even one that never allocates, is sampled;
/// and an allocator that never reaches malloc has no counts rather than none.
#[test]
fn counts_allocations_as_a_global_allocator_does() {
	let dir = scratch("allocs");
	let report = dir.join("report.json");
	let metrics = dir.join("metrics.json");
	let out = target(
		"allocs",
		&[
			"sim",
			"--report",
			report.to_str().unwrap(),
			"--out",
			metrics.to_str().unwrap(),
		],
	)
	.env("SWOLE_TEST_ALLOCS", &dir)
	.output()
	.unwrap();
	assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
	let report: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(&report).unwrap()).unwrap();
	let file = "crates/swole/benches/allocs.rs::";
	let side = |name: &str| report["benchmarks"][format!("{file}{name}")]["head"].clone();
	for name in ["vec", "boxes", "string", "aligned"] {
		let counted: Vec<u64> = std::fs::read_to_string(dir.join(name))
			.unwrap()
			.split(' ')
			.map(|n| n.parse().unwrap())
			.collect();
		let (values, sampled) = allocs(&side(name));
		assert!(!sampled, "{name}");
		assert_eq!(
			values.as_slice(),
			counted,
			"{name}: the interposer against the global allocator"
		);
		assert!(values[0] > 0, "{name}");
	}
	for name in ["spawned", "beside"] {
		assert!(allocs(&side(name)).1, "{name}");
	}
	// The CLI marks a sampled measure, and metrics.json holds each, with its spread when sampled.
	let table = String::from_utf8_lossy(&out.stdout);
	assert!(
		table.contains(&format!("| {file}beside | alloc_count | ~1 allocations (")),
		"{table}"
	);
	let metrics: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(metrics).unwrap()).unwrap();
	// Named by the run's fingerprint and the interposer's hash.
	let (series, unit) = metrics["units"]
		.as_object()
		.unwrap()
		.iter()
		.find(|(series, _)| series.starts_with("alloc_count ("))
		.unwrap();
	assert_eq!(unit, "allocations");
	let fingerprint = series.strip_prefix("alloc_count (").unwrap().strip_suffix(')').unwrap();
	assert!(
		fingerprint.starts_with(&format!("{}+", report["fingerprint"].as_str().unwrap())),
		"{fingerprint}"
	);
	let point = |name: &str| {
		metrics["samples"][0]["metrics"][format!("{file}{name}")][format!("peak_live_bytes ({fingerprint})")].clone()
	};
	assert!(point("vec").get("lower_value").is_none(), "{}", point("vec"));
	assert!(point("spawned").get("lower_value").is_some(), "{}", point("spawned"));

	let report = dir.join("bypass.json");
	let out = target("bypass", &["sim", "--report", report.to_str().unwrap()])
		.output()
		.unwrap();
	assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
	let err = String::from_utf8_lossy(&out.stderr);
	assert!(err.contains("never reaches swole's allocation interposer"), "{err}");
	let report: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(&report).unwrap()).unwrap();
	let head = &report["benchmarks"]["crates/swole/benches/bypass.rs::bumped"]["head"];
	assert!(
		head["instructions"].is_object() && head.get("alloc_count").is_none(),
		"{head}"
	);
}

#[test]
fn counts_each_harness_benchmark_alone_without_its_setup() {
	let dir = scratch("harness");
	let metrics = dir.join("metrics.json");
	let report = dir.join("report.json");
	let out = harness(&[
		"sim",
		"--out",
		metrics.to_str().unwrap(),
		"--report",
		report.to_str().unwrap(),
	]);
	assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
	// A benchmark that declares what each call processes reports its instructions per byte or item,
	// but no rate, since sim measures no native time; one that declares none, neither.
	let report: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(report).unwrap()).unwrap();
	assert_eq!(report["schema_version"], 1);
	let head = |name: &str| report["benchmarks"][format!("crates/swole/benches/fixture.rs::{name}")]["head"].clone();
	for (name, per, amount) in [("sized[2000]", "byte", 2000.0), ("alone", "item", 1000.0)] {
		let head = head(name);
		let instructions = head["instructions"]["value"].as_f64().unwrap();
		let derived = &head[format!("instructions/{per}")];
		assert_eq!(derived["unit"], format!("instructions/{per}"), "{name}: {head}");
		let ratio = derived["value"].as_f64().unwrap() * amount / instructions;
		assert!((ratio - 1.0).abs() < 1e-3, "{name}: {head}");
		assert!(head.get("throughput").is_none(), "{name}: {head}");
		// So does every other count and the call's allocations, but not the most it held at once.
		assert_eq!(
			head[format!("estimated cycles/{per}")]["unit"],
			format!("cycles/{per}"),
			"{name}: {head}"
		);
		assert_eq!(
			head[format!("data reads/{per}")]["unit"],
			format!("events/{per}"),
			"{name}: {head}"
		);
		if head.get("alloc_count").is_some() {
			assert_eq!(
				head[format!("alloc_count/{per}")]["unit"],
				format!("allocations/{per}"),
				"{name}: {head}"
			);
			assert!(head.get(format!("peak_live_bytes/{per}")).is_none(), "{name}: {head}");
		}
	}
	let borrowed = head("borrowed");
	assert!(
		borrowed
			.as_object()
			.unwrap()
			.keys()
			.all(|key| !key.ends_with("/byte") && !key.ends_with("/item")),
		"{borrowed}"
	);
	let table = String::from_utf8_lossy(&out.stdout);
	assert!(
		table.contains("| crates/swole/benches/fixture.rs::sized[2000] | instructions/byte | "),
		"{table}"
	);
	let metrics: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(metrics).unwrap()).unwrap();
	let metrics = metrics["samples"][0]["metrics"].as_object().unwrap();
	let file = "crates/swole/benches/fixture.rs::";
	let names: Vec<&str> = metrics.keys().map(|name| name.strip_prefix(file).unwrap()).collect();
	assert_eq!(
		names,
		[
			"alone",
			"borrowed",
			"dropped",
			"generic[4000]",
			"handed",
			"inner::nested[a]",
			"inner::nested[bc]",
			"sized[1000]",
			"sized[2000]",
			"sized[4000]",
			"slept",
			"spawned[100000]",
			"spawned[1000]",
			"stepped",
			"timed",
			"unstable[1000]",
			"unstable[8000]",
			"word[word]"
		]
	);
	let count = |name: &str| {
		let measures = metrics[&format!("{file}{name}")].as_object().unwrap();
		let (_, instructions) = measures
			.iter()
			.find(|(measure, _)| measure.starts_with("instructions ("))
			.unwrap();
		instructions["value"].as_u64().unwrap()
	};
	let alone = count("alone");
	assert!(alone > 1000, "{alone}");
	let near = |name: &str, expected: u64| {
		let ratio = count(name) as f64 / expected as f64;
		assert!(
			(0.9..1.1).contains(&ratio),
			"{name}: {} against {expected}",
			count(name)
		);
	};
	// Every one spins a thousand times but the larger sized ones; a setup's or a step's million
	// spins and its input's free would count a thousand times as much, were they counted.
	near("sized[1000]", alone);
	near("sized[2000]", 2 * alone);
	near("sized[4000]", 4 * alone);
	near("unstable[1000]", alone);
	near("unstable[8000]", 8 * alone);
	near("borrowed", alone);
	near("dropped", alone);
	near("generic[4000]", alone);
	near("word[word]", alone);
	near("stepped", alone);
	near("timed", alone);
	near("inner::nested[a]", alone);
	near("inner::nested[bc]", alone);

	// A list counts only what it names, and fails, before counting any, on a name none has.
	std::fs::write(dir.join("list"), format!("{file}sized[2000]\n")).unwrap();
	let only = dir.join("only.json");
	let out = harness(&[
		"sim",
		"--benchmarks",
		dir.join("list").to_str().unwrap(),
		"--out",
		only.to_str().unwrap(),
	]);
	assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
	let only: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(only).unwrap()).unwrap();
	let only = only["samples"][0]["metrics"].as_object().unwrap();
	assert_eq!(only.keys().collect::<Vec<_>>(), [&format!("{file}sized[2000]")]);
	std::fs::write(dir.join("list"), format!("{file}sized[3000]\n")).unwrap();
	let out = harness(&["sim", "--benchmarks", dir.join("list").to_str().unwrap()]);
	assert!(!out.status.success());
	let err = String::from_utf8_lossy(&out.stderr);
	assert!(
		err.contains(&format!("no benchmark is named {file}sized[3000]")),
		"{err}"
	);
	// Allowed, it counts the one there.
	std::fs::write(dir.join("list"), format!("{file}sized[2000]\n{file}sized[3000]\n")).unwrap();
	let some = dir.join("some.json");
	let out = harness(&[
		"sim",
		"--benchmarks",
		dir.join("list").to_str().unwrap(),
		"--allow-missing",
		"--out",
		some.to_str().unwrap(),
	]);
	assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
	let some: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(some).unwrap()).unwrap();
	let some = some["samples"][0]["metrics"].as_object().unwrap();
	assert_eq!(some.keys().collect::<Vec<_>>(), [&format!("{file}sized[2000]")]);
}

/// Each count and name of the table `run` prints.
fn table(stdout: &[u8]) -> std::collections::BTreeMap<String, u64> {
	String::from_utf8_lossy(stdout)
		.lines()
		.map_while(|line| {
			let (count, rest) = line.trim_start().split_once("  ")?;
			// A sampled count's least and most follow its name.
			let name = rest.split("  ").next()?;
			Some((name.to_owned(), count.parse().ok()?))
		})
		.collect()
}

/// The fixture's benchmarks that run another thread.
const THREADED: [&str; 3] = ["handed", "spawned[100000]", "spawned[1000]"];

/// A file naming each of the fixture's benchmarks that runs no other thread, one a line.
fn single_threaded(dir: &Path) -> PathBuf {
	let file = "crates/swole/benches/fixture.rs::";
	let names = [
		"alone",
		"borrowed",
		"dropped",
		"generic[4000]",
		"inner::nested[a]",
		"inner::nested[bc]",
		"sized[1000]",
		"sized[2000]",
		"sized[4000]",
		"slept",
		"stepped",
		"timed",
		"unstable[1000]",
		"unstable[8000]",
		"word[word]",
	];
	let list = dir.join("single-threaded");
	std::fs::write(&list, names.map(|name| format!("{file}{name}\n")).concat()).unwrap();
	list
}

/// On a host that exposes perf counters, they count what Valgrind does on the calling thread,
/// and fail a benchmark that runs another; on one that exposes none, as a hosted CI runner, the
/// run fails, saying so.
#[test]
fn counts_with_perf_counters_what_valgrind_does() {
	let dir = scratch("quick-single");
	let file = "crates/swole/benches/fixture.rs::";
	// Each alone, since a host with one PMU CPU runs one job at a time and stops at a failure.
	for name in THREADED {
		let list = dir.join("threaded");
		std::fs::write(&list, format!("{file}{name}\n")).unwrap();
		let out = harness(&["quick", "--benchmarks", list.to_str().unwrap()]);
		let err = String::from_utf8_lossy(&out.stderr);
		assert!(!out.status.success(), "{err}");
		if err.contains("quick counts with perf counters, and") {
			return;
		}
		assert!(
			err.contains(&format!("{file}{name}: another thread ran beside a counted call"))
				&& err.contains("count it with sim"),
			"{name}: {err}"
		);
	}
	let list = single_threaded(&dir);
	let list = list.to_str().unwrap();
	let report = dir.join("report.json");
	let out = harness(&["quick", "--benchmarks", list, "--report", report.to_str().unwrap()]);
	assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
	// Each counter's share of a byte, of a benchmark that declares its bytes.
	let report: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(report).unwrap()).unwrap();
	let head = &report["benchmarks"][format!("{file}sized[2000]")]["head"];
	let instructions = head["instructions:u"]["value"].as_f64().unwrap();
	let per_byte = head["instructions:u/byte"]["value"].as_f64().unwrap();
	assert!((per_byte * 2000.0 / instructions - 1.0).abs() < 1e-3, "{head}");
	assert!(head.get("throughput").is_none(), "{head}");
	let perf = table(&out.stdout);
	let sim = harness(&["sim", "--benchmarks", list]);
	assert!(sim.status.success(), "{}", String::from_utf8_lossy(&sim.stderr));
	let valgrind = table(&sim.stdout);
	assert_eq!(perf.keys().collect::<Vec<_>>(), valgrind.keys().collect::<Vec<_>>());
	assert_eq!(perf.len(), 15);
	for (name, &count) in &perf {
		// The same instructions, but for each bracket's own few dozen.
		let under = valgrind[name];
		assert!(
			count.abs_diff(under) < 100,
			"{name}: {count} natively, {under} under Valgrind"
		);
	}
	// A count reads the counters in user space, the same few instructions around every call,
	// whatever follows it, and exactly so on every run.
	let (alone, dropped) = (perf[&format!("{file}alone")], perf[&format!("{file}dropped")]);
	assert!(alone.abs_diff(dropped) < 16, "{alone} {dropped}");
	// A call that sleeps leaves the CPU and its counters move, which moves its count not at all:
	// it is as far from Valgrind's as `alone`'s.
	let gap = |name: &str| {
		let name = format!("{file}{name}");
		perf[&name] as i64 - valgrind[&name] as i64
	};
	assert!(
		gap("slept").abs_diff(gap("alone")) < 16,
		"{} {}",
		gap("slept"),
		gap("alone")
	);
	// Repeat counts stay tight for short calls (at most a few instructions if a hardware interrupt
	// lands under load) and close for longer ones.
	let again = table(&harness(&["quick", "--benchmarks", list]).stdout);
	assert_eq!(perf.keys().collect::<Vec<_>>(), again.keys().collect::<Vec<_>>());
	let short = perf.values().filter(|&&count| count < 5000).count();
	assert_eq!(short, 11, "{perf:?}");
	for (name, &count) in &perf {
		let other = again[name];
		if count < 5000 {
			assert!(count.abs_diff(other) <= 4, "{name}: {count} then {other}");
		} else {
			assert!(count.abs_diff(other) < 16, "{name}: {count} then {other}");
		}
	}
}

#[test]
fn repeats_a_count_that_reads_randomness_and_the_clock() {
	let dir = scratch("noisy");
	let count = |out: &str| {
		let run = bench(&dir, "noisy=1000", "", &["--out", out]);
		assert!(run.status.success(), "{}", String::from_utf8_lossy(&run.stderr));
		std::fs::read(dir.join(out)).unwrap()
	};
	// Each run would otherwise draw one of a million counts.
	assert_eq!(count("one.json"), count("two.json"));
}

#[test]
fn fails_on_a_process_that_never_loaded_the_preload() {
	let dir = scratch("bypass");
	let out = bench(&dir, "a=10", "env -u LD_PRELOAD", &[]);
	assert!(!out.status.success());
	let err = String::from_utf8_lossy(&out.stderr);
	assert!(err.contains("never loaded swole's preload"), "{err}");
}

#[test]
fn fails_on_a_timer_armed_at_an_absolute_time() {
	let dir = scratch("abstimer");
	let out = bench(&dir, "abstimer=10", "", &[]);
	assert!(
		!out.status.success(),
		"a timer the kernel reads on the host's clock was counted"
	);
}

/// Runs git in `dir` under a fixed identity, returning its stdout.
fn git(dir: &Path, args: &[&str]) -> String {
	let out = Command::new("git")
		.args([
			"-c",
			"user.name=test",
			"-c",
			"user.email=test@localhost",
			"-c",
			"commit.gpgsign=false",
		])
		.args(args)
		.current_dir(dir)
		.output()
		.unwrap();
	assert!(
		out.status.success(),
		"git {args:?}: {}",
		String::from_utf8_lossy(&out.stderr)
	);
	String::from_utf8(out.stdout).unwrap()
}

#[test]
fn compares_a_base_with_every_change_in_the_working_tree_from_one_directory() {
	let repo = scratch("compare");
	let cache = scratch("compare-cache");
	git(&repo, &["init", "--quiet"]);
	std::fs::write(repo.join("a.list"), "cwd=1000,a=1000,").unwrap();
	std::fs::write(repo.join("d.list"), "d=100,").unwrap();
	std::fs::write(repo.join(".gitignore"), "Cargo.lock\n").unwrap();
	git(&repo, &["add", "."]);
	git(&repo, &["commit", "--quiet", "-m", "base"]);
	// Changed but unstaged, staged only, untracked, and deleted.
	std::fs::write(repo.join("a.list"), "cwd=1000,a=2000,").unwrap();
	std::fs::write(repo.join("c.list"), "c=300,").unwrap();
	git(&repo, &["add", "c.list"]);
	std::fs::write(repo.join("b.list"), "b=500,").unwrap();
	std::fs::remove_file(repo.join("d.list")).unwrap();
	let status = git(&repo, &["status", "--porcelain"]);
	let out = Command::new(env!("CARGO_BIN_EXE_swole"))
		.args(["sim", "HEAD", "--program", &program("$(cat *.list)", "")])
		// Each side's build resolves its own ignored lockfile, never the side's before.
		.args(["--build", "! test -e Cargo.lock && touch Cargo.lock"])
		.current_dir(&repo)
		.env("XDG_CACHE_HOME", &cache)
		.output()
		.unwrap();
	assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
	let table = String::from_utf8(out.stdout).unwrap();
	let change = |name: &str| {
		let row = table
			.lines()
			.find(|row| row.starts_with(&format!("| {name} |")))
			.unwrap_or_else(|| panic!("no {name} in {table}"));
		row.rsplit('|').nth(1).unwrap().trim().to_owned()
	};
	// Both sides count from one working directory, whose path is in the count.
	assert_eq!(change("cwd"), "+0.00%");
	let a = change("a");
	assert!(a.starts_with('+') && a != "+0.00%", "{table}");
	assert_eq!(change("b"), "added");
	assert_eq!(change("c"), "added");
	assert_eq!(change("d"), "removed");
	// The caller's index, working tree, and stash are as they were.
	assert_eq!(git(&repo, &["status", "--porcelain"]), status);
	assert_eq!(git(&repo, &["stash", "list"]), "");
}

/// Writes a workspace package `name` under `repo` with one harness benchmark target, `bench`,
/// which needs the package's features `needs`.
fn package(repo: &Path, name: &str, bench: &str, needs: &[&str]) {
	let dir = repo.join(name);
	std::fs::create_dir_all(dir.join("benches")).unwrap();
	let harness = Path::new(env!("CARGO_MANIFEST_DIR")).join("../swole");
	let features: String = needs.iter().map(|f| format!("{f} = []\n")).collect();
	std::fs::write(
		dir.join("Cargo.toml"),
		format!(
			"[package]\nname = \"{name}\"\nedition = \"2024\"\n\n[features]\n{features}\n\
			 [dev-dependencies]\nswole = {{ path = {harness:?} }}\n\n\
			 [[bench]]\nname = \"{bench}\"\nharness = false\nrequired-features = {needs:?}\n"
		),
	)
	.unwrap();
	std::fs::create_dir_all(dir.join("src")).unwrap();
	std::fs::write(dir.join("src/lib.rs"), "").unwrap();
	std::fs::write(
		dir.join("benches").join(format!("{bench}.rs")),
		"#[swole::bench]\nfn spin() -> u64 {\n\t(0..1000u64).map(std::hint::black_box).sum()\n}\n\n\
		 fn main() {\n\tswole::main();\n}\n",
	)
	.unwrap();
}

#[test]
fn compares_a_selection_only_one_side_has() {
	let repo = scratch("compare-select");
	let cache = scratch("compare-select-cache");
	git(&repo, &["init", "--quiet"]);
	// The runner's own lockfile, so the workspace resolves the harness's dependencies as the
	// runner's build already has, offline.
	std::fs::copy(
		Path::new(env!("CARGO_MANIFEST_DIR")).join("../../Cargo.lock"),
		repo.join("Cargo.lock"),
	)
	.unwrap();
	std::fs::write(
		repo.join("Cargo.toml"),
		"[workspace]\nmembers = [\"a\"]\nresolver = \"3\"\n",
	)
	.unwrap();
	package(&repo, "a", "x", &[]);
	git(&repo, &["add", "."]);
	git(&repo, &["commit", "--quiet", "-m", "base"]);
	// The head adds a package whose benchmark target needs a feature.
	std::fs::write(
		repo.join("Cargo.toml"),
		"[workspace]\nmembers = [\"a\", \"b\"]\nresolver = \"3\"\n",
	)
	.unwrap();
	package(&repo, "b", "y", &["f"]);
	// Compare from `dir`, in `mode`, with the `cargo` args.
	let run = |dir: &Path, mode: &str, cargo: &[&str]| {
		Command::new(env!("CARGO_BIN_EXE_swole"))
			.args([mode, "HEAD"])
			.arg("--")
			.args(cargo)
			.current_dir(dir)
			.env("XDG_CACHE_HOME", &cache)
			.env(
				"CARGO_TARGET_DIR",
				Path::new(env!("CARGO_TARGET_TMPDIR")).join("compare-select-target"),
			)
			.env("CARGO_NET_OFFLINE", "true")
			.output()
			.unwrap()
	};
	let compare = |cargo: &[&str]| run(&repo, "sim", cargo);
	let out = compare(&["-p", "a", "--package=b", "--bench", "x", "--bench=y", "-F", "b/f"]);
	let err = String::from_utf8_lossy(&out.stderr);
	assert!(out.status.success(), "{err}");
	assert!(err.contains("HEAD has no -p b"), "{err}");
	assert!(err.contains("HEAD has no --bench y"), "{err}");
	assert!(err.contains("feature b/f"), "{err}");
	let table = String::from_utf8(out.stdout).unwrap();
	let row = |table: &str, name: &str| {
		table
			.lines()
			.find(|row| row.starts_with(&format!("| {name}/benches/")))
			.unwrap_or_else(|| panic!("no {name} in {table}"))
			.to_owned()
	};
	assert!(!row(&table, "a").contains(" - "), "{table}");
	assert!(row(&table, "b").ends_with("| added |"), "{table}");

	// The head's package alone: the base selects nothing, rather than every benchmark.
	let out = compare(&["-p", "b", "--features", "b/f"]);
	let err = String::from_utf8_lossy(&out.stderr);
	assert!(out.status.success(), "{err}");
	assert!(err.contains("HEAD has nothing the cargo args select"), "{err}");
	let table = String::from_utf8(out.stdout).unwrap();
	assert_eq!(table.lines().take_while(|line| !line.is_empty()).count(), 3, "{table}");
	assert!(row(&table, "b").ends_with("| added |"), "{table}");

	// So does a manifest only the head has.
	let out = compare(&["--manifest-path", "b/Cargo.toml", "--features", "f"]);
	let err = String::from_utf8_lossy(&out.stderr);
	assert!(out.status.success(), "{err}");
	assert!(err.contains("HEAD has no --manifest-path b/Cargo.toml"), "{err}");
	let table = String::from_utf8(out.stdout).unwrap();
	assert_eq!(table.lines().take_while(|line| !line.is_empty()).count(), 3, "{table}");
	assert!(row(&table, "b").ends_with("| added |"), "{table}");

	// So does the directory of a package only the head has, even once a run left the head's
	// build output in it.
	for _ in 0..2 {
		let out = run(&repo.join("b"), "sim", &["--features", "f"]);
		let err = String::from_utf8_lossy(&out.stderr);
		assert!(out.status.success(), "{err}");
		let table = String::from_utf8(out.stdout).unwrap();
		assert_eq!(table.lines().take_while(|line| !line.is_empty()).count(), 3, "{table}");
		assert!(row(&table, "b").ends_with("| added |"), "{table}");
	}

	// Under perf counters, the empty side carries the counted side's fingerprint.
	let out = run(&repo, "quick", &["-p", "b", "--features", "b/f"]);
	let err = String::from_utf8_lossy(&out.stderr);
	if out.status.success() {
		assert!(row(&String::from_utf8(out.stdout).unwrap(), "b").ends_with("| added |"));
	} else {
		assert!(err.contains("quick counts with perf counters, and"), "{err}");
	}

	// Without `-p`, the base leaves out the feature of a package it lacks.
	let out = compare(&["--workspace", "--features", "b/f"]);
	let err = String::from_utf8_lossy(&out.stderr);
	assert!(out.status.success(), "{err}");
	assert!(err.contains("HEAD has no feature b/f"), "{err}");
	let table = String::from_utf8(out.stdout).unwrap();
	assert!(!row(&table, "a").contains(" - "), "{table}");
	assert!(row(&table, "b").ends_with("| added |"), "{table}");

	// Beside `--benches`, a target the base lacks leaves it every other target.
	let out = compare(&["-p", "a", "-p", "b", "--bench", "y", "--benches", "-F", "b/f"]);
	let err = String::from_utf8_lossy(&out.stderr);
	assert!(out.status.success(), "{err}");
	let table = String::from_utf8(out.stdout).unwrap();
	assert!(!row(&table, "a").contains(" - "), "{table}");
	assert!(row(&table, "b").ends_with("| added |"), "{table}");

	// A selector neither side has still fails.
	let out = compare(&["-p", "a", "-p", "c"]);
	let err = String::from_utf8_lossy(&out.stderr);
	assert!(!out.status.success());
	assert!(err.contains("neither HEAD nor the working tree has -p c"), "{err}");
	// Even beside a manifest only one side has.
	let out = compare(&["--manifest-path", "b/Cargo.toml", "-p", "b", "-p", "c"]);
	let err = String::from_utf8_lossy(&out.stderr);
	assert!(!out.status.success());
	assert!(err.contains("neither HEAD nor the working tree has -p c"), "{err}");
}

#[cfg(target_env = "gnu")]
#[test]
fn detects_c11_threads_and_uncounted_or_late_workers() {
	let dir = scratch("thread-watch");
	let repo = dir.join("repo");
	std::fs::create_dir_all(repo.join("benches")).unwrap();
	let harness = Path::new(env!("CARGO_MANIFEST_DIR")).join("../swole");
	std::fs::write(
		repo.join("Cargo.toml"),
		format!(
			"[package]\nname = \"threads\"\nversion = \"0.0.0\"\nedition = \"2024\"\npublish = false\n\n[workspace]\n\n\
			 [dev-dependencies]\nswole = {{ path = {harness:?} }}\n\n[[bench]]\nname = \"workers\"\nharness = false\n"
		),
	)
	.unwrap();
	std::fs::copy(
		Path::new(env!("CARGO_MANIFEST_DIR")).join("../../Cargo.lock"),
		repo.join("Cargo.lock"),
	)
	.unwrap();
	std::fs::write(
		repo.join("benches/workers.rs"),
		r#"use std::ffi::{c_int, c_void};
use std::sync::atomic::{AtomicU64, Ordering};
use swole::bench;

unsafe extern "C" {
	fn thrd_create(thr: *mut u64, func: extern "C" fn(*mut c_void) -> c_int, arg: *mut c_void) -> c_int;
	fn thrd_join(thr: u64, res: *mut c_int) -> c_int;
}

extern "C" fn noop(_: *mut c_void) -> c_int { 0 }

#[bench]
fn c11_worker() {
	let mut thr = 0u64;
	let mut res = 0;
	unsafe {
		assert_eq!(thrd_create(&mut thr, noop, std::ptr::null_mut()), 0);
		assert_eq!(thrd_join(thr, &mut res), 0);
	}
}

/// Spawns and immediately joins a short-lived worker only on the 10th call. Each call sleeps 2ms
/// so bench calibration settles on 1 iteration (meaning the counter process runs only 3 calls and
/// never reaches call 10, while timed samples run at least 12 calls and must catch the worker via
/// the thread-start counter even though it exits before the sample ends).
#[bench]
fn late_worker() -> u64 {
	static CALLS: AtomicU64 = AtomicU64::new(0);
	if CALLS.fetch_add(1, Ordering::Relaxed) == 10 {
		std::thread::spawn(|| {}).join().unwrap();
	}
	std::thread::sleep(std::time::Duration::from_millis(2));
	std::hint::black_box(1)
}

fn main() { swole::main(); }
"#,
	)
	.unwrap();

	let run_swole = |args: &[&str]| {
		Command::new(env!("CARGO_BIN_EXE_swole"))
			.args(args)
			.current_dir(&repo)
			.env("XDG_CACHE_HOME", dir.join("cache"))
			.env("CARGO_TARGET_DIR", dir.join("target"))
			.env("CARGO_NET_OFFLINE", "true")
			.output()
			.unwrap()
	};

	let list = dir.join("c11");
	std::fs::write(&list, "benches/workers.rs::c11_worker\n").unwrap();
	let out = run_swole(&["quick", "--benchmarks", list.to_str().unwrap()]);
	let err = String::from_utf8_lossy(&out.stderr);
	assert!(!out.status.success(), "{err}");
	if err.contains("quick counts with perf counters, and") {
		return;
	}
	assert!(
		err.contains("benches/workers.rs::c11_worker: another thread ran beside a counted call"),
		"{err}"
	);

	let late = dir.join("late");
	std::fs::write(&late, "benches/workers.rs::late_worker\n").unwrap();
	let report_path = dir.join("report.json");
	let out = run_swole(&[
		"bench",
		"--benchmarks",
		late.to_str().unwrap(),
		"--report",
		report_path.to_str().unwrap(),
	]);
	let err = String::from_utf8_lossy(&out.stderr);
	assert!(out.status.success(), "{err}");
	let report: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(&report_path).unwrap()).unwrap();
	let host = report["host"].as_array().unwrap();
	assert!(
		host.iter().any(|line| line
			.as_str()
			.is_some_and(|line| line.starts_with("libc: ") && line.contains("build-id "))),
		"{host:?}"
	);
	let entry = &report["benchmarks"]["benches/workers.rs::late_worker"];
	assert_eq!(entry["threaded"], true, "{entry}");
	assert!(entry["head"].get("instructions:u").is_none(), "{entry}");
}
