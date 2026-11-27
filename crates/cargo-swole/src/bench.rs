//! Wall-clock times of the working tree's benchmarks, each alone, or each paired with a base
//! commit's: a sample of one beside a sample of the other, in an order drawn anew for each pair,
//! so host load that drifts or recurs biases neither side. Load still widens the change's interval.
//! Beside each time, what the process used over its samples, and every perf counter the host
//! lets swole open, a call's share of each, counted in processes of their own.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{ChildStdin, ChildStdout, Stdio};
use std::time::{Duration, Instant};

use serde_json::Number;

use crate::cargo::Lacking;
use crate::compare::Checkout;
use crate::fingerprint::Native;
use crate::{Job, Measure, Mode, Options, cargo, metrics, missing, perf, work};

/// How long a sample's timed part aims to take: long enough that the clock's reads and its
/// resolution vanish in it, short enough that a pair's two samples see the same host and that a
/// preemption spoils few samples, which the median then leaves out.
const SLICE: Duration = Duration::from_millis(1);

/// How long a whole sample may take before its timed part fills a slice, as when steps or inputs
/// built between calls take far longer than the calls.
const WALL: Duration = Duration::from_millis(50);

/// How long each benchmark's samples take in all, both sides of a pair together.
const BUDGET: Duration = Duration::from_secs(1);

/// The fewest samples, or pairs, a benchmark takes however long they take: enough for a 95%
/// interval around their median that leaves out the extremes on each side.
const MIN_SAMPLES: usize = 10;

/// The most iterations one sample takes: a benchmark that runs these in under a slice does
/// nothing the clock can see.
const MAX_ITERATIONS: u64 = 1 << 40;

/// What a timed harness target prints, alone on a line, before each sample's nanoseconds.
const SAMPLE: &str = "swole sample ";

/// The unit every time is in.
const UNIT: &str = "ns";

