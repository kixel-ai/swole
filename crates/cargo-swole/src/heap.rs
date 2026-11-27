//! Each benchmark's heap allocations, counted in a pass of its own: one call, after its setup,
//! under swole's allocation interposer, which only this pass loads, so its hooks' instructions
//! stay out of every other measure. A call beside which another thread ran is sampled: the pass
//! repeats, and each measure is the median of its repeats, with their least and most.

use std::path::{Path, PathBuf};
use std::process::Stdio;

use serde_json::{Map, Value};

use crate::{Job, Measure, SAMPLES, perf};

/// The variable that has a harness target count its benchmark's allocations.
const HEAP: &str = "SWOLE_HEAP";

/// What a harness target prints before what the interposer counted, and the benchmark's name.
const ALLOCS: &str = "swole allocs ";

/// What a harness target prints after [`ALLOCS`] for a benchmark whose allocations never reach
/// the interposer.
const BYPASSED: &str = "bypassed ";

/// The measures, a call's allocations, their bytes, and the most bytes the call's own
/// allocations held at once, each with its unit.
pub const MEASURES: [(&str, &str); 3] = [
	("alloc_count", "allocations"),
	("alloc_bytes", "bytes"),
	("peak_live_bytes", "bytes"),
];

/// What a benchmark's allocation pass found.
#[derive(Debug, PartialEq)]
pub enum Allocs {
	/// Each of [`MEASURES`], with the least and most of its repeats when other threads ran.
	Counted(Vec<Measure>),
	/// The program's global allocator never reaches the interposer, so it has no counts.
	Bypassed,
	/// Its harness predates allocation counts.
	Unwatched,
}

/// The interposer for a pass beside a count, with its randomness and clock when `shimmed`, or
/// beside a time, without them; `None` where swole has none to load: macOS has only a time's.
pub fn library(shimmed: bool) -> Result<Option<PathBuf>, String> {
	let built = cfg!(all(target_os = "linux", target_arch = "x86_64", target_env = "gnu"))
		|| cfg!(target_os = "macos") && !shimmed;
	built.then(|| crate::preload::heap(shimmed)).transpose()
}

/// How an allocation pass launches, as the pass beside it does.
#[derive(Clone, Copy)]
pub struct Pass<'a> {
	/// The interposer, from [`library`].
	pub library: &'a Path,
	/// Whether the launch is padded, as a count's.
	pub padded: bool,
	/// The CPUs the pass beside it runs on, if pinned, so a benchmark that sizes its work by the
	/// CPUs it may use allocates for the same work.
	pub cpus: Option<&'a [usize]>,
}

/// Counts `job`'s benchmark's allocations as `pass` launches, each run's output under `dir`. Each
/// run must declare `throughput` as the process that timed or counted the benchmark did, since
/// the allocations are divided by what that one declared.
pub fn measure(
	job: &Job,
	pass: Pass,
	throughput: Option<crate::metrics::Throughput>,
	dir: &Path,
) -> Result<Allocs, String> {
	let name = job.expect.as_deref().expect("a harness job names its benchmark");
	let mut runs = Vec::new();
	for run in 1..=SAMPLES {
		let dir = dir.join(format!("heap-{run}"));
		let (counted, threaded) = match once(job, pass, throughput, &dir, name)? {
			Some(Record::Counted(counted, threaded)) => (counted, threaded),
			Some(Record::Bypassed) => {
				eprintln!(
					"swole: {name}'s global allocator never reaches swole's allocation interposer, as one that maps \
					 memory of its own or a static malloc does, so it has no allocation counts"
				);
				return Ok(Allocs::Bypassed);
			}
			None => {
				eprintln!(
					"swole: {}'s swole harness is too old to count allocations, so it has none",
					job.label
				);
				return Ok(Allocs::Unwatched);
			}
		};
		runs.push(counted);
		// A call alone repeats exactly; one beside another thread is sampled.
		if !threaded && run == 1 {
			break;
		}
	}
	Ok(Allocs::Counted(measures(&runs)))
}

/// What a harness target said of a benchmark's allocations.
#[derive(Debug, PartialEq)]
enum Record {
	/// The allocations, their bytes, and the most bytes live at once, and whether another thread
	/// ran beside the call.
	Counted([u64; 3], bool),
	/// Its global allocator never reaches the interposer.
	Bypassed,
}

