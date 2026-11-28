//! swole: counts each benchmark's instructions natively with Linux perf counters (`quick`) or
//! under stock Valgrind, simulating its caches and branches (`sim`), one process a job, in parallel, and times each by wall clock with
//! every counter the host allows (`bench`), alone or beside a base commit's.

mod attribution;
mod bench;
mod callgrind;
mod cargo;
mod compare;
#[cfg(target_os = "linux")]
mod cores;
mod diagnose;
/// No host but Linux leases a core.
#[cfg(not(target_os = "linux"))]
mod cores {
	pub enum Reservation {}

	pub struct Spread {
		pub cpus: Vec<usize>,
	}

	impl Reservation {
		pub fn cpu(&self) -> usize {
			match *self {}
		}

		pub fn governor(&self) -> &str {
			match *self {}
		}

		pub fn spread(&self) -> Result<Spread, String> {
			match *self {}
		}

		pub fn describe(&self) -> String {
			match *self {}
		}
	}

	pub fn reserve(_: Option<&crate::perf::Pmu>) -> Result<Option<Reservation>, String> {
		Ok(None)
	}
}
mod fingerprint;
mod heap;
mod metrics;
#[cfg_attr(
	not(target_os = "linux"),
	allow(dead_code, reason = "elsewhere it only says the host has no perf counters")
)]
mod perf;
mod preload;
mod profile;

use std::collections::{BTreeMap, BTreeSet};
use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::Instant;

use fingerprint::{Fingerprint, Native};

/// The hidden argument that runs this executable as the fingerprint's CPU probe.
const PROBE: &str = "__cpu";

const USAGE: &str = "\
usage: swole quick [<base>] [options] [-- cargo bench args...]
       swole sim [<base>] [options] [-- cargo bench args...]
       swole bench [<base>] [options] [-- cargo bench args...]
       swole fingerprint quick|sim|bench

quick counts each benchmark once, natively, with this host's perf counters; sim counts each
under Valgrind, the same on any host that shares its fingerprint; bench times each by wall clock,
with every perf counter this host allows. With <base>, a commit, each measures <base> too, in
a worktree of its own under ~/.cache/swole, and prints each change.

Benchmarks are the Cargo workspace's, written with swole's harness, or for sim, CodSpeed's
compatibility crates, unless --program names them.

options:
  --benchmarks <file>  measure only the benchmarks it names, one a line; any it names that no
                       benchmark has fails the run
  --allow-missing      with --benchmarks, report each name no benchmark has and measure the
                       rest, failing only when none is there, as a bisect step needs (no <base>)
  --out <file>         write the headline measures as Kixel Quest's metrics.json: instructions
                       for quick, instructions and estimated cycles for sim, time for bench,
                       each beside its heap allocations (sim and quick: no <base>)
  --report <file>      write every measure of every benchmark as JSON
  --all-details        print every measure of every benchmark, not only those that moved in a
                       suite of more than 10 (without a base, none moved)
  --jobs <n>           how many jobs run at once (default: every core, or for quick, every core
                       of the PMU it counts on; bench runs one at a time)
  --build <command>    a shell command that builds the programs, run first (sim, with --program)
  --program <command>  a shell command that runs a program whose benchmarks run through
                       swole or CodSpeed's instrument hooks; repeat for each (sim)";

/// Runs swole's command line, as either of its binaries.
pub fn main() -> ExitCode {
	let mut args: Vec<String> = std::env::args().skip(1).collect();
	// Cargo runs `cargo swole <args>` as `cargo-swole swole <args>`.
	if args.first().map(String::as_str) == Some("swole") {
		args.remove(0);
	}
	if args.first().map(String::as_str) == Some(PROBE) {
		print!("{}", fingerprint::cpu());
		return ExitCode::SUCCESS;
	}
	if args.first().map(String::as_str) == Some(cargo::CAPTURE) {
		return match cargo::capture(&args[1..]) {
			Ok(()) => ExitCode::SUCCESS,
			Err(err) => {
				eprintln!("swole: {err}");
				ExitCode::FAILURE
			}
		};
	}
	match perf::drop_ambient().and_then(|()| cli(&args)) {
		Ok(()) => ExitCode::SUCCESS,
		Err(err) => {
			eprintln!("swole: {err}");
			ExitCode::FAILURE
		}
	}
}

/// How a run measures each benchmark.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum Mode {
	/// Once, natively, with perf counters.
	Quick,
	/// Under Valgrind.
	Sim,
	/// By wall clock, with every perf counter the host allows.
	Bench,
}

impl Mode {
	fn parse(name: &str) -> Option<Self> {
		match name {
			"quick" => Some(Self::Quick),
			"sim" => Some(Self::Sim),
			"bench" => Some(Self::Bench),
			_ => None,
		}
	}

	pub(crate) fn name(self) -> &'static str {
		match self {
			Self::Quick => "quick",
			Self::Sim => "sim",
			Self::Bench => "bench",
		}
	}
}

fn cli(args: &[String]) -> Result<(), String> {
	let (command, rest) = args.split_first().ok_or(USAGE)?;
	if let Some(mode) = Mode::parse(command) {
		// A base is a commit, and no commit's name starts with a dash.
		let (base, rest) = match rest.split_first() {
			Some((base, rest)) if !base.starts_with('-') => (Some(base.as_str()), rest),
			_ => (None, rest),
		};
		let options = Options::parse(rest, mode, base.is_some())?;
		return match (mode, base) {
			(Mode::Bench, base) => bench::bench(base, &options),
			(_, Some(base)) => compare::compare(base, &options, mode),
			(_, None) => {
				let counter = Counter::new(mode)?;
				let counts = measure(Path::new("."), &options, &counter, true)?;
				if mode == Mode::Quick {
					eprintln!("swole: fingerprint {}", counts.fingerprint);
				}
				print!("{}", metrics::table(&counts));
				print!("{}", threaded(&counts));
				let pick = metrics::Pick::new(
					counts.counts.keys().map(|name| (name.clone(), false)).collect(),
					options.all_details,
					false,
				);
				print!("{}", metrics::details(None, &counts.shown(), &BTreeMap::new(), &pick));
				if let Some(out) = &options.out {
					metrics::File::counts(&counts, &counter)?.write(out)?;
					eprintln!("swole: wrote {}", out.display());
				}
				if let Some(report) = &options.report {
					metrics::report(&counts, None, &counter, None).write(report)?;
					eprintln!("swole: wrote {}", report.display());
				}
				Ok(())
			}
		};
	}
	match command.as_str() {
		"fingerprint" => {
			let [mode] = rest else {
				return Err(USAGE.to_owned());
			};
			let mode = Mode::parse(mode).ok_or(USAGE)?;
			match mode {
				Mode::Sim => {
					let fingerprint = Fingerprint::probe()?;
					println!("{}", fingerprint.label());
					eprint!("{}", fingerprint.describe());
				}
				Mode::Quick | Mode::Bench => {
					let native = Native::probe(mode)?;
					if mode == Mode::Quick {
						native.host.perf.as_ref().map_err(|why| quick_needs(why))?;
					}
					println!("{}", native.label());
					eprint!("{}", native.describe());
					if cfg!(target_os = "linux") {
						eprintln!(
							"note: host probe uses swole's own libc; quick and bench fingerprint each benchmark's \
							 mapped libc"
						);
					}
				}
			}
			Ok(())
		}
		"-h" | "--help" | "help" => {
			println!("{USAGE}");
			Ok(())
		}
		_ => Err(USAGE.to_owned()),
	}
}