/// Times the working tree's benchmarks, or with `base` each beside the base's, with what each
/// used and every counter the host allows, prints them, and writes them to `--out` and `--report`
/// when given.
pub fn bench(base: Option<&str>, options: &Options) -> Result<(), String> {
	// Probed before anything builds, so the run says first what it can measure.
	let native = Native::probe(Mode::Bench)?;
	eprint!("{}", crate::indent(&native.host.describe()));
	if let Err(why) = &native.host.perf {
		eprintln!("swole: no perf counters, since {why}: timing by wall clock, with rusage");
	}
	let perf = native.host.perf.as_ref().ok();
	// Both timed and counting passes run under the preload built to count threads alone when perf
	// is available, which detects even short-lived workers spawned and joined inside a timed sample
	// or counter pass while keeping the host's clock and randomness untouched.
	let preload = perf.map(|_| crate::preload::threads()).transpose()?;
	// Held until the base is timed, so no other compare checks out another commit under it.
	let checkout = base.map(|base| Checkout::new(base, options)).transpose()?;
	// Each side builds only the packages and targets of the Cargo args its revision has.
	let (before, head) = match &checkout {
		Some(checkout) => {
			let sides = checkout.select(options)?;
			(Some((checkout, sides.base)), sides.head)
		}
		None => (None, Some(options.cargo.clone())),
	};
	let before = match before {
		Some((checkout, Some(cargo))) => {
			eprintln!(
				"swole: building {} ({}) in {}",
				checkout.base,
				checkout.short(),
				checkout.dir.display()
			);
			let jobs = cargo::jobs(&checkout.dir, &cargo, Lacking::Counted)?;
			// A base from before a rewrite to swole has only CodSpeed's benchmarks,
			// which a time leaves out, so every benchmark of the head times alone.
			let (codspeed, jobs): (Vec<Job>, Vec<Job>) = jobs.into_iter().partition(|job| job.codspeed);
			if !codspeed.is_empty() {
				eprintln!(
					"swole: the base's {} benchmark(s) of CodSpeed's crates time as none",
					codspeed.len()
				);
			}
			Some(by_name(jobs))
		}
		Some((checkout, None)) => {
			eprintln!(
				"swole: {} has nothing the cargo args select, so times none",
				checkout.base
			);
			Some(BTreeMap::new())
		}
		None => None,
	};
	let after = match head {
		Some(cargo) => {
			eprintln!("swole: building the working tree");
			// With a base, the head may have no benchmark target left, its base's all removed.
			let lacking = if before.is_some() {
				Lacking::Targets
			} else {
				Lacking::Nothing
			};
			by_name(cargo::jobs(Path::new("."), &cargo, lacking)?)
		}
		None => {
			eprintln!("swole: the working tree has nothing the cargo args select, so times none");
			BTreeMap::new()
		}
	};
	if let Some(job) = after.values().find(|job| job.codspeed) {
		return Err(format!(
			"{} is a benchmark of CodSpeed's crates, which only a count measures: write it with swole, \
			 or leave its target out with cargo's --bench",
			job.label
		));
	}
	let empty = BTreeMap::new();
	let mut names: BTreeSet<String> = before
		.as_ref()
		.unwrap_or(&empty)
		.keys()
		.chain(after.keys())
		.cloned()
		.collect();
	if let Some(wanted) = &options.benchmarks {
		missing(wanted, |name| names.contains(name))?;
		names.retain(|name| wanted.contains(name));
	}
	if names.is_empty() {
		return Err("no benchmark ran".to_owned());
	}
	let (mut before, mut after) = (before, after);
	let work = work(Path::new("."))?;
	let mut coin = Coin::seeded();
	let mut times = BTreeMap::new();
	// Each benchmark's other measures, by side: the base's, then the head's.
	let mut details: [BTreeMap<String, Vec<Measure>>; 2] = Default::default();
	// Each paired benchmark's throughput change, from its paired timing.
	let mut rates = BTreeMap::new();
	// The benchmarks beside which another thread ran, on either side, which have no counters.
	let mut threaded = std::collections::BTreeSet::new();
	// Whether another thread ran beside each benchmark, where every side was watched or one
	// was seen threaded.
	let mut seen = BTreeMap::new();
	// Mapped libc identity reported by every benchmark process across the run, verifying none is
	// missing (on Linux) and all agree on one libc build.
	let mut libcs: Vec<(String, Option<String>)> = Vec::new();
	let total = names.len();
	for (index, name) in names.into_iter().enumerate() {
		let started = Instant::now();
		let dir = work.join(index.to_string());
		let base = before.as_mut().and_then(|jobs| jobs.remove(&name));
		let head = after.remove(&name);
		let pmu = perf.map(|perf| &perf.pmu);
		let (timed, iterations, used, timed_threaded) = match (&base, &head) {
			(Some(base), Some(head)) => {
				let mut base = Process::spawn(base, pmu, preload.as_deref(), &dir.join("base"))?;
				let mut head = Process::spawn(head, pmu, preload.as_deref(), &dir.join("head"))?;
				let (iterations, pairs) = paired(&mut base, &mut head, &mut coin)?;
				let base_threaded = base.threaded || pairs.iter().any(|(b, _)| b.threaded);
				let head_threaded = head.threaded || pairs.iter().any(|(_, h)| h.threaded);
				libcs.push((format!("{name} (base)"), base.libc.take()));
				libcs.push((format!("{name} (head)"), head.libc.take()));
				base.finish()?;
				head.finish()?;
				let nanos: Vec<(u64, u64)> = pairs.iter().map(|(b, h)| (b.nanos, h.nanos)).collect();
				let base_used = usage(pairs.iter().map(|(b, _)| b));
				let head_used = usage(pairs.iter().map(|(_, h)| h));
				let timed = Times::pairs(iterations, &nanos).map_err(|err| format!("{name}: {err}"))?;
				(
					timed,
					iterations,
					[base_used, head_used],
					[base_threaded, head_threaded],
				)
			}
			(base, head) => {
				let (side, job) = match (base, head) {
					(Some(base), None) => (Which::Base, base),
					(None, Some(head)) => (Which::Head, head),
					_ => unreachable!("every name is one side's"),
				};
				let mut process = Process::spawn(job, pmu, preload.as_deref(), &dir.join(side.name()))?;
				let (iterations, samples) = alone(&mut process)?;
				let side_threaded = process.threaded || samples.iter().any(|sample| sample.threaded);
				libcs.push((name.clone(), process.libc.take()));
				process.finish()?;
				let nanos: Vec<u64> = samples.iter().map(|sample| sample.nanos).collect();
				let timed = Times::alone(side, iterations, &nanos).map_err(|err| format!("{name}: {err}"))?;
				let used = usage(samples.iter());
				(
					timed,
					iterations,
					if side == Which::Base {
						[used, None]
					} else {
						[None, used]
					},
					if side == Which::Base {
						[side_threaded, false]
					} else {
						[false, side_threaded]
					},
				)
			}
		};
		let mut watched = true;
		for (i, (side, job)) in [(Which::Base, &base), (Which::Head, &head)].into_iter().enumerate() {
			let Some(job) = job else { continue };
			let mut measures = if timed_threaded[i] {
				threaded.insert(name.clone());
				Vec::new()
			} else {
				match (perf, &preload) {
					(Some(perf), Some(preload)) => {
						let (counted, count_libc) = count(job, perf, preload, iterations, &dir.join(side.name()))?;
						libcs.push((format!("{name} ({} counters)", side.name()), count_libc));
						match counted {
							Counted::Measures(measures) => measures,
							Counted::Threaded => {
								threaded.insert(name.clone());
								Vec::new()
							}
							Counted::Unwatched => {
								watched = false;
								Vec::new()
							}
						}
					}
					_ => {
						watched = false;
						Vec::new()
					}
				}
			};
			if let Some((used, calls)) = &used[i] {
				measures.extend(used.measures(*calls * iterations));
			}
			// What derives from the side's time and counts, per what each of its calls processes.
			if let (Some(throughput), Some(time)) = (job.throughput, [timed.base(), timed.head()][i]) {
				let mut all = vec![Measure::new("time", time, UNIT)];
				all.extend(measures.iter().cloned());
				measures.extend(throughput.measures(&all));
			}
			if !measures.is_empty() {
				details[i].insert(name.clone(), measures);
			}
		}
		// A paired rate changes as the work each call does over the paired time ratio, which the
		// time's own change is, rather than as its sides' medians do.
		if let (Times::Both { ratio, .. }, Some(base), Some(head)) = (
			&timed,
			base.as_ref().and_then(|job| job.throughput),
			head.as_ref().and_then(|job| job.throughput),
		) && let Some(scale) = head.scale(base)
		{
			rates.insert(name.clone(), scale / ratio.value - 1.0);
		}
		if threaded.contains(&name) {
			seen.insert(name.clone(), true);
		} else if watched {
			seen.insert(name.clone(), false);
		}
		eprintln!(
			"swole: [{}/{total}] {name}: {}, {:.1}s",
			index + 1,
			timed.summary(),
			started.elapsed().as_secs_f64()
		);
		times.insert(name, timed);
	}
	// Every process exited cleanly, so the run's output is no longer evidence.
	std::fs::remove_dir_all(&work).map_err(|e| format!("cannot remove {}: {e}", work.display()))?;
	let native = match crate::fingerprint::require_single_libc(
		libcs.iter().map(|(label, libc)| (label.as_str(), libc.as_deref())),
		cfg!(target_os = "linux"),
	)? {
		Some(bench_libc) => native.with_libc(Some(bench_libc)),
		None => native,
	};
	let host = native.label();
	eprintln!("swole: fingerprint {host}");
	let paired = checkout.is_some();
	print!("{}", table(&times, paired));
	if !threaded.is_empty() {
		let names: Vec<&str> = threaded.iter().map(String::as_str).collect();
		println!(
			"\nOther threads ran beside {}, so bench left out their perf counters, which read the calling thread \
			 alone.",
			names.join(", ")
		);
	}
	let [base_details, head_details] = &details;
	print!(
		"{}",
		metrics::details(paired.then_some(base_details), head_details, &rates)
	);
	let sha = checkout.as_ref().map(|checkout| checkout.sha.as_str());
	if let Some(out) = &options.out {
		file(&times, &host, sha).write(out)?;
		eprintln!("swole: wrote {}", out.display());
	}
	if let Some(path) = &options.report {
		report(&times, &details, &seen, &host, &native.describe(), sha).write(path)?;
		eprintln!("swole: wrote {}", path.display());
	}
	Ok(())
}

