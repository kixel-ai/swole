//! swole's wall-clock times end to end: the harness's fixture timed alone, and a scratch
//! repository's benchmarks paired with its base commit's. Times move with the host's load, so
//! these check what a time is, and only a difference far past any load's.

#![cfg(unix)]

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use serde_json::Value;

fn scratch(name: &str) -> PathBuf {
	let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join(name);
	let _ = std::fs::remove_dir_all(&dir);
	std::fs::create_dir_all(&dir).unwrap();
	dir
}

fn succeeded(out: &Output) {
	assert!(
		out.status.success(),
		"{}\n{}",
		String::from_utf8_lossy(&out.stdout),
		String::from_utf8_lossy(&out.stderr)
	);
}

/// The time a `metrics.json` records, beside each benchmark's allocations where the host counts
/// them, and the file.
fn read(path: &Path) -> (String, Value) {
	let metrics: Value = serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
	let units = metrics["units"].as_object().unwrap();
	let (measure, unit) = units.iter().find(|(name, _)| name.starts_with("time (")).unwrap();
	let others: Vec<&str> = units
		.keys()
		.filter(|name| *name != measure)
		.map(|name| name.split(" (").next().unwrap())
		.collect();
	assert!(
		others.is_empty() || others == ["alloc_bytes", "alloc_count", "peak_live_bytes"],
		"{others:?}"
	);
	assert_eq!(unit, "ns");
	assert!(
		measure.starts_with(&format!("time ({}-", std::env::consts::ARCH)),
		"{measure}"
	);
	(measure.clone(), metrics)
}

/// Each benchmark's time in a sample, a bare value: a time of one side has no interval.
fn times(sample: &Value, measure: &str) -> Vec<(String, f64)> {
	sample["metrics"]
		.as_object()
		.unwrap()
		.iter()
		.map(|(name, measures)| {
			let time = measures[measure].as_object().unwrap();
			assert_eq!(time.len(), 1, "{name}: {time:?}");
			let value = time["value"].as_f64().unwrap();
			assert!(value > 0.0, "{name}: {value}");
			(name.clone(), value)
		})
		.collect()
}

/// swole run in this workspace with `args` on the harness's fixture benchmark target.
fn fixture(args: &[&std::ffi::OsStr]) -> Output {
	Command::new(env!("CARGO_BIN_EXE_swole"))
		.arg("bench")
		.args(args)
		.args(["--", "-p", "swole", "--bench", "fixture"])
		.current_dir(env!("CARGO_MANIFEST_DIR"))
		// The target directory the count tests build the fixture in, apart from this test's own.
		.env(
			"CARGO_TARGET_DIR",
			Path::new(env!("CARGO_TARGET_TMPDIR")).join("harness-target"),
		)
		.output()
		.unwrap()
}

/// The fixture's benchmarks that run another thread, which counters every thread inherits count,
/// sampled, rather than ones read in user space.
const THREADED: [&str; 3] = ["handed", "spawned[100000]", "spawned[1000]"];