/// Why a quick count cannot run on a host without perf counters, and what can.
fn quick_needs(why: &str) -> String {
	format!("quick counts with perf counters, and {why}: count under Valgrind with swole sim, or time with swole bench")
}

/// What to measure, and how.
#[derive(Clone)]
pub(crate) struct Options {
	/// The only benchmarks to measure, when given.
	benchmarks: Option<BTreeSet<String>>,
	/// Whether a wanted benchmark no benchmark is gets reported rather than failing the run.
	allow_missing: bool,
	out: Option<PathBuf>,
	report: Option<PathBuf>,
	/// Whether stdout prints every measure of every benchmark, rather than only what moved.
	all_details: bool,
	/// How many jobs run at once, when given.
	jobs: Option<usize>,
	build: Option<String>,
	programs: Vec<String>,
	cargo: Vec<String>,
}

impl Options {
	fn parse(args: &[String], mode: Mode, base: bool) -> Result<Self, String> {
		let mut options = Options {
			benchmarks: None,
			allow_missing: false,
			out: None,
			report: None,
			all_details: false,
			jobs: None,
			build: None,
			programs: Vec::new(),
			cargo: Vec::new(),
		};
		let mut args = args.iter();
		while let Some(arg) = args.next() {
			let mut value = || args.next().ok_or(format!("{arg} takes a value\n\n{USAGE}"));
			match arg.as_str() {
				"--benchmarks" => {
					let path = value()?;
					let text = std::fs::read_to_string(path).map_err(|e| format!("cannot read {path}: {e}"))?;
					options.benchmarks = Some(names(&text));
				}
				"--allow-missing" => options.allow_missing = true,
				"--out" => options.out = Some(PathBuf::from(value()?)),
				"--report" => options.report = Some(PathBuf::from(value()?)),
				"--all-details" => options.all_details = true,
				"--jobs" => {
					let jobs = value()?;
					options.jobs = Some(
						jobs.parse()
							.ok()
							.filter(|&n| n > 0)
							.ok_or(format!("--jobs {jobs} is not a count"))?,
					);
				}
				"--build" => options.build = Some(value()?.clone()),
				"--program" => options.programs.push(value()?.clone()),
				"--" => options.cargo = args.by_ref().cloned().collect(),
				_ => return Err(format!("unknown argument {arg}\n\n{USAGE}")),
			}
		}
		let name = mode.name();
		if options.allow_missing && options.benchmarks.is_none() {
			return Err("--allow-missing tolerates names --benchmarks lists: give it one".to_owned());
		}
		if options.programs.is_empty() && options.build.is_some() {
			return Err("--build builds programs: name them with --program".to_owned());
		}
		if !options.programs.is_empty() && !options.cargo.is_empty() {
			return Err("cargo bench args are for a Cargo workspace's benchmarks, not --program's".to_owned());
		}
		if mode != Mode::Sim && !options.programs.is_empty() {
			return Err(format!(
				"{name} measures a Cargo workspace's benchmarks of swole's harness: --program is sim's"
			));
		}
		if mode == Mode::Bench && options.jobs.is_some() {
			return Err(
				"bench runs one benchmark at a time, so a time is never another's load: --jobs is quick's and sim's"
					.to_owned(),
			);
		}
		if base && mode != Mode::Bench && options.out.is_some() {
			return Err(format!("{name} <base> writes no metrics.json: --out is for one tree's"));
		}
		if base && options.allow_missing {
			return Err(
				"a compare already measures a name only one side has: --allow-missing is for one tree's".to_owned(),
			);
		}
		if mode == Mode::Bench && options.allow_missing {
			return Err("bench fails on any name --benchmarks lists that no benchmark has: --allow-missing is quick's and sim's".to_owned());
		}
		Ok(options)
	}
}

/// The benchmark names a list holds, one a line, ignoring blank lines.
fn names(text: &str) -> BTreeSet<String> {
	text.lines()
		.map(str::trim)
		.filter(|line| !line.is_empty())
		.map(str::to_owned)
		.collect()
}

/// One process to run: its command, and the one benchmark it must measure, when known.
pub(crate) struct Job {
	label: String,
	command: Command,
	expect: Option<String>,
	/// Whether it runs a benchmark of CodSpeed's crates, which only Valgrind counts.
	codspeed: bool,
}

impl Job {
	/// A new command that launches the job as its own does, so a job can run more than once.
	pub(crate) fn command(&self) -> Command {
		let mut command = Command::new(self.command.get_program());
		command.args(self.command.get_args());
		for (key, value) in self.command.get_envs() {
			match value {
				Some(value) => command.env(key, value),
				None => command.env_remove(key),
			};
		}
		if let Some(dir) = self.command.get_current_dir() {
			command.current_dir(dir);
		}
		command
	}
}

/// One measure of a benchmark beside its headline, in its unit.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Measure {
	pub name: String,
	/// The median of its samples, or its one value.
	pub value: f64,
	pub unit: &'static str,
	/// The least and most of its samples, when it took several, which marks it sampled.
	pub spread: Option<(f64, f64)>,
}

impl Measure {
	pub(crate) fn new(name: &str, value: f64, unit: &'static str) -> Self {
		Self {
			name: name.to_owned(),
			value,
			unit,
			spread: None,
		}
	}
}

/// Each benchmark's headline count under one fingerprint, and every other measure of it.
pub(crate) struct Counts {
	pub fingerprint: String,
	/// The fingerprint's inputs, a line each, reflecting the benchmark process's loaded libc.
	pub host: String,
	/// The median of a benchmark's samples, or its one count.
	pub counts: BTreeMap<String, u64>,
	/// The least and most of the samples of each benchmark counted more than once.
	pub spread: BTreeMap<String, (u64, u64)>,
	/// The benchmarks beside which the process had other threads.
	pub threaded: BTreeSet<String>,
	/// Every measure of each benchmark, the headline's among them, each the median of its samples
	/// with their least and most when it took several.
	pub details: BTreeMap<String, Vec<Measure>>,
	/// The self cost of each function in a benchmark's first run, from sim's Callgrind profiles.
	pub functions: BTreeMap<String, callgrind::Functions>,
	/// What each call processes, of each benchmark that declares it.
	pub throughput: BTreeMap<String, metrics::Throughput>,
}

impl Counts {
	/// None, labeled as `counter` labels its counts, so they compare with another side's.
	pub(crate) fn none(counter: &Counter) -> Self {
		Self {
			fingerprint: counter.fingerprint.clone(),
			host: counter.host.clone(),
			counts: BTreeMap::new(),
			spread: BTreeMap::new(),
			threaded: BTreeSet::new(),
			details: BTreeMap::new(),
			functions: BTreeMap::new(),
			throughput: BTreeMap::new(),
		}
	}

	/// Every measure of the benchmark `name`, when counted: its details, or its headline count
	/// alone, then what derives from them per what each call processes, and the ratios they give.
	pub(crate) fn measures(&self, name: &str) -> Option<Vec<Measure>> {
		let count = *self.counts.get(name)?;
		let mut measures = match self.details.get(name) {
			Some(details) => details.clone(),
			// Valgrind's instructions alone, with the spread of the count.
			None => vec![Measure {
				spread: self.spread.get(name).map(|&(least, most)| (least as f64, most as f64)),
				..Measure::new("instructions", count as f64, "instructions")
			}],
		};
		if let Some(throughput) = self.throughput.get(name) {
			measures.extend(throughput.measures(&measures));
		}
		let derived = diagnose::derive(&measures, false);
		measures.extend(derived);
		Some(measures)
	}

