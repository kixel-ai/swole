//! Measures as Kixel Quest's `metrics.json`, as a full report, and as tables to read.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write;
use std::path::Path;

use serde_json::{Map, Number, Value, json};

use crate::{Counter, Counts, Measure};

/// The most bytes of `metrics.json` Kixel Quest reads from one run attempt.
const LIMIT: usize = 1 << 20;
/// The longest benchmark name Kixel Quest takes, in characters.
const NAME_LIMIT: usize = 1024;
/// The longest measure name Kixel Quest takes, in characters.
const MEASURE_LIMIT: usize = 64;

/// The version of `--report`'s JSON, which changes only when a field changes meaning or goes;
/// a new field keeps it.
const SCHEMA_VERSION: u64 = 1;

/// What a harness target prints before what each call of a benchmark processes.
pub const THROUGHPUT: &str = "swole throughput ";

/// What each call of a benchmark processes, as its `#[bench(bytes = ..)]` or `items` declares.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Throughput {
	Bytes(u64),
	Items(u64),
}

impl Throughput {
	/// What the harness prints after `swole throughput `: `bytes` or `items`, then how many.
	fn parse(unit: &str, amount: &str) -> Option<Self> {
		let amount = amount.parse().ok().filter(|&n| n > 0)?;
		match unit {
			"bytes" => Some(Self::Bytes(amount)),
			"items" => Some(Self::Items(amount)),
			_ => None,
		}
	}

	/// Each benchmark's throughput as a harness target's `stdout` declares it, a
	/// `swole throughput <bytes|items> <n> <name>` line each, failing on one no harness prints,
	/// or two that disagree.
	pub fn declared(stdout: &str) -> Result<BTreeMap<String, Self>, String> {
		let mut declared = BTreeMap::new();
		for rest in stdout.lines().filter_map(|line| line.strip_prefix(THROUGHPUT)) {
			let (name, throughput) = Self::record(rest)?;
			Self::agree(&mut declared, name, throughput)?;
		}
		Ok(declared)
	}

	/// The benchmark and throughput a `swole throughput ` line declares after its prefix.
	pub fn record(rest: &str) -> Result<(&str, Self), String> {
		let malformed = || format!("a harness target printed {THROUGHPUT}{rest}, which no harness prints");
		let mut fields = rest.splitn(3, ' ');
		let (Some(unit), Some(amount), Some(name)) = (fields.next(), fields.next(), fields.next()) else {
			return Err(malformed());
		};
		Ok((name, Self::parse(unit, amount).ok_or_else(malformed)?))
	}

	/// Records `throughput` as `name`'s in `declared`, failing if it already holds another.
	pub fn agree(declared: &mut BTreeMap<String, Self>, name: &str, throughput: Self) -> Result<(), String> {
		match declared.insert(name.to_owned(), throughput) {
			Some(other) if other != throughput => {
				Err(format!("{name} declared two throughputs, {other:?} and {throughput:?}"))
			}
			_ => Ok(()),
		}
	}

	/// How many times what `base`'s calls each process this one's do, when both count the same
	/// unit.
	pub fn scale(self, base: Self) -> Option<f64> {
		match (self, base) {
			(Self::Bytes(_), Self::Bytes(_)) | (Self::Items(_), Self::Items(_)) => Some(self.amount() / base.amount()),
			_ => None,
		}
	}

	/// How many bytes or items each call processes.
	fn amount(self) -> f64 {
		match self {
			Self::Bytes(n) | Self::Items(n) => n as f64,
		}
	}

	/// What derives from a call's `measures`: each count's share of a byte or an item, and from
	/// its native wall-clock `time`, how many it processes a second. A count or time the run did
	/// not measure derives nothing.
	pub fn measures(self, measures: &[Measure]) -> Vec<Measure> {
		let bytes = matches!(self, Self::Bytes(_));
		let mut out = Vec::new();
		for measure in measures {
			let per = match (measure.unit, bytes) {
				("instructions", true) => "instructions/byte",
				("instructions", false) => "instructions/item",
				("cycles", true) => "cycles/byte",
				("cycles", false) => "cycles/item",
				("events", true) => "events/byte",
				("events", false) => "events/item",
				("ns", _) if measure.name == "time" => {
					if measure.value > 0.0 {
						let rate = if bytes { "bytes/s" } else { "items/s" };
						out.push(Measure::new("throughput", self.amount() * 1e9 / measure.value, rate));
					}
					continue;
				}
				_ => continue,
			};
			let name = format!("{}/{}", measure.name, if bytes { "byte" } else { "item" });
			out.push(Measure::new(&name, measure.value / self.amount(), per));
		}
		out
	}
}