/// What the process used over `samples`, in all, and how many samples that took, when its harness
/// says what it used.
fn usage<'a>(samples: impl Iterator<Item = &'a Sample>) -> Option<(perf::Usage, u64)> {
	let mut total = perf::Usage::default();
	let mut count = 0;
	for sample in samples {
		total.add(&sample.usage?);
		count += 1;
	}
	(count > 0).then_some((total, count))
}

/// What counting a benchmark found.
enum Counted {
	/// Each counter's share of a call.
	Measures(Vec<Measure>),
	/// Another thread ran beside a call, whose work the counters, reading the calling thread
	/// alone, would leave out.
	Threaded,
	/// The job's harness is too old to count, or to say whether another thread ran.
	Unwatched,
}

/// Counts `job`'s benchmark with each of `perf`'s passes, in a process of its own for each under
/// the library at `preload`, over `iterations` calls, its output under `dir`.
fn count(
	job: &Job,
	perf: &perf::Perf,
	preload: &Path,
	iterations: u64,
	dir: &Path,
) -> Result<(Counted, Option<String>), String> {
	let mut values = Vec::new();
	let mut libcs: Vec<(String, Option<String>)> = Vec::new();
	for (i, pass) in perf.passes.iter().enumerate() {
		let dir = dir.join(format!("pass-{}", i + 1));
		std::fs::create_dir_all(&dir).map_err(|e| format!("cannot create {}: {e}", dir.display()))?;
		let file = |name: &str| {
			std::fs::File::create(dir.join(name))
				.map_err(|e| format!("cannot create {}: {e}", dir.join(name).display()))
		};
		let mut command = job.command();
		perf::launch(&mut command, &perf.pmu, pass, false);
		command.env(perf::ITERATIONS, iterations.to_string());
		// Bench keeps ASLR and unpadded stacks; preload threads.so without 1 MiB padding.
		let env = crate::environment(&command, preload, &job.label, false)?;
		let status = command
			.env_clear()
			.envs(env)
			.stdin(Stdio::null())
			.stdout(file("stdout")?)
			.stderr(file("stderr")?)
			.status()
			.map_err(|e| format!("cannot run {}: {e}", job.label))?;
		if !status.success() {
			return Err(format!(
				"{} failed counting ({status}){}; its output is in {}",
				job.label,
				crate::reason(&dir.join("stderr")),
				dir.display()
			));
		}
		let path = dir.join("stdout");
		let text = crate::printed(&path)?;
		let readings = perf::readings(&text, pass.len()).map_err(|err| format!("{}: {err}", job.label))?;
		match readings.as_slice() {
			[] => {
				eprintln!(
					"swole: {}'s swole harness is too old to count, so it has no counters",
					job.label
				);
				return Ok((Counted::Unwatched, None));
			}
			[reading] => {
				let verified = reading
					.libc
					.as_deref()
					.map(crate::fingerprint::validate_libc_record)
					.transpose()?;
				libcs.push((job.label.clone(), verified));
				if reading.threaded {
					let libc = crate::fingerprint::require_single_libc(
						libcs.iter().map(|(label, libc)| (label.as_str(), libc.as_deref())),
						cfg!(target_os = "linux"),
					)?;
					return Ok((Counted::Threaded, libc));
				}
				// Over the calls the harness counted (stratified over the sample's iterations), or
				// `iterations` for a harness too old to emit `swole calls`.
				values.push((reading.values.clone(), reading.calls.unwrap_or(iterations)));
			}
			_ => {
				return Err(format!(
					"{} counted more than one benchmark; its output is in {}",
					job.label,
					dir.display()
				));
			}
		}
	}
	let libc = crate::fingerprint::require_single_libc(
		libcs.iter().map(|(label, libc)| (label.as_str(), libc.as_deref())),
		cfg!(target_os = "linux"),
	)?;
	Ok((Counted::Measures(perf::measures(&perf.passes, &values)), libc))
}