	/// Every measure of each counted benchmark, as a table shows them below the headline counts.
	pub(crate) fn shown(&self) -> BTreeMap<String, Vec<Measure>> {
		self.counts
			.keys()
			.filter_map(|name| Some((name.clone(), self.measures(name)?)))
			.collect()
	}
}

/// How many times a benchmark beside which the process had another thread counts, or under sim,
/// might have, since what the threads run while its call does depends on the scheduler: sim's
/// every count, and every pass of quick's or bench's counters, which every thread inherits.
pub(crate) const SAMPLES: usize = 5;

/// What counts each job, and the fingerprint its counts carry.
pub(crate) struct Counter {
	kind: CounterKind,
	pub fingerprint: String,
	/// The fingerprint's inputs, a line each.
	pub host: String,
}

enum CounterKind {
	Valgrind,
	Perf {
		native: Box<Native>,
		pmu: perf::Pmu,
		passes: Vec<perf::Pass>,
	},
}

impl Counter {
	/// The counter of `mode`, quick's or sim's, once it has probed the host, saying what it found.
	pub(crate) fn new(mode: Mode) -> Result<Self, String> {
		let counter = match mode {
			Mode::Sim => {
				let fingerprint = Fingerprint::probe()?;
				let counter = Self {
					kind: CounterKind::Valgrind,
					fingerprint: fingerprint.label(),
					host: fingerprint.describe(),
				};
				eprintln!("swole: fingerprint {}", counter.fingerprint);
				counter
			}
			Mode::Quick => {
				let native = Native::probe(mode)?;
				let perf = native.host.perf.as_ref().map_err(|why| quick_needs(why))?;
				eprint!("{}", indent(&native.host.describe()));
				let pmu = perf.pmu.clone();
				let passes = perf.passes.clone();
				Self {
					fingerprint: native.label(),
					host: native.describe(),
					kind: CounterKind::Perf {
						native: Box::new(native),
						pmu,
						passes,
					},
				}
			}
			Mode::Bench => unreachable!("bench times rather than counts"),
		};
		Ok(counter)
	}

	/// The measures `metrics.json` holds, a series each, each by the name a count's details give it
	/// and the name and unit `metrics.json` gives it under `fingerprint`: instructions in user space,
	/// natively, or Valgrind's, which are user space's too, beside its estimated cycles.
	pub(crate) fn series(&self, fingerprint: &str) -> Vec<(&'static str, String, &'static str)> {
		match self.kind {
			CounterKind::Valgrind => vec![
				("instructions", format!("instructions ({fingerprint})"), "instructions"),
				(
					callgrind::ESTIMATED,
					format!("{} ({fingerprint})", callgrind::ESTIMATED),
					"cycles",
				),
			],
			CounterKind::Perf { .. } => vec![(
				"instructions:u",
				format!("instructions:u ({fingerprint})"),
				"instructions",
			)],
		}
	}

	/// The mode that counts so.
	pub(crate) fn mode(&self) -> Mode {
		match self.kind {
			CounterKind::Valgrind => Mode::Sim,
			CounterKind::Perf { .. } => Mode::Quick,
		}
	}
}

/// The reason a failed harness target gave on its stderr, at `errors`, after a colon, or nothing.
pub(crate) fn reason(errors: &Path) -> String {
	std::fs::read(errors)
		.ok()
		.and_then(|bytes| {
			String::from_utf8_lossy(&bytes)
				.lines()
				.rev()
				.find_map(|line| line.strip_prefix("swole: ").map(str::to_owned))
		})
		.map_or(String::new(), |why| format!(": {why}"))
}

/// A harness target's stdout at `path`, where a benchmark may write any bytes around the
/// harness's own lines, which are UTF-8.
pub(crate) fn printed(path: &Path) -> Result<String, String> {
	let bytes = std::fs::read(path).map_err(|e| format!("cannot read {}: {e}", path.display()))?;
	Ok(String::from_utf8_lossy(&bytes).into_owned())
}

/// Each line of `text` under swole's prefix.
pub(crate) fn indent(text: &str) -> String {
	text.lines().map(|line| format!("swole: {line}\n")).collect()
}

/// A note naming the benchmarks beside which other threads ran, if any, and what that did.
pub(crate) fn threaded(counts: &Counts) -> String {
	let mut out = String::new();
	if !counts.threaded.is_empty() {
		let names: Vec<&str> = counts.threaded.iter().map(String::as_str).collect();
		// Valgrind counts every thread, and quick's counters, once a call runs another thread, are
		// ones every thread inherits.
		out += &format!(
			"\nOther threads ran beside {}, so each count and measure, of every thread, is the median of {SAMPLES}, with \
			 their least and most ({}), which the scheduler moves.\n",
			names.join(", "),
			metrics::SAMPLED
		);
	}
	// Sampled without having been seen to run other threads: swole could not watch them.
	let unwatched: Vec<&str> = counts
		.spread
		.keys()
		.filter(|name| !counts.threaded.contains(*name))
		.map(String::as_str)
		.collect();
	if !unwatched.is_empty() {
		out += &format!(
			"\nswole cannot see whether other threads ran beside {}, so each count and measure is the median of \
			 {SAMPLES}, with their least and most ({}).\n",
			unwatched.join(", "),
			metrics::SAMPLED
		);
	}
	out
}

/// The Valgrind every count and the fingerprint's probe run under: Callgrind, counting only
/// what the hooks bracket, following each exec, so a program a shell or `just` execs is
/// measured, leaving the exit code the program's, and simulating caches and branches. It reads
/// no options from a `.valgrindrc` or `VALGRIND_OPTS`, which the fingerprint never sees.
fn valgrind() -> Result<Command, String> {
	let mut command = Command::new(valgrind_path()?);
	command.args([
		"--command-line-only=yes",
		"--tool=callgrind",
		"--instr-atstart=no",
		"--trace-children=yes",
		"--dump-line=no",
	]);
	command.args(callgrind::SIMULATION);
	Ok(command)
}

/// swole's directory in the user's cache, created when missing, as a canonical path.
pub(crate) fn cache() -> Result<PathBuf, String> {
	let cache = std::env::var_os("XDG_CACHE_HOME")
		.map(PathBuf::from)
		.or_else(|| std::env::var_os("HOME").map(|home| Path::new(&home).join(".cache")))
		.ok_or("neither XDG_CACHE_HOME nor HOME is set, so swole's cache has nowhere to go")?
		.join("swole");
	std::fs::create_dir_all(&cache).map_err(|e| format!("cannot create {}: {e}", cache.display()))?;
	cache
		.canonicalize()
		.map_err(|e| format!("cannot resolve {}: {e}", cache.display()))
}