/// One run of the pass, or `None` from a harness too old to count allocations.
fn once(
	job: &Job,
	pass: Pass,
	throughput: Option<crate::metrics::Throughput>,
	dir: &Path,
	name: &str,
) -> Result<Option<Record>, String> {
	std::fs::create_dir_all(dir).map_err(|e| format!("cannot create {}: {e}", dir.display()))?;
	let file = |name: &str| {
		std::fs::File::create(dir.join(name)).map_err(|e| format!("cannot create {}: {e}", dir.join(name).display()))
	};
	let mut command = job.command();
	command
		.env(HEAP, "1")
		.env_remove(perf::COUNTERS)
		.env_remove(perf::ITERATIONS);
	if let Some(cpus) = pass.cpus {
		perf::pin_command(&mut command, cpus);
	}
	let env = crate::environment(&command, pass.library, &job.label, pass.padded)?;
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
			"{} failed counting allocations ({status}){}; its output is in {}",
			job.label,
			crate::reason(&dir.join("stderr")),
			dir.display()
		));
	}
	let stdout = crate::printed(&dir.join("stdout"))?;
	let declared = crate::metrics::Throughput::declared(&stdout).map_err(|err| format!("{}: {err}", job.label))?;
	if declared.get(name).copied() != throughput {
		return Err(format!(
			"{} declared another throughput (or none) counting allocations than measuring; its output is in {}",
			job.label,
			dir.display()
		));
	}
	let record =
		parse(&stdout, name).map_err(|err| format!("{}: {err}; its output is in {}", job.label, dir.display()))?;
	let Some(Record::Counted(counted, _)) = record else {
		return Ok(record);
	};
	// The harness says whether another thread ran, unless a line the benchmark printed without a
	// newline swallowed it, which must not pass for none having run.
	let threaded = *perf::threads(&stdout).get(name).ok_or_else(|| {
		format!(
			"{} counted {name}'s allocations but said neither whether another thread ran beside it: a line the \
			 benchmark printed without a newline ran into it; its output is in {}",
			job.label,
			dir.display()
		)
	})?;
	Ok(Some(Record::Counted(counted, threaded)))
}

/// What a harness target's `stdout` says the interposer counted of `name`, or `None` without a
/// record, as from a harness too old to count; whether another thread ran is left for the caller.
fn parse(stdout: &str, name: &str) -> Result<Option<Record>, String> {
	let mut found = None;
	for rest in stdout.lines().filter_map(|line| line.strip_prefix(ALLOCS)) {
		let malformed = || format!("a harness target printed {ALLOCS}{rest}, which no harness prints");
		let (record, of) = match rest.strip_prefix(BYPASSED) {
			Some(of) => (Record::Bypassed, of),
			None => {
				let mut fields = rest.splitn(4, ' ');
				let mut value = || fields.next().and_then(|n| n.parse::<u64>().ok()).ok_or_else(malformed);
				let counted = [value()?, value()?, value()?];
				(Record::Counted(counted, false), fields.next().ok_or_else(malformed)?)
			}
		};
		if of != name {
			return Err(format!("counted the allocations of {of}, not {name}"));
		}
		if found.replace(record).is_some() {
			return Err(format!("counted {name}'s allocations twice"));
		}
	}
	Ok(found)
}

/// Each of [`MEASURES`] over `runs`: the one run's, or the median of several, with their least and
/// most.
fn measures(runs: &[[u64; 3]]) -> Vec<Measure> {
	MEASURES
		.iter()
		.enumerate()
		.map(|(i, (name, unit))| {
			let mut values: Vec<u64> = runs.iter().map(|run| run[i]).collect();
			values.sort_unstable();
			let mut measure = Measure::new(name, values[values.len() / 2] as f64, unit);
			if values.len() > 1 {
				measure.spread = Some((values[0] as f64, values[values.len() - 1] as f64));
			}
			measure
		})
		.collect()
}