/// Jobs by the benchmark each runs.
fn by_name(jobs: Vec<Job>) -> BTreeMap<String, Job> {
	jobs.into_iter()
		.map(|job| (job.expect.clone().expect("a Cargo job names its benchmark"), job))
		.collect()
}

/// One sample of one side.
#[derive(Clone, Copy, Debug)]
struct Sample {
	/// What the benchmark's timed part took.
	nanos: u64,
	/// What the request took in all, steps and setup between calls included.
	wall: Duration,
	/// What the process used over the sample, when its harness says.
	usage: Option<perf::Usage>,
	/// Whether the timing process observed another thread alive during this sample.
	threaded: bool,
}

/// A benchmark set up in a process of one side, which runs a sample when asked.
trait Side {
	fn sample(&mut self, iterations: u64) -> Result<Sample, String>;
}

/// The iterations each sample takes: doubled from one until the timed part of a sample of either
/// side takes a slice, or the whole sample its most. The samples it takes warm both sides up, and
/// are dropped.
fn calibrate(sides: &mut [&mut dyn Side]) -> Result<u64, String> {
	let mut iterations = 1;
	loop {
		let mut full = false;
		for side in sides.iter_mut() {
			let sample = side.sample(iterations)?;
			full |= Duration::from_nanos(sample.nanos) >= SLICE || sample.wall >= WALL;
		}
		if full {
			return Ok(iterations);
		}
		if iterations >= MAX_ITERATIONS {
			return Err(format!(
				"{iterations} iterations took under {} ms: the benchmark does nothing the clock sees",
				SLICE.as_millis()
			));
		}
		iterations *= 2;
	}
}

/// Samples `side` alone until the budget is spent: the iterations each sample took, and each sample.
fn alone(side: &mut dyn Side) -> Result<(u64, Vec<Sample>), String> {
	let iterations = calibrate(&mut [&mut *side])?;
	let mut spent = Duration::ZERO;
	let mut samples = Vec::new();
	while samples.len() < MIN_SAMPLES || spent < BUDGET {
		let sample = side.sample(iterations)?;
		spent += sample.wall;
		samples.push(sample);
	}
	Ok((iterations, samples))
}

/// Samples `base` and `head` in pairs, in an order `coin` draws anew for each, until the budget is
/// spent: the iterations each sample took, and each pair, the base's first.
fn paired(base: &mut dyn Side, head: &mut dyn Side, coin: &mut Coin) -> Result<(u64, Vec<(Sample, Sample)>), String> {
	let iterations = calibrate(&mut [&mut *base, &mut *head])?;
	let mut spent = Duration::ZERO;
	let mut pairs = Vec::new();
	while pairs.len() < MIN_SAMPLES || spent < BUDGET {
		let (b, h) = if coin.flip() {
			let b = base.sample(iterations)?;
			(b, head.sample(iterations)?)
		} else {
			let h = head.sample(iterations)?;
			(base.sample(iterations)?, h)
		};
		spent += b.wall + h.wall;
		pairs.push((b, h));
	}
	Ok((iterations, pairs))
}

/// A stream of fair coin flips: xorshift64*, enough to order a pair, seeded apart in every run
/// so no order recurs with the host's own rhythm.
struct Coin(u64);

impl Coin {
	fn seeded() -> Self {
		use std::hash::BuildHasher;
		let seed = std::collections::hash_map::RandomState::new().hash_one(std::process::id());
		Self(seed | 1)
	}

	fn flip(&mut self) -> bool {
		self.0 ^= self.0 >> 12;
		self.0 ^= self.0 << 25;
		self.0 ^= self.0 >> 27;
		self.0.wrapping_mul(0x2545_f491_4f6c_dd1d) >> 63 == 1
	}
}

/// A median and a 95% confidence interval around it.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Estimate {
	value: f64,
	lower: f64,
	upper: f64,
}

impl Estimate {
	/// The median of `values` and a distribution-free 95% interval around it: the order statistics
	/// that a binomial count of values below the median puts it between, which a few samples a
	/// spike of load stretched cannot move.
	fn median(mut values: Vec<f64>) -> Self {
		values.sort_by(f64::total_cmp);
		let n = values.len();
		let value = if n % 2 == 1 {
			values[n / 2]
		} else {
			(values[n / 2 - 1] + values[n / 2]) / 2.0
		};
		let k = rank(n);
		Self {
			value,
			lower: values[k - 1],
			upper: values[n - k],
		}
	}

	fn map(self, f: impl Fn(f64) -> f64) -> Self {
		Self {
			value: f(self.value),
			lower: f(self.lower),
			upper: f(self.upper),
		}
	}
}

/// The largest `k` for which at most 2.5% of samples of `n` values put fewer than `k` below their
/// median: the `k`th smallest and `k`th largest then hold the median with 95% confidence or more.
fn rank(n: usize) -> usize {
	// P(X <= j) for X ~ Binomial(n, 1/2), each term in logs so none underflows before it matters.
	let mut log_term = -(n as f64) * std::f64::consts::LN_2;
	let mut below = 0.0;
	let mut k = 0;
	for j in 0..n {
		below += log_term.exp();
		if below > 0.025 {
			break;
		}
		k = j + 1;
		log_term += ((n - j) as f64 / (j + 1) as f64).ln();
	}
	assert!(k > 0, "{n} samples are too few for a 95% interval");
	k
}

/// Which side a time is of.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Which {
	Base,
	Head,
}