/// Valgrind on swole's own `PATH`, found once, so the fingerprint and every count run
/// the same one even when a job's environment sets another `PATH`.
pub(crate) fn valgrind_path() -> Result<&'static Path, String> {
	static PATH: std::sync::OnceLock<Result<PathBuf, String>> = std::sync::OnceLock::new();
	PATH.get_or_init(|| {
		use std::os::unix::fs::PermissionsExt;
		let path = std::env::var_os("PATH").unwrap_or_default();
		std::env::split_paths(&path)
			.map(|dir| dir.join("valgrind"))
			.find(|file| {
				file.metadata()
					.is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
			})
			.ok_or_else(|| "valgrind is not on PATH".to_owned())
			// Absolute, since jobs run in directories of their own.
			.and_then(|file| std::path::absolute(&file).map_err(|e| format!("cannot resolve {}: {e}", file.display())))
	})
	.as_deref()
	.map_err(Clone::clone)
}

/// Counts the benchmarks in `dir` with `counter`: builds them, then runs each job. When
/// `strict`, a wanted benchmark `dir` lacks fails it; a compare checks that across both trees.
pub(crate) fn measure(dir: &Path, options: &Options, counter: &Counter, strict: bool) -> Result<Counts, String> {
	// Absolute, since each job runs in a directory of its own choosing.
	let dir = &dir
		.canonicalize()
		.map_err(|e| format!("cannot resolve {}: {e}", dir.display()))?;
	let preload = preload::install()?;
	fits(&std::fs::read_to_string("/proc/self/limits").map_err(|e| format!("cannot read /proc/self/limits: {e}"))?)?;
	let mut jobs = if options.programs.is_empty() {
		cargo::jobs(
			dir,
			&cargo::Build {
				args: &options.cargo,
				lacking: if strict {
					cargo::Lacking::Nothing
				} else {
					cargo::Lacking::Targets
				},
				rustflags: &[],
			},
		)?
	} else {
		if let Some(build) = &options.build {
			eprintln!("swole: {build}");
			// Its output goes to stderr, so stdout holds only the counts.
			let status = shell(build, dir)
				.stdout(std::io::stderr())
				.status()
				.map_err(|e| format!("cannot run {build}: {e}"))?;
			if !status.success() {
				return Err(format!("{build} failed ({status})"));
			}
		}
		options
			.programs
			.iter()
			.map(|program| Job {
				label: program.clone(),
				command: shell(program, dir),
				expect: None,
				codspeed: false,
			})
			.collect()
	};
	if let (CounterKind::Perf { .. }, Some(job)) = (&counter.kind, jobs.iter().find(|job| job.codspeed)) {
		return Err(format!(
			"quick counts benchmarks of swole's harness, and {} is of CodSpeed's crates, which only sim counts",
			job.label
		));
	}
	if let Some(wanted) = &options.benchmarks {
		// Where every job names its benchmark, a name none has fails before any runs.
		if strict && jobs.iter().all(|job| job.expect.is_some()) {
			let known: BTreeSet<&String> = jobs.iter().filter_map(|job| job.expect.as_ref()).collect();
			absent(wanted, |name| known.contains(name), options.allow_missing)?;
		}
		// A job that runs one known benchmark runs only when wanted; a program runs
		// whichever it holds.
		jobs.retain(|job| job.expect.as_ref().is_none_or(|name| wanted.contains(name)));
	}
	let parallel = options.jobs.unwrap_or_else(|| match &counter.kind {
		CounterKind::Perf { pmu, .. } => pmu.cpus.len(),
		CounterKind::Valgrind => std::thread::available_parallelism().map_or(1, usize::from),
	});
	let work = work(dir)?;
	let all: Vec<&Job> = jobs.iter().collect();
	// The first round also counts each harness benchmark's allocations, in a pass of its own
	// beside its count, with the same randomness and clock.
	let heap = heap::library(true)?;
	let first = run(
		&all,
		&work.join("1"),
		Preloads {
			count: &preload,
			heap: heap.as_deref(),
		},
		parallel,
		counter,
		false,
	)?;
	let mut counts = Counts::none(counter);
	// Each count's benchmark and loaded libc, every round's, which one fingerprint must cover.
	let mut libcs: Vec<(String, Option<String>)> = first
		.iter()
		.flatten()
		.map(|found| (found.name.clone(), found.libc.clone()))
		.collect();
	// Each benchmark's samples: its count, and its details.
	let mut samples: BTreeMap<String, Vec<(u64, Vec<Measure>)>> = BTreeMap::new();
	for found in first.iter().flatten() {
		if found.threaded {
			counts.threaded.insert(found.name.clone());
		}
		if let Some(throughput) = found.throughput {
			counts.throughput.insert(found.name.clone(), throughput);
		}
		if samples
			.insert(found.name.clone(), vec![(found.count, found.details.clone())])
			.is_some()
		{
			return Err(format!("two benchmarks are named {}", found.name));
		}
	}
	// A job beside whose benchmarks other threads ran counts again, its samples' spread
	// standing for what the scheduler moves: natively, with counters every thread inherits.
	// Under Valgrind, so does one whose threads swole cannot see: CodSpeed's, a program's,
	// whose harness no `SWOLE_BENCH` watches, and one whose harness is too old to say.
	// Each job repeated, with the names its first round measured.
	let valgrind = matches!(counter.kind, CounterKind::Valgrind);
	let again: Vec<(&Job, Vec<&String>)> = jobs
		.iter()
		.zip(&first)
		.filter(|(job, found)| {
			found.iter().any(|found| found.threaded)
				|| valgrind && (job.codspeed || job.expect.is_none() || found.iter().any(|found| !found.watched))
		})
		.map(|(job, found)| {
			let mut names: Vec<&String> = found.iter().map(|found| &found.name).collect();
			names.sort_unstable();
			(job, names)
		})
		.collect();
	let repeated: Vec<&Job> = again.iter().map(|(job, _)| *job).collect();
	for round in 1..SAMPLES {
		if repeated.is_empty() {
			break;
		}
		let more = run(
			&repeated,
			&work.join((round + 1).to_string()),
			Preloads {
				count: &preload,
				heap: None,
			},
			parallel,
			counter,
			true,
		)?;
		// Each job counts again exactly what its first round did, so every median is of
		// SAMPLES of one job's benchmark.
		for ((job, expected), found) in again.iter().zip(&more) {
			let mut names: Vec<&String> = found.iter().map(|found| &found.name).collect();
			names.sort_unstable();
			if names != *expected {
				return Err(format!(
					"count {} of {} measured {names:?}, not {expected:?}",
					round + 1,
					job.label
				));
			}
		}
		libcs.extend(
			more.iter()
				.flatten()
				.map(|found| (found.name.clone(), found.libc.clone())),
		);
		for found in more.iter().flatten() {
			// Each count divides by what the process that took it declared.
			if found.throughput != counts.throughput.get(&found.name).copied() {
				return Err(format!(
					"count {} of {} declared another throughput than its first",
					round + 1,
					found.name
				));
			}
			if found.threaded {
				counts.threaded.insert(found.name.clone());
			}
			samples
				.entry(found.name.clone())
				.or_default()
				.push((found.count, found.details.clone()));
		}
	}
	if let CounterKind::Perf { native, .. } = &counter.kind
		&& let Some(bench_libc) =
			fingerprint::require_single_libc(libcs.iter().map(|(name, libc)| (name.as_str(), libc.as_deref())), true)?
	{
		let resolved = native.with_libc(Some(bench_libc));
		counts.fingerprint = resolved.label();
		counts.host = resolved.describe();
	}
	std::fs::remove_dir_all(&work).map_err(|e| format!("cannot remove {}: {e}", work.display()))?;
	let mut allocs: BTreeMap<String, Vec<Measure>> = BTreeMap::new();
	for found in first.into_iter().flatten() {
		if let Some(heap::Allocs::Counted(measures)) = found.allocs {
			allocs.insert(found.name.clone(), measures);
		}
		if !found.functions.is_empty() {
			counts.functions.insert(found.name, found.functions);
		}
	}
	for (name, taken) in samples {
		let details: Vec<Vec<Measure>> = taken.iter().map(|(_, details)| details.clone()).collect();
		let mut details = median(&details).map_err(|err| format!("{name}: {err}"))?;
		details.extend(allocs.remove(&name).unwrap_or_default());
		if !details.is_empty() {
			counts.details.insert(name.clone(), details);
		}
		let mut taken: Vec<u64> = taken.into_iter().map(|(count, _)| count).collect();
		taken.sort_unstable();
		if taken.len() > 1 {
			counts.spread.insert(name.clone(), (taken[0], taken[taken.len() - 1]));
		}
		counts.counts.insert(name, taken[taken.len() / 2]);
	}
	if let Some(wanted) = &options.benchmarks {
		if strict {
			let absent = absent(wanted, |name| counts.counts.contains_key(name), options.allow_missing)?;
			if !absent.is_empty() {
				report(&absent)?;
			}
		}
		counts.counts.retain(|name, _| wanted.contains(name));
		counts.spread.retain(|name, _| wanted.contains(name));
		counts.threaded.retain(|name| wanted.contains(name));
		counts.details.retain(|name, _| wanted.contains(name));
		counts.functions.retain(|name, _| wanted.contains(name));
		counts.throughput.retain(|name, _| wanted.contains(name));
	}
	if strict && counts.counts.is_empty() {
		return Err("no benchmark ran".to_owned());
	}
	Ok(counts)
}