#[test]
fn times_each_harness_benchmark() {
	let dir = scratch("time-alone");
	let file = "crates/swole/benches/fixture.rs::";
	let out = fixture(&[
		"--out".as_ref(),
		dir.join("metrics.json").as_os_str(),
		"--report".as_ref(),
		dir.join("report.json").as_os_str(),
		"--all-details".as_ref(),
	]);
	succeeded(&out);
	let (measure, metrics) = read(&dir.join("metrics.json"));
	let samples = metrics["samples"].as_array().unwrap();
	assert_eq!(samples.len(), 1);
	assert!(samples[0].get("commit").is_none());
	let times = times(&samples[0], &measure);
	let names: Vec<&str> = times.iter().map(|(name, _)| name.strip_prefix(file).unwrap()).collect();
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
	let time = |name: &str| times.iter().find(|(n, _)| n == &format!("{file}{name}")).unwrap().1;
	// Eight times the spins, which no load makes look like under twice.
	assert!(time("unstable[8000]") > 2.0 * time("unstable[1000]"), "{times:?}");
	let table = String::from_utf8(out.stdout).unwrap();
	assert!(table.starts_with("| benchmark | time |\n"), "{table}");
	assert_eq!(
		table.lines().take_while(|line| !line.is_empty()).count(),
		2 + names.len(),
		"{table}"
	);
	// Beside each time, what the process used over its samples on any host, and where the host
	// has perf counters, each one's share of a call.
	let report: Value = serde_json::from_str(&std::fs::read_to_string(dir.join("report.json")).unwrap()).unwrap();
	assert_eq!(report["mode"], "bench");
	assert!(measure.contains(report["fingerprint"].as_str().unwrap()), "{measure}");
	let head = |name: &str| report["benchmarks"][format!("{file}{name}")]["head"].clone();
	for name in &names {
		let head = head(name);
		assert_eq!(head["time"]["unit"], "ns", "{name}: {head}");
		assert!(head["max RSS"]["value"].as_f64().unwrap() > 0.0, "{name}: {head}");
		// Sampled within the least and most one sample used, as the tick splits it from system time.
		assert!(head["user time"]["lower"].is_number(), "{name}: {head}");
		assert!(
			head["CPUs busy"].is_null() || head["CPUs busy"]["unit"] == "CPUs",
			"{name}: {head}"
		);
	}
	// How busy a sample kept the CPUs only where one clock times the whole sample: a step between
	// calls, or a benchmark's own timer, leaves the sample wider than its time.
	for name in ["stepped", "timed"] {
		assert!(head(name)["CPUs busy"].is_null(), "{name}: {}", head(name));
	}
	// A benchmark that sleeps through its calls waits off the CPU, and one that spins does not.
	let busy = |name: &str| head(name)["CPUs busy"]["value"].as_f64().unwrap();
	assert!(busy("slept") < 0.5, "{}", busy("slept"));
	assert!(busy("unstable[8000]") > busy("slept"), "{}", busy("unstable[8000]"));
	assert_eq!(report["schema_version"], 1);
	// A benchmark that declares what each call processes reports how many it processes a second,
	// from its time.
	for (name, unit, amount) in [("sized[2000]", "bytes/s", 2000.0), ("alone", "items/s", 1000.0)] {
		let head = head(name);
		assert_eq!(head["throughput"]["unit"], unit, "{name}: {head}");
		let rate = head["throughput"]["value"].as_f64().unwrap();
		let time = head["time"]["value"].as_f64().unwrap();
		assert!((rate * time / (amount * 1e9) - 1.0).abs() < 1e-3, "{name}: {head}");
	}
	assert!(head("borrowed").get("throughput").is_none());
	assert!(
		table.contains("| crates/swole/benches/fixture.rs::sized[2000] | throughput | "),
		"{table}"
	);
	// Each one's allocations, on a host with the interposer: none in a loop alone, exactly, and a
	// sample's beside another thread.
	if cfg!(any(
		all(target_os = "linux", target_arch = "x86_64"),
		target_os = "macos"
	)) {
		assert_eq!(head("alone")["alloc_count"]["value"], 0);
		assert_eq!(head("alone")["alloc_count"]["sampled"], false);
		for name in THREADED {
			assert_eq!(head(name)["alloc_bytes"]["sampled"], true, "{name}");
		}
	}
	let host: Vec<&str> = report["host"]
		.as_array()
		.unwrap()
		.iter()
		.map(|line| line.as_str().unwrap())
		.collect();
	if host.iter().any(|line| line.starts_with("perf: tier")) {
		// A benchmark beside which another thread ran counts every thread, the other's spins among
		// them, each counter the median of several counts with their least and most, and the note
		// under the table names it.
		for name in THREADED {
			let head = head(name);
			let instructions = &head["instructions:u"];
			let (value, lower, upper) = (
				instructions["value"].as_f64().unwrap(),
				instructions["lower"].as_f64().unwrap(),
				instructions["upper"].as_f64().unwrap(),
			);
			assert!(lower <= value && value <= upper, "{name}: {head}");
			assert_eq!(report["benchmarks"][format!("{file}{name}")]["threaded"], true);
		}
		for name in ["handed", "spawned[100000]"] {
			let instructions = head(name)["instructions:u"]["value"].as_f64().unwrap();
			assert!(instructions > 400_000.0, "{name}: {instructions}");
		}
		assert!(table.contains(&format!(
			"\nOther threads ran beside {}, so bench counted every thread",
			THREADED.map(|name| format!("{file}{name}")).join(", ")
		)));
		assert!(head("alone")["instructions:u"].get("lower").is_none());
		// A call of `alone` spins a thousand times, some four thousand instructions, as Valgrind
		// counts it, and as many calls of a counter pass share them.
		let instructions = head("alone")["instructions:u"]["value"].as_f64().unwrap();
		assert!((3600.0..4400.0).contains(&instructions), "{instructions}");
		let unstable = head("unstable[8000]")["instructions:u"]["value"].as_f64().unwrap();
		assert!(
			(7.5..8.5).contains(&(unstable / instructions)),
			"{unstable} {instructions}"
		);
		assert!(head("alone")["IPC:u"]["value"].as_f64().unwrap() > 0.0);
		// What derives from the counters: misses a thousand instructions, the clock against its
		// reference where the PMU counts one, else an estimate from user time, and the top-down
		// breakdown's first level, which divides every slot, where the PMU breaks it down.
		let alone = head("alone");
		// Whether one pass, one group, counts every one of `events`.
		let pass = |events: &[&str]| {
			host.iter().any(|line| {
				line.strip_prefix("pass ")
					.and_then(|line| line.split_once(": "))
					.is_some_and(|(_, names)| events.iter().all(|event| names.split(' ').any(|name| name == *event)))
			})
		};
		if pass(&["branch-misses:u"]) {
			assert_eq!(alone["branch MPKI:u"]["unit"], "MPKI", "{alone}");
		}
		if pass(&["branch-misses:u"]) && pass(&["branches:u"]) {
			assert!(
				alone["branch miss rate:u"]["value"].as_f64().unwrap() < 100.0,
				"{alone}"
			);
		}
		if pass(&["cycles:u", "ref-cycles:u"]) {
			let clock = alone["cycles/ref-cycles:u"]["value"].as_f64().unwrap();
			assert!((0.1..10.0).contains(&clock), "{alone}");
			assert!(alone.get("estimated clock:u").is_none(), "{alone}");
		} else if pass(&["cycles:u"]) {
			assert_eq!(alone["estimated clock:u"]["unit"], "GHz", "{alone}");
		}
		if pass(&["slots:u"]) || pass(&["topdown-retiring:u"]) {
			// A call eight times `alone`'s, whose window's edges, where each event of a group is
			// read a few cycles apart, move its shares a tenth as much: up to 1% on `alone`'s.
			let long = head("unstable[8000]");
			let first = [
				"retiring",
				"bad speculation",
				"frontend bound",
				"backend bound",
				"SMT contention",
			];
			let total: f64 = first
				.iter()
				.filter_map(|share| long[format!("topdown {share}")]["value"].as_f64())
				.sum();
			// The SMT sibling's share, which a PMU a counter short leaves out, is all that may be
			// missing, and with it as much as the sibling took.
			assert!(
				first[..4]
					.iter()
					.all(|share| long.get(format!("topdown {share}")).is_some()),
				"{long}"
			);
			// AMD's sibling share, which no pass counts on a PMU a counter short.
			let smt = "de_no_dispatch_per_slot.smt_contention:u";
			let amd = host.iter().any(|line| line.contains("de_no_dispatch_per_slot"));
			let least = if amd && !pass(&[smt]) { 0.0 } else { 90.0 };
			assert!((least..=100.5).contains(&total), "{total}: {long}");
		}
		// And each counter per byte, cycles among them where the host counts them.
		let sized = head("sized[2000]");
		let per_byte = sized["instructions:u/byte"]["value"].as_f64().unwrap();
		let instructions = sized["instructions:u"]["value"].as_f64().unwrap();
		assert!((per_byte * 2000.0 / instructions - 1.0).abs() < 1e-3, "{sized}");
		if let Some(cycles) = sized["cycles:u"]["value"].as_f64() {
			let per_byte = sized["cycles:u/byte"]["value"].as_f64().unwrap();
			assert!((per_byte * 2000.0 / cycles - 1.0).abs() < 1e-3, "{sized}");
		}
		// A drop to run after each call changes nothing counted: each count, of each side,
		// starts and stops around every call alike.
		let alone = head("alone")["instructions:u"]["value"].as_f64().unwrap();
		let dropped = head("dropped")["instructions:u"]["value"].as_f64().unwrap();
		assert!((dropped - alone).abs() < 16.0, "{dropped} {alone}");
		// The counters are read in user space, so a count holds none of the kernel's work to
		// start and stop them, some 18,000 instructions a call: only interrupts', a few a call.
		// A benchmark that times itself counts as one called each time does, but for the
		// reads' own fixed few dozen instructions a call.
		if let Some(kernel) = head("alone")["instructions:k"]["value"].as_f64() {
			assert!(kernel < 1000.0, "{kernel}");
			let timed = head("timed")["instructions:u"]["value"].as_f64().unwrap();
			assert!((alone - timed).abs() < 300.0, "{alone} {timed}");
		}
	} else {
		assert!(head("alone").get("instructions:u").is_none());
	}
}