impl Which {
	fn name(self) -> &'static str {
		match self {
			Which::Base => "base",
			Which::Head => "head",
		}
	}
}

/// One benchmark's times, each side's median in nanoseconds an iteration: one side's, or both
/// sides' and the head's over the base's, with its interval. A side's own time has none: load
/// that slows many samples in a row makes them no independent evidence of it, while it slows
/// both of a pair alike.
#[derive(Debug, PartialEq)]
enum Times {
	Base(f64),
	Head(f64),
	Both { base: f64, head: f64, ratio: Estimate },
}

impl Times {
	/// The times of a benchmark one side has.
	fn alone(side: Which, iterations: u64, samples: &[u64]) -> Result<Self, String> {
		let time = Estimate::median(each(iterations, samples.iter().copied())?).value;
		Ok(match side {
			Which::Base => Self::Base(time),
			Which::Head => Self::Head(time),
		})
	}

	/// The times of a benchmark both sides have, from pairs of samples, the base's first.
	fn pairs(iterations: u64, pairs: &[(u64, u64)]) -> Result<Self, String> {
		let base = each(iterations, pairs.iter().map(|&(base, _)| base))?;
		let head = each(iterations, pairs.iter().map(|&(_, head)| head))?;
		// Each pair's ratio, in logs, so a pair twice as slow weighs as one twice as fast.
		let ratios = base.iter().zip(&head).map(|(b, h)| (h / b).ln()).collect();
		Ok(Self::Both {
			base: Estimate::median(base).value,
			head: Estimate::median(head).value,
			ratio: Estimate::median(ratios).map(f64::exp),
		})
	}

	fn base(&self) -> Option<f64> {
		match self {
			Self::Base(base) | Self::Both { base, .. } => Some(*base),
			Self::Head(_) => None,
		}
	}

	fn head(&self) -> Option<f64> {
		match self {
			Self::Head(head) | Self::Both { head, .. } => Some(*head),
			Self::Base(_) => None,
		}
	}

	/// A line on what it took, for the progress log.
	fn summary(&self) -> String {
		match self {
			Self::Both { ratio, .. } => change(*ratio),
			Self::Base(base) => format!("{} (base only)", duration(*base)),
			Self::Head(head) => duration(*head),
		}
	}
}

/// Each sample's nanoseconds an iteration, refusing a sample that took none, whose ratio to
/// another would be meaningless.
fn each(iterations: u64, samples: impl Iterator<Item = u64>) -> Result<Vec<f64>, String> {
	samples
		.map(|nanos| match nanos {
			0 => Err(format!("a sample of {iterations} iterations took no time")),
			nanos => Ok(nanos as f64 / iterations as f64),
		})
		.collect()
}

/// `nanos` in the unit that suits it.
fn duration(nanos: f64) -> String {
	let (value, unit) = match nanos {
		n if n < 1e3 => (n, "ns"),
		n if n < 1e6 => (n / 1e3, "µs"),
		n if n < 1e9 => (n / 1e6, "ms"),
		n => (n / 1e9, "s"),
	};
	let precision = match value {
		v if v >= 100.0 => 1,
		v if v >= 10.0 => 2,
		_ => 3,
	};
	format!("{value:.precision$} {unit}")
}

/// A ratio of times as the change it is, with its interval.
fn change(ratio: Estimate) -> String {
	let percent = |ratio: f64| format!("{:+.2}%", (ratio - 1.0) * 100.0);
	format!(
		"{} ({} to {})",
		percent(ratio.value),
		percent(ratio.lower),
		percent(ratio.upper)
	)
}

/// Each benchmark's time as a Markdown table, and with a base, the base's and the change.
fn table(times: &BTreeMap<String, Times>, paired: bool) -> String {
	let cell = |time: Option<f64>| time.map_or("-".to_owned(), duration);
	let mut out = if paired {
		String::from("| benchmark | base | head | change | 95% interval |\n|---|---:|---:|---:|---:|\n")
	} else {
		String::from("| benchmark | time |\n|---|---:|\n")
	};
	for (name, times) in times {
		// A `|` in a name would split its cell.
		let shown = name.replace('|', "\\|");
		if paired {
			let (change, interval) = match times {
				Times::Both { ratio, .. } => {
					let percent = |ratio: f64| format!("{:+.2}%", (ratio - 1.0) * 100.0);
					(
						percent(ratio.value),
						format!("{} to {}", percent(ratio.lower), percent(ratio.upper)),
					)
				}
				Times::Base(_) => ("removed".to_owned(), String::new()),
				Times::Head(_) => ("added".to_owned(), String::new()),
			};
			writeln!(
				out,
				"| {shown} | {} | {} | {change} | {interval} |",
				cell(times.base()),
				cell(times.head())
			)
		} else {
			let head = times.head().expect("an unpaired run times the working tree's alone");
			writeln!(out, "| {shown} | {} |", duration(head))
		}
		.expect("a String takes every write");
	}
	out
}

/// The times as a `metrics.json`: the working tree's as the run's own commit's, and the base's,
/// when there is one, as its commit's.
fn file(times: &BTreeMap<String, Times>, host: &str, base: Option<&str>) -> metrics::File {
	let values = |side: fn(&Times) -> Option<f64>| -> BTreeMap<String, metrics::Point> {
		times
			.iter()
			.filter_map(|(name, times)| side(times).map(|time| (name.clone(), number(time).into())))
			.collect()
	};
	let mut samples = Vec::new();
	if let Some(base) = base {
		samples.push(metrics::Sample {
			commit: Some(base.to_owned()),
			values: values(Times::base),
		});
	}
	samples.push(metrics::Sample {
		commit: None,
		values: values(Times::head),
	});
	metrics::File {
		measure: format!("time ({host})"),
		unit: UNIT,
		samples,
	}
}