/// Each measure of a benchmark's `samples`, the median of its values, with their least and most
/// when it took more than one, so a measure the scheduler moves reads as its spread, not one draw.
/// A ratio of counters a sample may leave undefined, as a top-down split when neither of its pair
/// counted, is kept only when every sample defines it; any other measure every sample must have.
pub(crate) fn median(samples: &[Vec<Measure>]) -> Result<Vec<Measure>, String> {
	let mut names: Vec<(&str, &'static str)> = Vec::new();
	for measure in samples.iter().flatten() {
		if !names.iter().any(|(name, _)| *name == measure.name) {
			names.push((&measure.name, measure.unit));
		}
	}
	let mut measures = Vec::new();
	'measure: for (name, unit) in names {
		let mut values = Vec::with_capacity(samples.len());
		for details in samples {
			match details.iter().find(|other| other.name == name) {
				Some(other) => values.push(other.value),
				None if perf::derived(name) => continue 'measure,
				None => return Err(format!("a sample lacks its {name}")),
			}
		}
		values.sort_by(f64::total_cmp);
		let mut median = Measure::new(name, values[values.len() / 2], unit);
		if values.len() > 1 {
			median.spread = Some((values[0], values[values.len() - 1]));
		}
		measures.push(median);
	}
	Ok(measures)
}

/// Fails naming each wanted benchmark `has` says no benchmark is, so a bisect step never
/// reads a suspect it could not count as clean.
pub(crate) fn missing(wanted: &BTreeSet<String>, has: impl Fn(&String) -> bool) -> Result<(), String> {
	absent(wanted, has, false).map(drop)
}

/// The wanted benchmarks `has` says no benchmark is. Any fails the run unless `allow`, when
/// only all of them does: Kixel Quest reads a benchmark `metrics.json` lacks as unmeasured,
/// never as clean, so a bisect step keeps the suspects it can count.
fn absent(wanted: &BTreeSet<String>, has: impl Fn(&String) -> bool, allow: bool) -> Result<Vec<&str>, String> {
	let absent: Vec<&str> = wanted.iter().filter(|name| !has(name)).map(String::as_str).collect();
	if absent.is_empty() || (allow && absent.len() < wanted.len()) {
		Ok(absent)
	} else {
		Err(format!("no benchmark is named {}", absent.join(", ")))
	}
}

/// Reports the wanted benchmarks a run lacks on stderr, and in the GitHub Actions job's
/// summary when it runs in one.
fn report(absent: &[&str]) -> Result<(), String> {
	use std::io::Write;
	eprintln!("swole: no benchmark is named {}; counted the rest", absent.join(", "));
	let Some(summary) = std::env::var_os("GITHUB_STEP_SUMMARY").filter(|path| !path.is_empty()) else {
		return Ok(());
	};
	let mut text = String::from("swole counted no benchmark named:\n\n");
	for name in absent {
		text.push_str(&format!("- `{name}`\n"));
	}
	text.push('\n');
	std::fs::OpenOptions::new()
		.append(true)
		.create(true)
		.open(&summary)
		.and_then(|mut file| file.write_all(text.as_bytes()))
		.map_err(|e| format!("cannot write the job summary {}: {e}", Path::new(&summary).display()))
}

/// `sh -c command` in `dir`.
fn shell(command: &str, dir: &Path) -> Command {
	let mut sh = Command::new("sh");
	sh.arg("-c").arg(command).current_dir(dir);
	sh
}

/// A new directory for one run's jobs in the tree at `dir`, apart from any other run's, so
/// two runs in one tree never read each other's profiles.
pub(crate) fn work(dir: &Path) -> Result<PathBuf, String> {
	let parent = dir.join("target/swole");
	std::fs::create_dir_all(&parent).map_err(|e| format!("cannot create {}: {e}", parent.display()))?;
	let pid = std::process::id();
	for n in 0.. {
		let work = parent.join(format!("jobs-{pid}-{n}"));
		match std::fs::create_dir(&work) {
			Ok(()) => return Ok(work),
			// Left by an earlier process of the same id that failed.
			Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
			Err(e) => return Err(format!("cannot create {}: {e}", work.display())),
		}
	}
	unreachable!("some name is free")
}

/// The libraries a round of jobs preloads: the count's, and the allocation pass's, when the round
/// counts allocations too.
#[derive(Clone, Copy)]
struct Preloads<'a> {
	count: &'a Path,
	heap: Option<&'a Path>,
}