/// Labels the allocation measures of a report's side, `side`, built from `measures`: each as the
/// allocation pass's own, sampled or exact, and beside them, the call's churn, the bytes it
/// allocated for each of the most it held at once.
pub fn label(measures: &[Measure], side: &mut Map<String, Value>) {
	let find = |name: &str| measures.iter().find(|measure| measure.name == name);
	for (name, _) in MEASURES {
		if let (Some(measure), Some(Value::Object(entry))) = (find(name), side.get_mut(name)) {
			entry.insert("pass".to_owned(), "allocation".into());
			entry.insert("sampled".to_owned(), measure.spread.is_some().into());
		}
	}
	if let (Some(bytes), Some(peak)) = (find("alloc_bytes"), find("peak_live_bytes"))
		&& peak.value > 0.0
	{
		side.insert(
			"alloc_churn".to_owned(),
			serde_json::json!({
				"value": crate::metrics::number(bytes.value / peak.value),
				"unit": "bytes/peak byte",
				"pass": "allocation",
				"sampled": bytes.spread.is_some(),
			}),
		);
	}
}

/// The label of allocation counts taken beside a measure of `fingerprint`, with the interposer's
/// hash after it, since how the interposer counts can move them as the mode's environment does
/// its own: the shimmed one's beside a count, else the one beside a time.
pub fn fingerprint(fingerprint: &str, shimmed: bool) -> String {
	format!("{fingerprint}+{}", crate::preload::heap_hash(shimmed))
}

/// The allocation measures among a benchmark's `measures`, each by the name `metrics.json` gives
/// it under `fingerprint`, from [`fingerprint`], with its unit.
pub fn series<'a>(measures: &'a [Measure], fingerprint: &str) -> Vec<(String, &'static str, &'a Measure)> {
	MEASURES
		.iter()
		.filter_map(|&(name, unit)| {
			let measure = measures.iter().find(|measure| measure.name == name)?;
			Some((format!("{name} ({fingerprint})"), unit, measure))
		})
		.collect()
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn parses_the_one_record_of_its_benchmark() {
		let out = "noise\nswole alone a\nswole allocs 3 96 64 a\nswole ran a\n";
		assert_eq!(parse(out, "a").unwrap(), Some(Record::Counted([3, 96, 64], false)));
		assert_eq!(
			parse("swole allocs bypassed a b\n", "a b").unwrap(),
			Some(Record::Bypassed)
		);
		assert_eq!(parse("swole ran a\n", "a").unwrap(), None);
		assert!(parse("swole allocs 3 96 64 b\n", "a").unwrap_err().contains("not a"));
		assert!(
			parse("swole allocs 1 2 3 a\nswole allocs 1 2 3 a\n", "a")
				.unwrap_err()
				.contains("twice")
		);
		for bad in [
			"swole allocs 3 96 a\n",
			"swole allocs x 1 2 a\n",
			"swole allocs 1 2 3\n",
		] {
			assert!(parse(bad, "a").is_err(), "{bad}");
		}
	}

	#[test]
	fn one_run_is_exact_and_several_are_their_median_and_spread() {
		let exact = measures(&[[3, 96, 64]]);
		assert_eq!(exact.iter().map(|m| m.value).collect::<Vec<_>>(), [3.0, 96.0, 64.0]);
		assert!(exact.iter().all(|m| m.spread.is_none()));
		let sampled = measures(&[[5, 10, 8], [3, 30, 4], [4, 20, 6]]);
		assert_eq!(sampled[0].value, 4.0);
		assert_eq!(sampled[0].spread, Some((3.0, 5.0)));
		assert_eq!(sampled[1].spread, Some((10.0, 30.0)));
		assert_eq!(sampled[2].value, 6.0);
	}

	#[test]
	fn labels_each_measure_and_adds_churn() {
		let mut measures = measures(&[[2, 300, 100]]);
		measures.push(Measure::new("time", 1.0, "ns"));
		let mut side = crate::metrics::side(&measures);
		let Value::Object(side) = &mut side else { unreachable!() };
		label(&measures, side);
		assert_eq!(side["alloc_count"]["pass"], "allocation");
		assert_eq!(side["peak_live_bytes"]["sampled"], false);
		assert_eq!(side["alloc_churn"]["value"], 3);
		assert!(side["time"].get("pass").is_none());
	}
}