/// A `metrics.json`: one measure, in one unit, of each sample's benchmarks.
pub struct File {
	pub measure: String,
	pub unit: &'static str,
	pub samples: Vec<Sample>,
}

/// One commit's values: the run's own, or another's, such as a paired comparison's base.
pub struct Sample {
	/// The commit's full ID, when it is not the run's.
	pub commit: Option<String>,
	/// Each benchmark's value.
	pub values: BTreeMap<String, Point>,
}

/// One benchmark's value, and the least and most of its samples, when it took several.
pub struct Point {
	pub value: Number,
	pub spread: Option<(Number, Number)>,
}

impl From<Number> for Point {
	fn from(value: Number) -> Self {
		Self { value, spread: None }
	}
}

impl File {
	/// `counts` as one sample of the run's own commit, `measure` per benchmark.
	pub fn counts(counts: &Counts, measure: String) -> Self {
		let values = counts
			.counts
			.iter()
			.map(|(name, &count)| {
				let spread = counts
					.spread
					.get(name)
					.map(|&(least, most)| (least.into(), most.into()));
				(
					name.clone(),
					Point {
						value: count.into(),
						spread,
					},
				)
			})
			.collect();
		Self {
			measure,
			unit: "instructions",
			samples: vec![Sample { commit: None, values }],
		}
	}

	/// The file's JSON, refusing what Kixel Quest would.
	pub fn json(&self) -> Result<String, String> {
		let measure = &self.measure;
		if measure.chars().count() > MEASURE_LIMIT {
			return Err(format!("the measure {measure} is longer than Kixel Quest takes"));
		}
		let mut samples = Vec::new();
		for sample in &self.samples {
			let mut metrics = Map::new();
			for (name, point) in &sample.values {
				if name.is_empty() || name.chars().count() > NAME_LIMIT || name.chars().any(char::is_control) {
					return Err(format!("Kixel Quest takes no benchmark named {name:?}"));
				}
				let mut value = json!({ "value": point.value });
				if let Some((lower, upper)) = &point.spread {
					value["lower_value"] = lower.clone().into();
					value["upper_value"] = upper.clone().into();
				}
				metrics.insert(name.clone(), json!({ measure: value }));
			}
			if metrics.is_empty() {
				continue;
			}
			let mut sample_json = Map::new();
			if let Some(commit) = &sample.commit {
				sample_json.insert("commit".to_owned(), commit.clone().into());
			}
			sample_json.insert("metrics".to_owned(), Value::Object(metrics));
			samples.push(Value::Object(sample_json));
		}
		if samples.is_empty() {
			return Err("no benchmark to write".to_owned());
		}
		let file = json!({ "units": { measure: self.unit }, "samples": samples });
		let text = format!("{file}\n");
		if text.len() > LIMIT {
			return Err(format!(
				"metrics.json is {} bytes, over the {LIMIT} Kixel Quest reads",
				text.len()
			));
		}
		Ok(text)
	}

	/// Writes the file at `path`.
	pub fn write(&self, path: &Path) -> Result<(), String> {
		write(path, &self.json()?)
	}
}

/// Writes `text` at `path`, creating its directory.
fn write(path: &Path, text: &str) -> Result<(), String> {
	if let Some(dir) = path.parent().filter(|dir| !dir.as_os_str().is_empty()) {
		std::fs::create_dir_all(dir).map_err(|e| format!("cannot create {}: {e}", dir.display()))?;
	}
	std::fs::write(path, text).map_err(|e| format!("cannot write {}: {e}", path.display()))
}