/// Runs each job with the libraries `preloads`, counted by `counter`, `parallel` at once, each
/// in a directory of its own under `work`, and gathers what each measured, in the jobs' order.
/// Natively, a job counts with counters every thread inherits when `threaded`, as a job seen to
/// run another thread repeats, and otherwise switches to them once it sees one. Any failure fails
/// the whole and keeps `work` for its output.
fn run(
	jobs: &[&Job],
	work: &Path,
	preloads: Preloads,
	parallel: usize,
	counter: &Counter,
	threaded: bool,
) -> Result<Vec<Vec<Found>>, String> {
	let preload = preloads.count;
	let total = jobs.len();
	let queue = Mutex::new(jobs.iter().enumerate());
	let done = AtomicUsize::new(0);
	let failed = AtomicBool::new(false);
	let mut results: Vec<(usize, Measured)> = std::thread::scope(|scope| {
		let workers: Vec<_> = (0..parallel.min(total))
			.map(|_| {
				scope.spawn(|| {
					let mut results = Vec::new();
					// A failure stops what has not started, since the run fails anyway.
					while !failed.load(Ordering::Relaxed) {
						let Some((index, job)) = queue.lock().expect("the queue's lock is never poisoned").next()
						else {
							break;
						};
						let started = Instant::now();
						let dir = work.join(index.to_string());
						let mut result = match &counter.kind {
							CounterKind::Valgrind => one(job, &dir, preload),
							CounterKind::Perf { pmu, passes, .. } => native(job, &dir, preload, pmu, passes, threaded),
						};
						// A harness job measures its one benchmark, whose allocations its own pass counts.
						if let (Ok([found]), Some(heap)) = (result.as_deref_mut(), preloads.heap)
							&& job.expect.is_some()
							&& !job.codspeed
						{
							let pass = heap::Pass {
								library: heap,
								padded: true,
								cpus: match &counter.kind {
									CounterKind::Perf { pmu, .. } => Some(&pmu.cpus),
									CounterKind::Valgrind => None,
								},
							};
							match heap::measure(job, pass, found.throughput, &dir) {
								Ok(allocs) => found.allocs = Some(allocs),
								Err(err) => result = Err(err),
							}
						}
						let finished = done.fetch_add(1, Ordering::Relaxed) + 1;
						match &result {
							Ok(measured) => eprintln!(
								"swole: [{finished}/{total}] {}: {} benchmark(s), {} instructions, {:.1}s",
								job.label,
								measured.len(),
								measured.iter().map(|found| found.count).sum::<u64>(),
								started.elapsed().as_secs_f64()
							),
							Err(_) => failed.store(true, Ordering::Relaxed),
						}
						results.push((index, result));
					}
					results
				})
			})
			.collect();
		workers
			.into_iter()
			.flat_map(|worker| worker.join().expect("a job's thread panicked"))
			.collect()
	});
	results.sort_by_key(|(index, _)| *index);
	let mut found = Vec::new();
	let mut errors = Vec::new();
	for (_, result) in results {
		match result {
			Ok(measured) => found.push(measured),
			Err(err) => errors.push(err),
		}
	}
	if !errors.is_empty() {
		return Err(errors.join("\n"));
	}
	Ok(found)
}

/// The prefix of the variables [`pad`] fills.
const PAD: &str = "SWOLE_PAD_";

/// How many variables [`pad`] fills: a fixed number, so the count of pointers is fixed too, and
/// enough that none passes the 128 KiB Linux takes in one.
const PADS: usize = 16;

/// How many bytes [`pad`] brings a job's arguments and environment to: room for a dev shell's
/// several hundred KiB, and half the 2 MiB Linux takes in all under the default stack limit.
const LAUNCH: usize = 1024 * 1024;

/// How far a program's initial stack pointer sits below its strings, as Valgrind lays them out:
/// each argument and variable with its NUL, the program's path once more for `AT_EXECFN`, and a
/// pointer for each argument and variable. The rest it adds is the same for every job.
fn launch(env: &BTreeMap<OsString, OsString>, args: &[&OsStr]) -> usize {
	use std::os::unix::ffi::OsStrExt;
	let strings = args.iter().map(|arg| arg.as_bytes().len() + 1).sum::<usize>()
		+ args.first().map_or(0, |program| program.as_bytes().len() + 1)
		+ env
			.iter()
			.map(|(key, value)| key.as_bytes().len() + value.as_bytes().len() + 2)
			.sum::<usize>();
	strings + 8 * (args.len() + env.len())
}

/// Adds [`PADS`] variables to `env`, sized so the job's arguments and environment take
/// [`LAUNCH`] bytes, so its initial stack, and every address a count could depend on, sits at the
/// same place however long the checkout's path is or however large the host's environment.
fn pad(env: &mut BTreeMap<OsString, OsString>, args: &[&OsStr]) -> Result<(), String> {
	use std::os::unix::ffi::OsStrExt;
	env.retain(|key, _| !key.as_bytes().starts_with(PAD.as_bytes()));
	let name = |i: usize| format!("{PAD}{i:02}");
	// Each variable's name, `=`, NUL, and pointer.
	let used = launch(env, args) + (0..PADS).map(|i| name(i).len() + 2 + 8).sum::<usize>();
	let fill = LAUNCH.checked_sub(used).ok_or(format!(
		"its arguments and environment take {used} bytes, more than the {LAUNCH} swole pads every job's to"
	))?;
	for i in 0..PADS {
		let size = fill / PADS + usize::from(i < fill % PADS);
		env.insert(name(i).into(), "x".repeat(size).into());
	}
	Ok(())
}

/// Room beside [`LAUNCH`] for Valgrind's own path and options, which name the job's directory.
const VALGRIND_ROOM: usize = 64 * 1024;

/// Fails unless the stack limit `limits` (`/proc/self/limits`) gives `exec` room for a padded
/// launch: Linux takes arguments and environment up to a quarter of the limit, at most 6 MiB.
fn fits(limits: &str) -> Result<(), String> {
	let soft = limits
		.lines()
		.find_map(|line| line.strip_prefix("Max stack size"))
		.and_then(|rest| rest.split_whitespace().next())
		.ok_or("/proc/self/limits names no stack limit")?;
	let room = match soft {
		"unlimited" => 6 << 20,
		bytes => (bytes
			.parse::<usize>()
			.map_err(|_| format!("cannot read the stack limit {bytes}"))?
			/ 4)
		.min(6 << 20),
	};
	if room < LAUNCH + VALGRIND_ROOM {
		return Err(format!(
			"the stack limit (ulimit -s) leaves exec {} KiB for a job's arguments and environment, less than the {} KiB a \
			 padded job's launch takes: raise it to at least {} KiB",
			room / 1024,
			(LAUNCH + VALGRIND_ROOM) / 1024,
			4 * (LAUNCH + VALGRIND_ROOM) / 1024
		));
	}
	Ok(())
}

/// One benchmark a job measured.
struct Found {
	name: String,
	/// Its instructions.
	count: u64,
	/// Whether the process had another thread beside it.
	threaded: bool,
	/// Whether its harness said whether it had, as one too old to watch does not.
	watched: bool,
	/// The loaded libc identity reported by the benchmark process on Linux.
	libc: Option<String>,
	/// Every measure of it, its count's among them.
	details: Vec<Measure>,
	/// What each of its calls processes, as the process that counted it declared.
	throughput: Option<metrics::Throughput>,
	/// What its allocation pass found, when the round ran one.
	allocs: Option<heap::Allocs>,
	/// The self cost of each function that ran, when Callgrind profiled it.
	functions: callgrind::Functions,
}

/// What one job measured, or why it failed.
type Measured = Result<Vec<Found>, String>;

/// The variable that names the libraries a process loads first: macOS inserts with dyld's.
const PRELOAD: &str = if cfg!(target_os = "macos") {
	"DYLD_INSERT_LIBRARIES"
} else {
	"LD_PRELOAD"
};