/// Every measure of each benchmark, its time and the change's interval among them, as `--report`
/// writes it, the base's from the commit `base` when there is one.
fn report(
	times: &BTreeMap<String, Times>,
	details: &[BTreeMap<String, Vec<Measure>>; 2],
	// Whether another thread ran beside each benchmark, where that is known.
	seen: &BTreeMap<String, bool>,
	host: &str,
	describe: &str,
	base: Option<&str>,
) -> metrics::Report {
	let mut benchmarks = BTreeMap::new();
	for (name, timed) in times {
		let mut entry = serde_json::Map::new();
		for (i, (side, time)) in [("base", timed.base()), ("head", timed.head())].into_iter().enumerate() {
			let Some(time) = time else { continue };
			let mut measures = vec![Measure::new("time", time, UNIT)];
			measures.extend(details[i].get(name).cloned().unwrap_or_default());
			entry.insert(side.to_owned(), metrics::side(&measures));
		}
		if let Some(&threaded) = seen.get(name) {
			entry.insert("threaded".to_owned(), threaded.into());
		}
		if let Times::Both { ratio, .. } = timed {
			entry.insert(
				"change".to_owned(),
				serde_json::json!({
					"value": metrics::number(ratio.value),
					"lower": metrics::number(ratio.lower),
					"upper": metrics::number(ratio.upper),
					"unit": "head/base",
				}),
			);
		}
		benchmarks.insert(name.clone(), entry);
	}
	metrics::Report::new(Mode::Bench, host, describe, base, benchmarks)
}

/// A time to five significant digits, more than load lets a run resolve.
fn number(time: f64) -> Number {
	let rounded: f64 = format!("{time:.4e}").parse().expect("a float reads back");
	Number::from_f64(rounded).expect("a time is finite")
}

/// A harness target's process timing one benchmark, set up once, a sample for each request.
struct Process {
	label: String,
	dir: PathBuf,
	child: std::process::Child,
	stdin: ChildStdin,
	stdout: BufReader<ChildStdout>,
	/// The target's own output, apart from its samples.
	log: std::fs::File,
	/// Whether any sample (including warmup/calibration) observed another thread alive or started.
	threaded: bool,
	/// The loaded libc identity reported by this benchmark process on Linux.
	libc: Option<String>,
}

impl Process {
	/// Starts `job`'s target timing its benchmark, pinned to `pmu`'s CPUs when given so timing and
	/// counters measure the same cores on hybrid hosts, preloading `preload` (threads-only) when
	/// perf counters are available so short-lived workers spawned during timed samples are detected,
	/// its output in `dir`, and waits until its benchmark is set up: a sample of one call, which
	/// no time keeps, answers only after setup, so no kept sample's wall time takes it in.
	fn spawn(job: &Job, pmu: Option<&perf::Pmu>, preload: Option<&Path>, dir: &Path) -> Result<Self, String> {
		let label = job.label.clone();
		let mut command = job.command();
		if let Some(pmu) = pmu {
			perf::pin_command(&mut command, pmu);
		}
		command.env(cargo::HARNESS_TIME, "1");
		if let Some(preload) = preload {
			let env = crate::environment(&command, preload, &label, false)?;
			command.env_clear().envs(env);
		}
		std::fs::create_dir_all(dir).map_err(|e| format!("cannot create {}: {e}", dir.display()))?;
		let file = |name: &str| {
			std::fs::File::create(dir.join(name))
				.map_err(|e| format!("cannot create {}: {e}", dir.join(name).display()))
		};
		let mut child = command
			.stdin(Stdio::piped())
			.stdout(Stdio::piped())
			.stderr(file("stderr")?)
			.spawn()
			.map_err(|e| format!("cannot run {label}: {e}"))?;
		let stdin = child.stdin.take().expect("stdin is piped");
		let stdout = BufReader::new(child.stdout.take().expect("stdout is piped"));
		let mut process = Self {
			label,
			dir: dir.to_owned(),
			child,
			stdin,
			stdout,
			log: file("stdout")?,
			threaded: false,
			libc: None,
		};
		let first = process.sample(1)?;
		process.threaded |= first.threaded;
		Ok(process)
	}

	/// Why the process stopped answering: how it exited, and where its output is.
	fn failure(&mut self) -> String {
		let status = match self.child.wait() {
			// A harness from before timing runs the benchmark once and exits cleanly.
			Ok(status) if status.success() => {
				"it exited without timing, as a swole harness too old to time does".to_owned()
			}
			Ok(status) => status.to_string(),
			Err(e) => format!("cannot wait for it: {e}"),
		};
		format!(
			"{} stopped timing ({status}); its output is in {}",
			self.label,
			self.dir.display()
		)
	}

	/// Ends the benchmark: closes its requests, keeps the rest of its output, and fails unless it
	/// exits cleanly.
	fn finish(self) -> Result<(), String> {
		let Self {
			label,
			dir,
			mut child,
			stdin,
			mut stdout,
			mut log,
			..
		} = self;
		drop(stdin);
		std::io::copy(&mut stdout, &mut log).map_err(|e| format!("cannot keep {label}'s output: {e}"))?;
		let status = child.wait().map_err(|e| format!("cannot wait for {label}: {e}"))?;
		if !status.success() {
			return Err(format!("{label} failed ({status}); its output is in {}", dir.display()));
		}
		Ok(())
	}
}