/// Every measure of every benchmark a run took, as JSON: what `--report` writes.
pub struct Report(Value);

impl Report {
	/// A report of a `mode` run under `fingerprint`, whose host `describe`s itself so, against the
	/// commit `base` when it has one, of each benchmark's measures by side.
	pub fn new(
		mode: crate::Mode,
		fingerprint: &str,
		host: &str,
		base: Option<&str>,
		benchmarks: BTreeMap<String, Map<String, Value>>,
	) -> Self {
		let mut report = json!({
			"schema_version": SCHEMA_VERSION,
			"mode": mode.name(),
			"fingerprint": fingerprint,
			"host": host.lines().collect::<Vec<_>>(),
			"benchmarks": benchmarks,
		});
		if let Some(base) = base {
			report["base"] = base.into();
		}
		Self(report)
	}

	/// Writes the report at `path`.
	pub fn write(&self, path: &Path) -> Result<(), String> {
		write(path, &format!("{:#}\n", self.0))
	}
}

/// Each measure of `measures` as a report holds it: its value and unit by name.
pub fn side(measures: &[Measure]) -> Value {
	let mut side = Map::new();
	for measure in measures {
		side.insert(
			measure.name.clone(),
			json!({ "value": number(measure.value), "unit": measure.unit }),
		);
	}
	Value::Object(side)
}

/// The report of a count, quick's or sim's, of the working tree, against `base`'s counts, from the
/// commit `sha`, when given.
pub fn report(counts: &Counts, base: Option<&Counts>, counter: &Counter, sha: Option<&str>) -> Report {
	let point = |counts: &Counts, name: &str| -> Option<Value> {
		let mut side = side(&counts.measures(name)?);
		if let Some(&(least, most)) = counts.spread.get(name) {
			side["instructions"]["lower"] = least.into();
			side["instructions"]["upper"] = most.into();
		}
		Some(side)
	};
	let names: BTreeSet<&String> = counts
		.counts
		.keys()
		.chain(base.into_iter().flat_map(|base| base.counts.keys()))
		.collect();
	let mut benchmarks = BTreeMap::new();
	for name in names {
		let mut entry = Map::new();
		if let Some(base) = base.and_then(|base| point(base, name)) {
			entry.insert("base".to_owned(), base);
		}
		if let Some(head) = point(counts, name) {
			entry.insert("head".to_owned(), head);
		}
		// Known only where a side saw another thread, or every side watched: a count sampled but
		// never seen threaded is one swole could not watch.
		let threaded = counts.threaded.contains(name) || base.is_some_and(|base| base.threaded.contains(name));
		let unwatched = |side: &Counts| side.spread.contains_key(name) && !side.threaded.contains(name);
		if threaded || !(unwatched(counts) || base.is_some_and(unwatched)) {
			entry.insert("threaded".to_owned(), threaded.into());
		}
		benchmarks.insert(name.clone(), entry);
	}
	Report::new(counter.mode(), &counts.fingerprint, &counts.host, sha, benchmarks)
}

/// A measure's value as JSON: a whole number as one, else five significant digits, more than any
/// run resolves.
pub fn number(value: f64) -> Value {
	if value.fract() == 0.0 && value.abs() < 9e15 {
		return (value as i64).into();
	}
	let rounded: f64 = format!("{value:.4e}").parse().expect("a float reads back");
	Number::from_f64(rounded).map_or(Value::Null, Value::Number)
}

/// Each benchmark's count, a line each, with the least and most of its samples when it took more
/// than one.
pub fn table(counts: &Counts) -> String {
	let width = counts.counts.values().map(|n| n.to_string().len()).max().unwrap_or(0);
	let mut out = String::new();
	for (name, count) in &counts.counts {
		match counts.spread.get(name) {
			Some((least, most)) => writeln!(out, "{count:>width$}  {name}  ({least} to {most})"),
			None => writeln!(out, "{count:>width$}  {name}"),
		}
		.expect("a String takes every write");
	}
	out
}