/// A benchmark target spinning `work` times in `work`, a thousand in `same`, and with `extra`,
/// a benchmark of that name.
fn spins(work: u64, extra: &str) -> String {
	format!(
		"use std::hint::black_box;\n\
		 use swole::bench;\n\
		 fn spin(n: u64) -> u64 {{ let mut sum = 0u64; for i in 0..n {{ sum = black_box(sum.wrapping_add(i)); }} sum }}\n\
		 #[bench] fn work() -> u64 {{ spin({work}) }}\n\
		 #[bench] fn same() -> u64 {{ spin(1000) }}\n\
		 #[bench] fn {extra}() -> u64 {{ spin(1000) }}\n\
		 fn main() {{ swole::main(); }}\n"
	)
}

fn git(dir: &Path, args: &[&str]) -> String {
	let out = Command::new("git").args(args).current_dir(dir).output().unwrap();
	succeeded(&out);
	String::from_utf8(out.stdout).unwrap().trim().to_owned()
}

#[test]
fn pairs_a_base_commit_with_the_working_tree() {
	let dir = scratch("time-paired");
	let repo = dir.join("repo");
	std::fs::create_dir_all(repo.join("benches")).unwrap();
	let harness = Path::new(env!("CARGO_MANIFEST_DIR")).join("../swole");
	std::fs::write(
		repo.join("Cargo.toml"),
		format!(
			"[package]\nname = \"paired\"\nversion = \"0.0.0\"\nedition = \"2024\"\npublish = false\n\n[workspace]\n\n\
			 [dev-dependencies]\nswole = {{ path = {:?} }}\n\n[[bench]]\nname = \"spin\"\nharness = false\n",
			harness.to_str().unwrap()
		),
	)
	.unwrap();
	// The runner's own lock, so the scratch workspace resolves the harness's dependencies offline.
	std::fs::copy(
		Path::new(env!("CARGO_MANIFEST_DIR")).join("../../Cargo.lock"),
		repo.join("Cargo.lock"),
	)
	.unwrap();
	std::fs::write(repo.join("benches/spin.rs"), spins(1000, "gone")).unwrap();
	std::fs::write(repo.join(".gitignore"), "/target\n").unwrap();
	git(&repo, &["init", "--quiet"]);
	git(&repo, &["add", "."]);
	git(
		&repo,
		&[
			"-c",
			"user.name=t",
			"-c",
			"user.email=t@t",
			"-c",
			"commit.gpgsign=false",
			"commit",
			"--quiet",
			"-m",
			"base",
		],
	);
	let base = git(&repo, &["rev-parse", "HEAD"]);
	// The working tree spins twice as long in `work`, and trades `gone` for `new`.
	std::fs::write(repo.join("benches/spin.rs"), spins(2000, "new")).unwrap();

	let out = Command::new(env!("CARGO_BIN_EXE_swole"))
		.args(["bench", "HEAD", "--out", "metrics.json"])
		.current_dir(&repo)
		// The base's worktree, outside the scratch repository, and one target directory both
		// sides share, so the harness builds once.
		.env("XDG_CACHE_HOME", dir.join("cache"))
		.env("CARGO_TARGET_DIR", dir.join("target"))
		.env("CARGO_NET_OFFLINE", "true")
		.output()
		.unwrap();
	succeeded(&out);
	let (measure, metrics) = read(&repo.join("metrics.json"));
	let samples = metrics["samples"].as_array().unwrap();
	assert_eq!(samples.len(), 2);
	assert_eq!(samples[0]["commit"], base);
	assert!(samples[1].get("commit").is_none());
	let (before, after) = (times(&samples[0], &measure), times(&samples[1], &measure));
	let names = |times: &[(String, f64)]| times.iter().map(|(name, _)| name.clone()).collect::<Vec<_>>();
	assert_eq!(
		names(&before),
		[
			"benches/spin.rs::gone",
			"benches/spin.rs::same",
			"benches/spin.rs::work"
		]
	);
	assert_eq!(
		names(&after),
		["benches/spin.rs::new", "benches/spin.rs::same", "benches/spin.rs::work"]
	);
	// Twice the spins, which no load makes look like under half again.
	assert!(after[2].1 > 1.5 * before[2].1, "{before:?} {after:?}");
	let table = String::from_utf8(out.stdout).unwrap();
	let row = |name: &str| {
		table
			.lines()
			.find(|line| line.starts_with(&format!("| benches/spin.rs::{name} |")))
			.unwrap_or_else(|| panic!("{table}"))
			.to_owned()
	};
	assert!(row("gone").contains("| removed |"), "{table}");
	assert!(row("new").contains("| added |"), "{table}");
	// `+NN.NN%`, then its interval.
	let change: f64 = row("work")
		.split('|')
		.nth(4)
		.unwrap()
		.trim()
		.trim_end_matches('%')
		.parse()
		.unwrap();
	assert!(change > 50.0, "{table}");
	// On a host that samples precise cycles, the function whose spins doubled leads those behind
	// the change, inlined though it is, by the symbols and line tables bench builds with.
	if String::from_utf8_lossy(&out.stderr).contains("swole: sampling each changed benchmark with cycles:pp") {
		let first = table
			.lines()
			.find(|line| line.starts_with("| benches/spin.rs::work | cycles |"))
			.unwrap_or_else(|| panic!("{table}"));
		assert!(first.contains("| `spin::spin` |"), "{table}");
	}
}