impl Side for Process {
	fn sample(&mut self, iterations: u64) -> Result<Sample, String> {
		let started = Instant::now();
		if writeln!(self.stdin, "{iterations}")
			.and_then(|()| self.stdin.flush())
			.is_err()
		{
			return Err(self.failure());
		}
		// Bytes, since the benchmark's own output need not be UTF-8.
		let mut line = Vec::new();
		let mut usage = None;
		let mut threaded = false;
		loop {
			line.clear();
			match self.stdout.read_until(b'\n', &mut line) {
				Ok(0) => return Err(self.failure()),
				Ok(_) => {}
				Err(e) => {
					// It may be waiting on its next request, so stop it rather than wait.
					let _ = self.child.kill();
					let failure = self.failure();
					return Err(format!("cannot read {}'s output ({e}): {failure}", self.label));
				}
			}
			if let Some(nanos) = line
				.strip_prefix(SAMPLE.as_bytes())
				.and_then(|rest| rest.strip_suffix(b"\n"))
			{
				let nanos = std::str::from_utf8(nanos)
					.ok()
					.and_then(|nanos| nanos.parse().ok())
					.ok_or_else(|| {
						format!(
							"{} printed a sample of {:?} nanoseconds",
							self.label,
							String::from_utf8_lossy(nanos)
						)
					})?;
				self.threaded |= threaded;
				return Ok(Sample {
					nanos,
					wall: started.elapsed(),
					usage: usage.take(),
					threaded,
				});
			}
			if let Some(fields) = line
				.strip_prefix(perf::RUSAGE.as_bytes())
				.and_then(|rest| rest.strip_suffix(b"\n"))
			{
				let fields = String::from_utf8_lossy(fields);
				usage = Some(perf::Usage::parse(&fields).map_err(|err| format!("{}: {err}", self.label))?);
				continue;
			}
			if let Some(raw) = line
				.strip_prefix(perf::LIBC.as_bytes())
				.and_then(|rest| rest.strip_suffix(b"\n"))
			{
				let raw =
					std::str::from_utf8(raw).map_err(|_| format!("{}: printed a non-UTF-8 libc record", self.label))?;
				let resolved =
					crate::fingerprint::validate_libc_record(raw).map_err(|err| format!("{}: {err}", self.label))?;
				if let Some(existing) = &self.libc
					&& existing != &resolved
				{
					return Err(format!(
						"{}: reported conflicting libc builds ({existing}, {resolved})",
						self.label
					));
				}
				self.libc = Some(resolved);
				continue;
			}
			if line.starts_with(perf::THREADED.as_bytes()) {
				threaded = true;
				continue;
			}
			self.log
				.write_all(&line)
				.map_err(|e| format!("cannot keep {}'s output: {e}", self.label))?;
		}
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	/// A side whose samples take what `nanos` says an iteration takes, given the sample's index,
	/// and `step` nanoseconds more an iteration untimed, recording each request in a log shared
	/// with another side.
	struct Fake<'a> {
		name: &'static str,
		nanos: fn(usize) -> u64,
		step: u64,
		log: &'a std::cell::RefCell<Vec<(&'static str, u64)>>,
	}

	impl Side for Fake<'_> {
		fn sample(&mut self, iterations: u64) -> Result<Sample, String> {
			let mut log = self.log.borrow_mut();
			let index = log.iter().filter(|(name, _)| *name == self.name).count();
			log.push((self.name, iterations));
			let nanos = (self.nanos)(index) * iterations;
			Ok(Sample {
				nanos,
				wall: Duration::from_nanos(nanos + self.step * iterations),
				usage: None,
				threaded: false,
			})
		}
	}

	#[cfg(unix)]
	#[test]
	fn keeps_a_benchmark_output_that_is_not_utf8() {
		// Beside the test's own executable, in the build's target directory, not a shared one.
		let exe = std::env::current_exe().unwrap();
		let dir = exe.with_file_name(format!("swole-bytes-{}", std::process::id()));
		let mut command = std::process::Command::new("sh");
		command.args(["-c", r"while read n; do printf '\377\n'; echo swole sample $n; done"]);
		let job = Job {
			label: "bytes".to_owned(),
			command,
			expect: None,
			codspeed: false,
			throughput: None,
		};
		let mut process = Process::spawn(&job, None, None, &dir).unwrap();
		assert_eq!(process.sample(7).unwrap().nanos, 7);
		process.finish().unwrap();
		assert_eq!(std::fs::read(dir.join("stdout")).unwrap(), b"\xff\n\xff\n");
		std::fs::remove_dir_all(&dir).unwrap();
	}

	#[test]
	fn calibrates_until_either_side_fills_a_slice() {
		let log = Default::default();
		let mut slow = Fake {
			name: "slow",
			nanos: |_| 1_000_000,
			step: 0,
			log: &log,
		};
		let mut fast = Fake {
			name: "fast",
			nanos: |_| 10_000,
			step: 0,
			log: &log,
		};
		assert_eq!(calibrate(&mut [&mut fast, &mut slow]).unwrap(), 1);
		assert_eq!(calibrate(&mut [&mut fast]).unwrap(), 128);
		// A step of 10 ms an iteration fills a sample's whole by eight iterations.
		let mut stepped = Fake {
			name: "stepped",
			nanos: |_| 1_000,
			step: 10_000_000,
			log: &log,
		};
		assert_eq!(calibrate(&mut [&mut stepped]).unwrap(), 8);
		let mut nothing = Fake {
			name: "nothing",
			nanos: |_| 0,
			step: 0,
			log: &log,
		};
		assert!(calibrate(&mut [&mut nothing]).unwrap_err().contains("does nothing"));
	}

	#[test]
	fn pairs_samples_in_both_orders_until_the_budget_is_spent() {
		let log = Default::default();
		let mut base = Fake {
			name: "base",
			nanos: |_| 1_000_000,
			step: 0,
			log: &log,
		};
		// A drift that slows each sample, which the pairs see on both sides alike.
		let mut head = Fake {
			name: "head",
			nanos: |index| 1_000_000 + index as u64,
			step: 0,
			log: &log,
		};
		let mut coin = Coin(0x9e37_79b9_7f4a_7c15);
		let (iterations, pairs) = paired(&mut base, &mut head, &mut coin).unwrap();
		assert_eq!(iterations, 1);
		// Each pair takes 2 ms, so 500 of them spend the budget.
		assert_eq!(pairs.len(), 500);
		let log = log.borrow();
		let measured = &log[log.len() - 2 * pairs.len()..];
		let firsts: Vec<&str> = measured.chunks(2).map(|pair| pair[0].0).collect();
		assert!(firsts.contains(&"base") && firsts.contains(&"head"), "{firsts:?}");
		for pair in measured.chunks(2) {
			assert_ne!(pair[0].0, pair[1].0, "a pair holds one sample of each side");
		}
	}

	#[test]
	fn takes_the_fewest_samples_however_long_each_takes() {
		let log = Default::default();
		let mut slow = Fake {
			name: "slow",
			nanos: |_| 1_000_000_000,
			step: 0,
			log: &log,
		};
		let (iterations, samples) = alone(&mut slow).unwrap();
		assert_eq!((iterations, samples.len()), (1, MIN_SAMPLES));
	}

	#[test]
	fn ranks_the_order_statistics_a_95_percent_interval_takes() {
		// P(X <= 0) = 1/64 for six, P(X <= 1) = 11/1024 for ten, and for a hundred, P(X <= 39)
		// is 1.8% and P(X <= 40) 2.8%.
		assert_eq!(rank(6), 1);
		assert_eq!(rank(10), 2);
		assert_eq!(rank(100), 40);
		// Enough samples that their first terms underflow.
		let k = rank(5000);
		assert!((2429..2432).contains(&k), "{k}");
	}

	#[test]
	fn estimates_a_median_its_extremes_cannot_move() {
		let mut values: Vec<f64> = (1..=10).map(f64::from).collect();
		values[9] = 1e9;
		assert_eq!(
			Estimate::median(values),
			Estimate {
				value: 5.5,
				lower: 2.0,
				upper: 9.0
			}
		);
	}

	#[test]
	fn a_pair_compares_in_ratios_of_its_own_samples() {
		// The base drifts from 100 to 200 ns; the head is 10% slower than the base beside it.
		let pairs: Vec<(u64, u64)> = (0..10).map(|i| (100 + 10 * i, (110 + 11 * i))).collect();
		let times = Times::pairs(1, &pairs).unwrap();
		let Times::Both { ratio, .. } = times else {
			panic!("{times:?}")
		};
		for value in [ratio.value, ratio.lower, ratio.upper] {
			assert!((value - 1.1).abs() < 1e-9, "{ratio:?}");
		}
		assert!(Times::pairs(1, &[(0, 1)]).unwrap_err().contains("took no time"));
	}

	#[test]
	fn tables_and_writes_each_side() {
		let estimate = |value: f64| Estimate {
			value,
			lower: value * 0.9,
			upper: value * 1.1,
		};
		let times = BTreeMap::from([
			(
				"a".to_owned(),
				Times::Both {
					base: 2000.0,
					head: 1500.0,
					ratio: estimate(0.75),
				},
			),
			("gone".to_owned(), Times::Base(5.0)),
			("new|x".to_owned(), Times::Head(12_345_678.0)),
		]);
		assert_eq!(
			table(&times, true),
			"| benchmark | base | head | change | 95% interval |\n|---|---:|---:|---:|---:|\n\
			 | a | 2.000 µs | 1.500 µs | -25.00% | -32.50% to -17.50% |\n\
			 | gone | 5.000 ns | - | removed |  |\n\
			 | new\\|x | - | 12.35 ms | added |  |\n"
		);
		let base = "c".repeat(40);
		let json = file(&times, "x86_64-0123456789ab", Some(&base)).json().unwrap();
		let json: serde_json::Value = serde_json::from_str(&json).unwrap();
		let measure = "time (x86_64-0123456789ab)";
		assert_eq!(json["units"][measure], "ns");
		assert_eq!(json["samples"][0]["commit"], base);
		assert_eq!(json["samples"][0]["metrics"]["gone"][measure]["value"], 5.0);
		assert_eq!(
			json["samples"][0]["metrics"]["a"][measure],
			serde_json::json!({ "value": 2000.0 })
		);
		assert!(json["samples"][1].get("commit").is_none());
		assert_eq!(json["samples"][1]["metrics"]["new|x"][measure]["value"], 12_346_000.0);
		assert!(json["samples"][1]["metrics"].get("gone").is_none());
	}
}