/// A measure's value as a table shows it, in its unit.
pub fn shown(value: f64, unit: &str) -> String {
	let number = if value.fract() == 0.0 {
		format!("{value}")
	} else if value.abs() >= 100.0 {
		format!("{value:.1}")
	} else {
		format!("{value:.3}")
	};
	match unit {
		"events" | "instructions" | "instructions/cycle" => number,
		"bytes/s" => rate(value, 1024.0, &["B/s", "KiB/s", "MiB/s", "GiB/s", "TiB/s"]),
		"items/s" => rate(
			value,
			1000.0,
			&["items/s", "K items/s", "M items/s", "G items/s", "T items/s"],
		),
		unit => format!("{number} {unit}"),
	}
}

/// A rate `value` in the largest of `units`, each `step` times the last, it reaches.
fn rate(mut value: f64, step: f64, units: &[&str]) -> String {
	let mut unit = 0;
	while value >= step && unit + 1 < units.len() {
		value /= step;
		unit += 1;
	}
	format!("{value:.2} {}", units[unit])
}

/// Every measure of each benchmark beyond its headline, a row each, as a Markdown table: with
/// `before`, the base's beside the head's, and the change, which for a benchmark `paired` names
/// is its `throughput`'s from its paired timing, rather than from its sides' medians; empty when
/// no benchmark has any. A measure whose unit differs by side, as a rate in bytes on one and
/// items on the other, takes a row a side.
pub fn details(
	before: Option<&BTreeMap<String, Vec<Measure>>>,
	after: &BTreeMap<String, Vec<Measure>>,
	paired: &BTreeMap<String, f64>,
) -> String {
	if after.is_empty() && before.is_none_or(BTreeMap::is_empty) {
		return String::new();
	}
	let mut out = match before {
		Some(_) => String::from("\n| benchmark | measure | base | head | change |\n|---|---|---:|---:|---:|\n"),
		None => String::from("\n| benchmark | measure | value |\n|---|---|---:|\n"),
	};
	let names: BTreeSet<&String> = after
		.keys()
		.chain(before.into_iter().flat_map(BTreeMap::keys))
		.collect();
	for name in names {
		// A `|` in a name would split its cell.
		let shown_name = name.replace('|', "\\|");
		let head = after.get(name).map(Vec::as_slice).unwrap_or_default();
		let base = before.and_then(|before| before.get(name)).map(Vec::as_slice);
		let mut measures: Vec<(&str, &'static str)> = Vec::new();
		for measure in head.iter().chain(base.unwrap_or_default()) {
			if !measures
				.iter()
				.any(|&(name, unit)| name == measure.name && unit == measure.unit)
			{
				measures.push((&measure.name, measure.unit));
			}
		}
		for (measure, unit) in measures {
			let find = |side: &[Measure]| {
				side.iter()
					.find(|m| m.name == measure && m.unit == unit)
					.map(|m| m.value)
			};
			let cell = |value: Option<f64>| value.map_or("-".to_owned(), |value| shown(value, unit));
			match base {
				Some(base) => {
					let change = match (find(base), find(head)) {
						(Some(_), Some(_)) if measure == "throughput" && paired.contains_key(name) => {
							format!("{:+.2}%", paired[name] * 100.0)
						}
						(Some(b), Some(a)) if b != 0.0 => format!("{:+.2}%", (a - b) / b * 100.0),
						(Some(_), Some(_)) => String::new(),
						(None, Some(_)) => "added".to_owned(),
						(Some(_), None) => "removed".to_owned(),
						(None, None) => unreachable!("every measure comes from one side"),
					};
					writeln!(
						out,
						"| {shown_name} | {measure} | {} | {} | {change} |",
						cell(find(base)),
						cell(find(head))
					)
				}
				None if before.is_some() => {
					writeln!(out, "| {shown_name} | {measure} | - | {} | added |", cell(find(head)))
				}
				None => writeln!(out, "| {shown_name} | {measure} | {} |", cell(find(head))),
			}
			.expect("a String takes every write");
		}
	}
	out
}

#[cfg(test)]
mod tests {
	use super::*;

	fn counts(names: &[(&str, u64)]) -> Counts {
		Counts {
			fingerprint: "x86_64-0123456789ab".to_owned(),
			host: String::new(),
			counts: names.iter().map(|(name, ir)| ((*name).to_owned(), *ir)).collect(),
			spread: Default::default(),
			threaded: Default::default(),
			details: Default::default(),
			throughput: Default::default(),
		}
	}

	fn measure() -> String {
		"instructions (x86_64-0123456789ab)".to_owned()
	}

	#[test]
	fn writes_one_sample_with_the_fingerprint_in_the_measure() {
		let mut counts = counts(&[("b::two", 20), ("a::one", 10)]);
		counts.spread.insert("b::two".to_owned(), (18, 25));
		let text = File::counts(&counts, measure()).json().unwrap();
		assert_eq!(
			text,
			"{\"samples\":[{\"metrics\":{\"a::one\":{\"instructions (x86_64-0123456789ab)\":{\"value\":10}},\
			 \"b::two\":{\"instructions (x86_64-0123456789ab)\":{\"lower_value\":18,\"upper_value\":25,\"value\":20}}}}],\
			 \"units\":{\"instructions (x86_64-0123456789ab)\":\"instructions\"}}\n"
		);
	}

	#[test]
	fn tables_every_measure_beside_the_base() {
		let side = |cycles: f64| {
			vec![
				Measure::new("cycles:u", cycles, "events"),
				Measure::new("user time", 1.5, "ns"),
			]
		};
		let base = BTreeMap::from([("a".to_owned(), side(100.0)), ("gone".to_owned(), side(1.0))]);
		let head = BTreeMap::from([("a".to_owned(), side(150.0))]);
		assert_eq!(
			details(Some(&base), &head, &BTreeMap::new()),
			"\n| benchmark | measure | base | head | change |\n|---|---|---:|---:|---:|\n\
			 | a | cycles:u | 100 | 150 | +50.00% |\n| a | user time | 1.500 ns | 1.500 ns | +0.00% |\n\
			 | gone | cycles:u | 1 | - | removed |\n| gone | user time | 1.500 ns | - | removed |\n"
		);
		assert_eq!(details(None, &BTreeMap::new(), &BTreeMap::new()), "");
		// A rate in bytes on one side and items on the other takes a row each, with no change; a
		// paired rate's change is the one its pairing gives.
		let rate = |unit| vec![Measure::new("throughput", 10.0, unit)];
		let base = BTreeMap::from([("a".to_owned(), rate("items/s")), ("b".to_owned(), rate("bytes/s"))]);
		let head = BTreeMap::from([("a".to_owned(), rate("bytes/s")), ("b".to_owned(), rate("bytes/s"))]);
		assert_eq!(
			details(Some(&base), &head, &BTreeMap::from([("b".to_owned(), 0.25)])),
			"\n| benchmark | measure | base | head | change |\n|---|---|---:|---:|---:|\n\
			 | a | throughput | - | 10.00 B/s | added |\n| a | throughput | 10.00 items/s | - | removed |\n\
			 | b | throughput | 10.00 B/s | 10.00 B/s | +25.00% |\n"
		);
	}

	#[test]
	fn derives_each_measured_count_per_byte_and_the_rate_from_native_time() {
		let measures = [
			Measure::new("time", 500.0, "ns"),
			Measure::new("user time", 400.0, "ns"),
			Measure::new("instructions:u", 4096.0, "events"),
			Measure::new("IPC:u", 2.0, "instructions/cycle"),
			Measure::new("max RSS", 1.0, "bytes"),
		];
		assert_eq!(
			Throughput::Bytes(1024).measures(&measures),
			[
				Measure::new("throughput", 2048e6, "bytes/s"),
				Measure::new("instructions:u/byte", 4.0, "events/byte"),
			]
		);
		// sim's counts have no time, so no rate.
		let counted = [
			Measure::new("instructions", 300.0, "instructions"),
			Measure::new("estimated cycles", 600.0, "cycles"),
		];
		assert_eq!(
			Throughput::Items(3).measures(&counted),
			[
				Measure::new("instructions/item", 100.0, "instructions/item"),
				Measure::new("estimated cycles/item", 200.0, "cycles/item"),
			]
		);
		let declared =
			Throughput::declared("own\nswole throughput bytes 8 a[1 2]\nswole throughput bytes 8 a[1 2]\n").unwrap();
		assert_eq!(declared, BTreeMap::from([("a[1 2]".to_owned(), Throughput::Bytes(8))]));
		let two = Throughput::declared("swole throughput bytes 8 a\nswole throughput items 8 a\n");
		assert!(two.unwrap_err().contains("two throughputs"));
		for bad in ["bytes 0 a", "bits 8 a", "bytes -1 a", "bytes 8"] {
			let err = Throughput::declared(&format!("swole throughput {bad}\n")).unwrap_err();
			assert!(err.contains("which no harness prints"), "{bad}: {err}");
		}
		assert_eq!(Throughput::parse("bytes", "0"), None);
		assert_eq!(Throughput::parse("items", "7"), Some(Throughput::Items(7)));
		assert_eq!(shown(2048e6, "bytes/s"), "1.91 GiB/s");
		assert_eq!(shown(12.0, "bytes/s"), "12.00 B/s");
		assert_eq!(shown(1.5e6, "items/s"), "1.50 M items/s");
		assert_eq!(shown(4.0, "events/byte"), "4 events/byte");
	}

	#[test]
	fn reports_its_schema_version_and_each_derived_measure() {
		let mut counts = counts(&[("a", 300), ("b", 10)]);
		counts.throughput.insert("a".to_owned(), Throughput::Bytes(3));
		let counter = Counter {
			kind: crate::CounterKind::Valgrind,
			fingerprint: counts.fingerprint.clone(),
			host: String::new(),
		};
		let Report(report) = report(&counts, None, &counter, None);
		assert_eq!(report["schema_version"], 1);
		assert_eq!(
			report["benchmarks"]["a"]["head"]["instructions/byte"],
			json!({ "value": 100, "unit": "instructions/byte" })
		);
		assert!(report["benchmarks"]["b"]["head"].get("instructions/byte").is_none());
		assert_eq!(
			counts.shown().keys().collect::<Vec<_>>(),
			["a"],
			"only a benchmark with details or a throughput has a row below the counts"
		);
	}

	#[test]
	fn refuses_what_kixel_quest_would() {
		let json = |counts: &Counts| File::counts(counts, measure()).json();
		assert!(json(&counts(&[])).is_err());
		assert!(json(&counts(&[("a\nb", 1)])).is_err());
		assert!(json(&counts(&[("", 1)])).is_err());
		let long = "x".repeat(NAME_LIMIT + 1);
		assert!(json(&counts(&[(&long, 1)])).is_err());
		let names: Vec<String> = (0..20_000)
			.map(|i| format!("benches/a/very/long/path.rs::group::case[{i}]"))
			.collect();
		let many: Vec<(&str, u64)> = names.iter().map(|name| (name.as_str(), u64::MAX)).collect();
		assert!(json(&counts(&many)).unwrap_err().contains("over the"));
	}

	#[test]
	fn writes_another_commits_sample() {
		let value = |value: f64| Number::from_f64(value).unwrap();
		let base = "a".repeat(40);
		let file = File {
			measure: "time (aarch64-0123456789ab)".to_owned(),
			unit: "ns",
			samples: vec![
				Sample {
					commit: Some(base.clone()),
					values: BTreeMap::from([("a".to_owned(), value(2.5).into())]),
				},
				Sample {
					commit: None,
					values: BTreeMap::from([("a".to_owned(), value(2.0).into())]),
				},
				Sample {
					commit: Some("b".repeat(40)),
					values: BTreeMap::new(),
				},
			],
		};
		assert_eq!(
			file.json().unwrap(),
			format!(
				"{{\"samples\":[{{\"commit\":\"{base}\",\"metrics\":{{\"a\":{{\"time (aarch64-0123456789ab)\":\
				 {{\"value\":2.5}}}}}}}},\
				 {{\"metrics\":{{\"a\":{{\"time (aarch64-0123456789ab)\":{{\"value\":2.0}}}}}}}}],\
				 \"units\":{{\"time (aarch64-0123456789ab)\":\"ns\"}}}}\n"
			)
		);
	}
}