/// Writes a workspace package `name` under `repo` with one harness benchmark target, `bench`.
fn package(repo: &Path, name: &str, bench: &str) {
	let dir = repo.join(name);
	std::fs::create_dir_all(dir.join("benches")).unwrap();
	std::fs::create_dir_all(dir.join("src")).unwrap();
	let harness = Path::new(env!("CARGO_MANIFEST_DIR")).join("../swole");
	std::fs::write(
		dir.join("Cargo.toml"),
		format!(
			"[package]\nname = \"{name}\"\nedition = \"2024\"\npublish = false\n\n\
			 [dev-dependencies]\nswole = {{ path = {harness:?} }}\n\n\
			 [[bench]]\nname = \"{bench}\"\nharness = false\n"
		),
	)
	.unwrap();
	std::fs::write(dir.join("src/lib.rs"), "").unwrap();
	std::fs::write(dir.join("benches").join(format!("{bench}.rs")), spins(1000, "other")).unwrap();
}

#[test]
fn times_a_selection_only_one_side_has() {
	let dir = scratch("time-select");
	let repo = dir.join("repo");
	std::fs::create_dir_all(&repo).unwrap();
	std::fs::copy(
		Path::new(env!("CARGO_MANIFEST_DIR")).join("../../Cargo.lock"),
		repo.join("Cargo.lock"),
	)
	.unwrap();
	std::fs::write(repo.join(".gitignore"), "/target\n").unwrap();
	std::fs::write(
		repo.join("Cargo.toml"),
		"[workspace]\nmembers = [\"a\"]\nresolver = \"3\"\n",
	)
	.unwrap();
	package(&repo, "a", "x");
	git(&repo, &["init", "--quiet"]);
	git(&repo, &["add", "."]);
	git(
		&repo,
		&[
			"-c",
			"user.name=t",
			"-c",
			"user.email=t@t",
			"-c",
			"commit.gpgsign=false",
			"commit",
			"--quiet",
			"-m",
			"base",
		],
	);
	// The head adds a package, which a run names before committing it.
	std::fs::write(
		repo.join("Cargo.toml"),
		"[workspace]\nmembers = [\"a\", \"b\"]\nresolver = \"3\"\n",
	)
	.unwrap();
	package(&repo, "b", "y");
	let time = |cargo: &[&str]| {
		Command::new(env!("CARGO_BIN_EXE_swole"))
			.args(["bench", "HEAD", "--"])
			.args(cargo)
			.current_dir(&repo)
			.env("XDG_CACHE_HOME", dir.join("cache"))
			.env("CARGO_TARGET_DIR", dir.join("target"))
			.env("CARGO_NET_OFFLINE", "true")
			.output()
			.unwrap()
	};

	let out = time(&["-p", "a", "--package=b", "--bench", "x", "--bench=y"]);
	let err = String::from_utf8_lossy(&out.stderr).into_owned();
	assert!(out.status.success(), "{err}");
	assert!(err.contains("HEAD has no -p b"), "{err}");
	assert!(err.contains("HEAD has no --bench y"), "{err}");
	let stdout = String::from_utf8(out.stdout).unwrap();
	// The times' table, before what else each benchmark measured.
	let table = stdout.split("\n\n").next().unwrap();
	// Each package's three benchmarks, `b`'s only at the head.
	assert_eq!(table.lines().count(), 2 + 6, "{table}");
	for row in table.lines().skip(2) {
		assert_eq!(
			row.contains("| added |"),
			row.starts_with("| b/benches/y.rs::"),
			"{table}"
		);
	}

	// The head's package alone: the base times nothing, rather than every benchmark.
	let out = time(&["-p", "b"]);
	let err = String::from_utf8_lossy(&out.stderr).into_owned();
	assert!(out.status.success(), "{err}");
	assert!(
		err.contains("HEAD has nothing the cargo args select, so times none"),
		"{err}"
	);
	let stdout = String::from_utf8(out.stdout).unwrap();
	let table = stdout.split("\n\n").next().unwrap();
	assert_eq!(table.matches("| added |").count(), 3, "{table}");
	assert_eq!(table.lines().count(), 2 + 3, "{table}");

	// A selector neither side has still fails, before either side builds.
	let out = time(&["-p", "a", "-p", "c"]);
	let err = String::from_utf8_lossy(&out.stderr);
	assert!(!out.status.success());
	assert!(err.contains("neither HEAD nor the working tree has -p c"), "{err}");
	assert!(!err.contains("building"), "{err}");
}