/// The whole environment `command` launches with, with `preload` first in `LD_PRELOAD`, and when
/// `padded`, padded to [`LAUNCH`] bytes so a counted process starts at the same stack address.
pub(crate) fn environment(
	command: &Command,
	preload: &Path,
	label: &str,
	padded: bool,
) -> Result<BTreeMap<OsString, OsString>, String> {
	let mut env: BTreeMap<OsString, OsString> = std::env::vars_os().collect();
	for (key, value) in command.get_envs() {
		match value {
			Some(value) => env.insert(key.to_owned(), value.to_owned()),
			None => env.remove(key),
		};
	}
	// First, so its randomness and clock stand in front of any other preload's.
	let mut preloads = preload.as_os_str().to_owned();
	if let Some(others) = env.get(OsStr::new(PRELOAD)).filter(|others| !others.is_empty()) {
		preloads.push(":");
		preloads.push(others);
	}
	env.insert(PRELOAD.into(), preloads);
	if padded {
		let args: Vec<&OsStr> = std::iter::once(command.get_program())
			.chain(command.get_args())
			.collect();
		pad(&mut env, &args).map_err(|err| format!("{label}: {err}"))?;
	}
	Ok(env)
}

/// Runs one job natively, once for each of `passes`, its benchmark counted by that pass's perf
/// counters of `pmu` on its CPUs, with the library at `preload`, each pass's output under `dir`,
/// and reads what they counted: with counters every thread inherits when `threaded`, or once a
/// pass sees another thread run beside a call, whose work counters of the calling thread miss.
fn native(job: &Job, dir: &Path, preload: &Path, pmu: &perf::Pmu, passes: &[perf::Pass], threaded: bool) -> Measured {
	let label = &job.label;
	let mut every_thread = threaded;
	let (values, first, libcs, throughput) = 'count: loop {
		let mut values = Vec::new();
		let mut libcs: Vec<(String, Option<String>)> = Vec::new();
		let mut first: Option<perf::Reading> = None;
		let mut throughput = None;
		for (i, pass) in passes.iter().enumerate() {
			let dir = dir.join(format!("pass-{}", i + 1));
			std::fs::create_dir_all(&dir).map_err(|e| format!("cannot create {}: {e}", dir.display()))?;
			let path = dir.join("stdout");
			let stdout = std::fs::File::create(&path).map_err(|e| format!("cannot create {}: {e}", path.display()))?;
			let errors = dir.join("stderr");
			let stderr =
				std::fs::File::create(&errors).map_err(|e| format!("cannot create {}: {e}", errors.display()))?;
			let mut command = job.command();
			perf::launch(
				&mut command,
				&pmu.cpus,
				pass,
				perf::Launch {
					fixed: true,
					every_thread,
				},
			);
			let env = environment(&command, preload, label, true)?;
			command
				.env_clear()
				.envs(env)
				.stdin(std::process::Stdio::null())
				.stdout(stdout)
				.stderr(stderr);
			let status = command.status().map_err(|e| format!("cannot run {label}: {e}"))?;
			if !status.success() {
				return Err(format!(
					"{label} failed ({status}){}; its output is in {}",
					reason(&errors),
					dir.display()
				));
			}
			let text = printed(&path)?;
			let declared = metrics::Throughput::declared(&text).map_err(|err| format!("{label}: {err}"))?;
			let declared = job.expect.as_ref().and_then(|name| declared.get(name).copied());
			match &throughput {
				None => throughput = Some(declared),
				Some(first) if *first != declared => {
					return Err(format!("{label} declared another throughput in each pass"));
				}
				Some(_) => {}
			}
			let mut readings = perf::readings(&text, pass.len()).map_err(|err| format!("{label}: {err}"))?;
			let reading = match (&job.expect, readings.len()) {
				(Some(name), 1) if readings[0].name == *name => readings.remove(0),
				(_, 0) => {
					return Err(format!(
						"{label} printed no perf count: its swole harness is older than quick; its output is in {}",
						dir.display()
					));
				}
				_ => {
					let found: Vec<&str> = readings.iter().map(|reading| reading.name.as_str()).collect();
					return Err(format!(
						"{label} should count itself alone, but counted [{}]; its output is in {}",
						found.join(", "),
						dir.display()
					));
				}
			};
			// The calling thread's counters missed the other thread, so every pass counts again,
			// with counters every thread inherits, which a harness too old to says it cannot.
			if reading.threaded && !every_thread && !reading.inherits {
				return Err(format!(
					"{}: another thread ran beside a counted call, whose work the calling thread's counters miss, and its \
					 swole harness is too old to count every thread: upgrade it, or count it with sim",
					reading.name
				));
			}
			if reading.threaded && !every_thread {
				every_thread = true;
				continue 'count;
			}
			let verified = reading
				.libc
				.as_deref()
				.map(fingerprint::validate_libc_record)
				.transpose()?;
			libcs.push((label.clone(), verified));
			values.push(reading.values.clone());
			first.get_or_insert(reading);
		}
		break (values, first.expect("a host that counts has a pass"), libcs, throughput);
	};
	let libc = fingerprint::require_single_libc(
		libcs.iter().map(|(label, libc)| (label.as_str(), libc.as_deref())),
		true,
	)?;
	let values: Vec<(Vec<u64>, u64)> = values.into_iter().map(|values| (values, 1)).collect();
	let mut details = perf::measures(passes, &values);
	let count = details
		.iter()
		.find(|measure| measure.name == "instructions:u")
		.map(|measure| measure.value as u64)
		.expect("every pass plan counts user-space instructions");
	if let Some(usage) = first.usage {
		details.extend(usage.measures(1));
	}
	Ok(vec![Found {
		name: first.name,
		count,
		// Counted by every thread once one ran, so sampled however this run went.
		threaded: every_thread,
		watched: true,
		libc,
		details,
		throughput: throughput.flatten(),
		allocs: None,
		functions: callgrind::Functions::new(),
	}])
}

/// Runs one job under Valgrind with the library at `preload` in every process, its profiles
/// and output in `dir`, and reads what it measured.
fn one(job: &Job, dir: &Path, preload: &Path) -> Measured {
	let label = &job.label;
	let command = job.command();
	std::fs::create_dir_all(dir).map_err(|e| format!("cannot create {}: {e}", dir.display()))?;
	let file = |name: &str| {
		std::fs::File::create(dir.join(name)).map_err(|e| format!("cannot create {}: {e}", dir.join(name).display()))
	};
	// Valgrind expands every `%` in a file name, so one in the directory's stands for itself.
	let escaped = dir.to_str().ok_or("a job's directory is not UTF-8")?.replace('%', "%%");
	let mut valgrind = valgrind()?;
	valgrind
		.arg(format!("--callgrind-out-file={escaped}/callgrind.%p"))
		.arg(format!("--log-file={escaped}/valgrind.%p.log"))
		.arg(command.get_program())
		.args(command.get_args())
		.stdin(std::process::Stdio::null())
		.stdout(file("stdout")?)
		.stderr(file("stderr")?);
	if let Some(cwd) = command.get_current_dir() {
		valgrind.current_dir(cwd);
	}
	let mut env = environment(&command, preload, label, true)?;
	// A `--program`'s shell inherits swole's environment, whose counter settings are a native count's.
	env.remove(OsStr::new(perf::COUNTERS));
	env.remove(OsStr::new(perf::INHERIT));
	env.remove(OsStr::new(perf::ITERATIONS));
	valgrind.env_clear().envs(env);
	let status = valgrind.status().map_err(|e| format!("cannot run valgrind: {e}"))?;
	if !status.success() {
		return Err(format!(
			"{label} failed under Valgrind ({status}); its output is in {}",
			dir.display()
		));
	}
	let stdout = printed(&dir.join("stdout"))?;
	let threads = perf::threads(&stdout);
	let declared = metrics::Throughput::declared(&stdout).map_err(|err| format!("{label}: {err}"))?;
	let mut measured = Vec::new();
	let mut entries: Vec<_> = std::fs::read_dir(dir)
		.map_err(|e| format!("cannot read {}: {e}", dir.display()))?
		.map(|entry| entry.map(|entry| entry.path()))
		.collect::<Result<_, _>>()
		.map_err(|e| format!("cannot read {}: {e}", dir.display()))?;
	entries.sort();
	for path in entries {
		if !path
			.file_name()
			.is_some_and(|name| name.to_string_lossy().starts_with("callgrind."))
		{
			continue;
		}
		let text = std::fs::read_to_string(&path).map_err(|e| format!("cannot read {}: {e}", path.display()))?;
		let found = callgrind::parse(&text, &path.display().to_string())?;
		if !found.is_empty() {
			// `callgrind.<pid>`, then `.<n>` for each dump after the first.
			let name = path.file_name().expect("listed").to_string_lossy();
			let pid = name["callgrind.".len()..].split('.').next().unwrap_or_default();
			preload::check(dir, pid, label)?;
		}
		measured.extend(found.into_iter().map(|benchmark| Found {
			threaded: threads.get(&benchmark.name) == Some(&true),
			watched: threads.contains_key(&benchmark.name),
			throughput: declared.get(&benchmark.name).copied(),
			name: benchmark.name,
			count: benchmark.costs.ir(),
			libc: None,
			details: benchmark.costs.measures(),
			allocs: None,
			functions: benchmark.functions,
		}));
	}
	match &job.expect {
		Some(name) => match measured.as_slice() {
			[only] if only.name == *name => {}
			_ => {
				let found: Vec<&str> = measured.iter().map(|found| found.name.as_str()).collect();
				return Err(format!(
					"{label} should measure itself alone, but measured [{}]",
					found.join(", ")
				));
			}
		},
		None if measured.is_empty() => return Err(format!("{label} measured no benchmark")),
		None => {}
	}
	Ok(measured)
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn a_ratio_some_sample_leaves_undefined_drops_but_a_counter_fails() {
		let split = |value| Measure::new("topdown branch mispredicts", value, "%");
		let counter = |value| Measure::new("cycles:u", value, "events");
		// Whichever round counted no mispredict or restart, the split goes and the counter stays.
		for samples in [
			vec![
				vec![counter(1.0)],
				vec![counter(2.0), split(5.0)],
				vec![counter(3.0), split(6.0)],
			],
			vec![
				vec![counter(1.0), split(5.0)],
				vec![counter(2.0)],
				vec![counter(3.0), split(6.0)],
			],
		] {
			let measures = median(&samples).unwrap();
			assert_eq!(measures.len(), 1);
			assert_eq!(measures[0].name, "cycles:u");
			assert_eq!(measures[0].spread, Some((1.0, 3.0)));
		}
		assert!(median(&[vec![counter(1.0), split(5.0)], vec![split(6.0)]]).is_err());
	}

	#[test]
	fn a_list_holds_one_name_a_line() {
		let wanted = names("a::b\n\n  c[1]  \na::b\n");
		assert_eq!(wanted.into_iter().collect::<Vec<_>>(), ["a::b", "c[1]"]);
	}

	#[test]
	fn allowing_absent_names_fails_only_when_all_are() {
		let wanted = names("a\nb\n");
		let has = |names: &'static [&str]| move |name: &String| names.contains(&name.as_str());
		assert_eq!(absent(&wanted, has(&["a", "b"]), false), Ok(vec![]));
		assert!(absent(&wanted, has(&["a"]), false).is_err());
		assert_eq!(absent(&wanted, has(&["a"]), true), Ok(vec!["b"]));
		assert_eq!(
			absent(&wanted, has(&[]), true),
			Err("no benchmark is named a, b".to_owned())
		);
	}

	#[test]
	fn refuses_a_stack_limit_too_small_to_pad_under() {
		let limits = |soft: &str| format!("Limit Soft Limit Hard Limit Units\nMax stack size {soft} unlimited bytes\n");
		assert!(fits(&limits("8388608")).is_ok());
		assert!(fits(&limits("unlimited")).is_ok());
		assert!(fits(&limits("2097152")).unwrap_err().contains("ulimit -s"));
		assert!(fits("Max open files 1024 1024 files\n").is_err());
	}

	#[test]
	fn pads_every_launch_to_one_size() {
		let env = |pairs: &[(&str, &str)]| -> BTreeMap<OsString, OsString> {
			pairs.iter().map(|(key, value)| (key.into(), value.into())).collect()
		};
		let mut short = env(&[("CARGO_MANIFEST_DIR", "/a/rs/net"), ("SWOLE_PAD_03", "stale")]);
		let mut long = env(&[
			("CARGO_MANIFEST_DIR", "/a/much/longer/checkout/rs/net"),
			("GITHUB_REF", "refs/heads/kixel-bisect/1/abc"),
		]);
		pad(&mut short, &[OsStr::new("/a/bench"), OsStr::new("--bench")]).unwrap();
		pad(&mut long, &[OsStr::new("/a/much/longer/checkout/bench")]).unwrap();
		assert_eq!(launch(&short, &[OsStr::new("/a/bench"), OsStr::new("--bench")]), LAUNCH);
		assert_eq!(launch(&long, &[OsStr::new("/a/much/longer/checkout/bench")]), LAUNCH);
		assert_eq!(short.len(), 1 + PADS);
		let mut huge = env(&[("BIG", &"x".repeat(LAUNCH))]);
		assert!(pad(&mut huge, &[OsStr::new("/a/bench")]).is_err());
	}

	#[test]
	fn options_keep_programs_and_cargo_args_apart() {
		let args = |list: &[&str]| list.iter().map(|s| (*s).to_owned()).collect::<Vec<_>>();
		let sim = |list: &[&str]| Options::parse(&args(list), Mode::Sim, false);
		let options = sim(&["--jobs", "3", "--", "-p", "net", "--features", "x"]).unwrap();
		assert_eq!(options.jobs, Some(3));
		assert_eq!(options.cargo, ["-p", "net", "--features", "x"]);
		assert!(sim(&["--program", "a", "--", "-p", "net"]).is_err());
		assert!(sim(&["--build", "make"]).is_err());
		assert!(sim(&["--jobs", "0"]).is_err());
		assert!(sim(&["--jobs"]).is_err());
		assert!(sim(&["--frobnicate"]).is_err());
		assert!(sim(&["--allow-missing"]).is_err());
		// A quick count writes its own host's series, but no compare writes any.
		assert!(Options::parse(&args(&["--out", "m.json"]), Mode::Quick, false).is_ok());
		assert!(Options::parse(&args(&["--out", "m.json"]), Mode::Quick, true).is_err());
		assert!(Options::parse(&args(&["--out", "m.json"]), Mode::Sim, true).is_err());
		assert!(Options::parse(&args(&["--out", "m.json"]), Mode::Bench, true).is_ok());
		assert!(Options::parse(&args(&["--program", "a"]), Mode::Quick, false).is_err());
		assert!(Options::parse(&args(&["--program", "a"]), Mode::Bench, false).is_err());
		assert!(Options::parse(&args(&["--jobs", "2"]), Mode::Bench, false).is_err());
	}
}
