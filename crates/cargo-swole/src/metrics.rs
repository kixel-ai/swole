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
		if name.is_empty() {
			return Err(malformed());
		}
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

	/// What derives from a call's `measures`: each count's share of a byte or an item, its
	/// allocations' too, and from its native wall-clock `time`, how many it processes a second,
	/// each with the spread of what it derives from. A count or time the run did not measure
	/// derives nothing.
	pub fn measures(self, measures: &[Measure]) -> Vec<Measure> {
		let bytes = matches!(self, Self::Bytes(_));
		let (per, of) = if bytes {
			("byte", "bytes/s")
		} else {
			("item", "items/s")
		};
		let amount = self.amount();
		let mut out = Vec::new();
		for measure in measures {
			let unit = match (measure.name.as_str(), measure.unit, bytes) {
				("time", "ns", _) => {
					if measure.value > 0.0 && measure.spread.is_none_or(|(least, _)| least > 0.0) {
						let mut rate = Measure::new("throughput", amount * 1e9 / measure.value, of);
						rate.spread = measure
							.spread
							.map(|(least, most)| (amount * 1e9 / most, amount * 1e9 / least));
						out.push(rate);
					}
					continue;
				}
				// A call's allocations sum over it as a count does; the most it held at once does not.
				(_, "instructions", true) => "instructions/byte",
				(_, "instructions", false) => "instructions/item",
				(_, "cycles", true) => "cycles/byte",
				(_, "cycles", false) => "cycles/item",
				(_, "events", true) => "events/byte",
				(_, "events", false) => "events/item",
				// A call's allocations sum over it as a count does; the most it held at once does not.
				("alloc_count", "allocations", true) => "allocations/byte",
				("alloc_count", "allocations", false) => "allocations/item",
				("alloc_bytes", "bytes", true) => "bytes/byte",
				("alloc_bytes", "bytes", false) => "bytes/item",
				_ => continue,
			};
			let mut derived = Measure::new(&format!("{}/{per}", measure.name), measure.value / amount, unit);
			derived.spread = measure.spread.map(|(least, most)| (least / amount, most / amount));
			out.push(derived);
		}
		out
	}
}

/// A `metrics.json`: a few measures, each in its unit, of each sample's benchmarks.
pub struct File {
	/// Each measure's unit, by its name.
	pub units: BTreeMap<String, &'static str>,
	pub samples: Vec<Sample>,
}

/// One commit's values: the run's own, or another's, such as a paired comparison's base.
pub struct Sample {
	/// The commit's full ID, when it is not the run's.
	pub commit: Option<String>,
	/// Each benchmark's value of each measure, by the measure's name.
	pub values: BTreeMap<String, BTreeMap<String, Point>>,
}

/// One benchmark's value, and the least and most of its samples, when it took several.
pub struct Point {
	pub value: Number,
	pub spread: Option<(Number, Number)>,
}

impl Point {
	/// A count's point: its value, and the least and most of its samples when it took several.
	pub fn of(measure: &Measure) -> Result<Self, String> {
		Ok(Self {
			value: whole(measure.value)?,
			spread: measure
				.spread
				.map(|(least, most)| Ok::<_, String>((whole(least)?, whole(most)?)))
				.transpose()?,
		})
	}
}

impl From<Number> for Point {
	fn from(value: Number) -> Self {
		Self { value, spread: None }
	}
}

impl File {
	/// `counts` as one sample of the run's own commit: each series `counter` writes, of each
	/// benchmark, and its allocations, when it has them.
	pub fn counts(counts: &Counts, counter: &Counter) -> Result<Self, String> {
		let series = counter.series(&counts.fingerprint);
		let mut units = BTreeMap::new();
		let allocs = crate::heap::fingerprint(&counts.fingerprint, true);
		let mut values = BTreeMap::new();
		for (name, details) in &counts.details {
			let mut measures = BTreeMap::new();
			for (detail, measure, _) in &series {
				let found = details
					.iter()
					.find(|found| found.name == *detail)
					.ok_or_else(|| format!("{name} has no {detail}"))?;
				measures.insert(measure.clone(), Point::of(found)?);
			}
			for (series, unit, found) in crate::heap::series(details, &allocs) {
				units.insert(series.clone(), unit);
				measures.insert(series, Point::of(found)?);
			}
			values.insert(name.clone(), measures);
		}
		units.extend(series.into_iter().map(|(_, measure, unit)| (measure, unit)));
		Ok(Self {
			units,
			samples: vec![Sample { commit: None, values }],
		})
	}

	/// The file's JSON, refusing what Kixel Quest would.
	pub fn json(&self) -> Result<String, String> {
		for measure in self.units.keys() {
			if measure.chars().count() > MEASURE_LIMIT {
				return Err(format!("the measure {measure} is longer than Kixel Quest takes"));
			}
		}
		let mut samples = Vec::new();
		for sample in &self.samples {
			let mut metrics = Map::new();
			for (name, points) in &sample.values {
				if name.is_empty() || name.chars().count() > NAME_LIMIT || name.chars().any(char::is_control) {
					return Err(format!("Kixel Quest takes no benchmark named {name:?}"));
				}
				let mut measures = Map::new();
				for (measure, point) in points {
					if !self.units.contains_key(measure) {
						return Err(format!("{name}'s measure {measure} has no unit"));
					}
					let mut value = json!({ "value": point.value });
					if let Some((lower, upper)) = &point.spread {
						value["lower_value"] = lower.clone().into();
						value["upper_value"] = upper.clone().into();
					}
					measures.insert(measure.clone(), value);
				}
				metrics.insert(name.clone(), Value::Object(measures));
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
		let file = json!({ "units": self.units, "samples": samples });
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

/// A count, which is whole, as a JSON number.
fn whole(value: f64) -> Result<Number, String> {
	if value.fract() != 0.0 || !(0.0..9e15).contains(&value) {
		return Err(format!("{value} is not a count"));
	}
	Ok((value as u64).into())
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

/// Each measure of `measures` as a report holds it: its value and unit by name, and the least and
/// most of its samples when it took several.
pub fn side(measures: &[Measure]) -> Value {
	let mut side = Map::new();
	for measure in measures {
		let value = match measure.spread {
			None => json!({ "value": number(measure.value), "unit": measure.unit }),
			Some((least, most)) => {
				// One rule for all three, which rounding keeps in order, so the bounds hold the median.
				let exact = [least, measure.value, most]
					.iter()
					.all(|v| v.fract() == 0.0 && v.abs() < 9e15);
				let number = |value: f64| if exact { number(value) } else { rounded(value) };
				json!({
					"value": number(measure.value),
					"unit": measure.unit,
					"lower": number(least),
					"upper": number(most),
				})
			}
		};
		side.insert(measure.name.clone(), value);
	}
	crate::heap::label(measures, &mut side);
	Value::Object(side)
}

/// The report of a count, quick's or sim's, of the working tree, against `base`'s counts, from the
/// commit `sha`, when given.
pub fn report(counts: &Counts, base: Option<&Counts>, counter: &Counter, sha: Option<&str>) -> Report {
	let point = |counts: &Counts, name: &str| -> Option<Value> { Some(side(&counts.measures(name)?)) };
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
	rounded(value)
}

/// `value` to five significant digits, which keeps any values in order, whole or not.
fn rounded(value: f64) -> Value {
	let rounded: f64 = format!("{value:.4e}").parse().expect("a float reads back");
	Number::from_f64(rounded).map_or(Value::Null, Value::Number)
}

/// Each benchmark's count, a line each, with the least and most of its samples when it took more
/// than one, marked sampled (`~`).
pub fn table(counts: &Counts) -> String {
	let shown = |name: &String, count: &u64| match counts.spread.get(name) {
		Some(_) => format!("{SAMPLED}{count}"),
		None => count.to_string(),
	};
	let width = counts
		.counts
		.iter()
		.map(|(name, count)| shown(name, count).chars().count())
		.max()
		.unwrap_or(0);
	let mut out = String::new();
	for (name, count) in &counts.counts {
		let count = shown(name, count);
		match counts.spread.get(name) {
			Some((least, most)) => writeln!(out, "{count:>width$}  {name}  ({least} to {most})"),
			None => writeln!(out, "{count:>width$}  {name}"),
		}
		.expect("a String takes every write");
	}
	out
}

/// What marks a value sampled, the median of counts another thread or the scheduler moves, rather
/// than one exact count.
pub const SAMPLED: &str = "~";

/// A change from `base` to `head`, each a value and the least and most of its samples when
/// sampled, as a table shows it: none from an exact base of zero, and from a sampled one, the
/// absolute change and the least and most its spreads allow. A sampled value moves within its
/// samples' spread, so the change between two says whether their spreads overlap; with one side's
/// spread alone, it gives no verdict and is marked sampled.
pub fn change(
	(base, base_spread): (f64, Option<(f64, f64)>),
	(head, head_spread): (f64, Option<(f64, f64)>),
) -> String {
	let change = if base != 0.0 {
		format!("{:+.2}%", (head - base) / base * 100.0)
	} else if base_spread.is_some() || head_spread.is_some() {
		// No percentage grows from zero, but a sampled zero still moves by some amount.
		let (base_least, base_most) = base_spread.unwrap_or((base, base));
		let (head_least, head_most) = head_spread.unwrap_or((head, head));
		format!(
			"{} ({} to {})",
			signed(head - base),
			signed(head_least - base_most),
			signed(head_most - base_least)
		)
	} else {
		String::new()
	};
	match (base_spread, head_spread) {
		(None, None) => change,
		(Some((base_least, base_most)), Some((head_least, head_most))) => {
			if head_least <= base_most && base_least <= head_most {
				format!("{SAMPLED}{change} (within spread)")
			} else {
				format!("{change} (beyond spread)")
			}
		}
		_ => format!("{SAMPLED}{change}"),
	}
}

/// `value` with its sign, whole when it is, else to three decimals.
fn signed(value: f64) -> String {
	if value.fract() == 0.0 {
		format!("{value:+}")
	} else {
		format!("{value:+.3}")
	}
}

/// A measure's value as a table shows it, in its unit.
pub fn shown(value: f64, unit: &str) -> String {
	let number = if value.fract() == 0.0 {
		format!("{value}")
	} else if value.abs() >= 100.0 {
		format!("{value:.1}")
	} else if value != 0.0 && value.abs() < 0.001 {
		// A per-byte count of a large input, which fixed decimals would show as zero.
		format!("{value:.2e}")
	} else {
		format!("{value:.3}")
	};
	match unit {
		"events" | "instructions" | "cycles" | "instructions/cycle" => number,
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
			fn find<'a>(side: &'a [Measure], name: &str, unit: &str) -> Option<&'a Measure> {
				side.iter().find(|m| m.name == name && m.unit == unit)
			}
			// A sampled measure, the median of its samples, is marked as one.
			let cell = |found: Option<&Measure>| match found {
				Some(Measure {
					value,
					spread: Some((least, most)),
					..
				}) => format!(
					"{SAMPLED}{} ({} to {})",
					shown(*value, unit),
					shown(*least, unit),
					shown(*most, unit)
				),
				Some(found) => shown(found.value, unit),
				None => "-".to_owned(),
			};
			match base {
				Some(base) => {
					let change = match (find(base, measure, unit), find(head, measure, unit)) {
						// A paired rate's change is the one its pairing gives.
						(Some(_), Some(_)) if measure == "throughput" && paired.contains_key(name) => {
							format!("{:+.2}%", paired[name] * 100.0)
						}
						(Some(b), Some(a)) => change((b.value, b.spread), (a.value, a.spread)),
						(None, Some(_)) => "added".to_owned(),
						(Some(_), None) => "removed".to_owned(),
						(None, None) => unreachable!("every measure comes from one side"),
					};
					writeln!(
						out,
						"| {shown_name} | {measure} | {} | {} | {change} |",
						cell(find(base, measure, unit)),
						cell(find(head, measure, unit))
					)
				}
				None if before.is_some() => {
					writeln!(
						out,
						"| {shown_name} | {measure} | - | {} | added |",
						cell(find(head, measure, unit))
					)
				}
				None => writeln!(
					out,
					"| {shown_name} | {measure} | {} |",
					cell(find(head, measure, unit))
				),
			}
			.expect("a String takes every write");
		}
	}
	out
}

#[cfg(test)]
mod tests {
	use super::*;

	/// Sim's counts of each named benchmark: its instructions, and three times as many cycles.
	fn counts(names: &[(&str, u64)]) -> Counts {
		let details = |ir: u64| {
			vec![
				Measure::new("instructions", ir as f64, "instructions"),
				Measure::new(crate::callgrind::ESTIMATED, 3.0 * ir as f64, "cycles"),
				Measure::new("data reads", 1.0, "events"),
			]
		};
		Counts {
			fingerprint: "x86_64-0123456789ab".to_owned(),
			host: String::new(),
			counts: names.iter().map(|(name, ir)| ((*name).to_owned(), *ir)).collect(),
			spread: Default::default(),
			threaded: Default::default(),
			details: names
				.iter()
				.map(|(name, ir)| ((*name).to_owned(), details(*ir)))
				.collect(),
			throughput: Default::default(),
		}
	}

	fn sim() -> Counter {
		Counter {
			kind: crate::CounterKind::Valgrind,
			fingerprint: String::new(),
			host: String::new(),
		}
	}

	#[test]
	fn writes_one_sample_of_each_series_with_the_fingerprint_in_the_measure() {
		let mut counts = counts(&[("b::two", 20), ("a::one", 10)]);
		let two = counts.details.get_mut("b::two").unwrap();
		two[0].spread = Some((18.0, 25.0));
		two[1].spread = Some((54.0, 75.0));
		let text = File::counts(&counts, &sim()).unwrap().json().unwrap();
		assert_eq!(
			text,
			"{\"samples\":[{\"metrics\":{\"a::one\":{\"estimated cycles (x86_64-0123456789ab)\":{\"value\":30},\
			 \"instructions (x86_64-0123456789ab)\":{\"value\":10}},\
			 \"b::two\":{\"estimated cycles (x86_64-0123456789ab)\":{\"lower_value\":54,\"upper_value\":75,\"value\":60},\
			 \"instructions (x86_64-0123456789ab)\":{\"lower_value\":18,\"upper_value\":25,\"value\":20}}}}],\
			 \"units\":{\"estimated cycles (x86_64-0123456789ab)\":\"cycles\",\
			 \"instructions (x86_64-0123456789ab)\":\"instructions\"}}\n"
		);
	}

	#[test]
	fn reports_and_tables_each_measures_spread() {
		let mut counts = counts(&[("a", 10)]);
		counts.details.get_mut("a").unwrap()[1].spread = Some((27.0, 33.0));
		assert_eq!(
			details(None, &counts.details, &BTreeMap::new()),
			"\n| benchmark | measure | value |\n|---|---|---:|\n| a | instructions | 10 |\n\
			 | a | estimated cycles | ~30 (27 to 33) |\n| a | data reads | 1 |\n"
		);
		let report = report(&counts, None, &sim(), None);
		assert_eq!(
			report.0["benchmarks"]["a"]["head"]["estimated cycles"],
			json!({ "value": 30, "unit": "cycles", "lower": 27, "upper": 33 })
		);
		assert_eq!(
			report.0["benchmarks"]["a"]["head"]["data reads"],
			json!({ "value": 1, "unit": "events" })
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

		// A sampled measure's change compares against both sides' spreads, and with one side's
		// alone, has no verdict.
		let sampled = |value: f64, spread: Option<(f64, f64)>| {
			let mut measure = Measure::new("alloc_count", value, "allocations");
			measure.spread = spread;
			BTreeMap::from([("a".to_owned(), vec![measure])])
		};
		let change = |base, head| {
			details(Some(&base), &head, &BTreeMap::new())
				.lines()
				.last()
				.unwrap()
				.to_owned()
		};
		assert_eq!(
			change(sampled(10.0, Some((8.0, 12.0))), sampled(11.0, Some((11.0, 13.0)))),
			"| a | alloc_count | ~10 allocations (8 allocations to 12 allocations) | ~11 allocations (11 allocations to \
			 13 allocations) | ~+10.00% (within spread) |"
		);
		assert!(
			change(sampled(10.0, Some((8.0, 12.0))), sampled(20.0, Some((19.0, 21.0))))
				.ends_with("| +100.00% (beyond spread) |")
		);
		assert!(change(sampled(10.0, None), sampled(20.0, Some((19.0, 21.0)))).ends_with("| ~+100.00% |"));
		assert!(change(sampled(10.0, None), sampled(20.0, None)).ends_with("| +100.00% |"));
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
		// A call's allocations derive as its counts do, each with its samples' spread; the most it
		// held at once derives nothing.
		let mut allocs = Measure::new("alloc_count", 8.0, "allocations");
		allocs.spread = Some((4.0, 12.0));
		let mut expected = Measure::new("alloc_count/item", 2.0, "allocations/item");
		expected.spread = Some((1.0, 3.0));
		assert_eq!(
			Throughput::Items(4).measures(&[allocs, Measure::new("peak_live_bytes", 64.0, "bytes")]),
			[expected]
		);
		let declared =
			Throughput::declared("own\nswole throughput bytes 8 a[1 2]\nswole throughput bytes 8 a[1 2]\n").unwrap();
		assert_eq!(declared, BTreeMap::from([("a[1 2]".to_owned(), Throughput::Bytes(8))]));
		let two = Throughput::declared("swole throughput bytes 8 a\nswole throughput items 8 a\n");
		assert!(two.unwrap_err().contains("two throughputs"));
		for bad in ["bytes 0 a", "bits 8 a", "bytes -1 a", "bytes 8", "bytes 8 "] {
			let err = Throughput::declared(&format!("swole throughput {bad}\n")).unwrap_err();
			assert!(err.contains("which no harness prints"), "{bad}: {err}");
		}
		assert_eq!(Throughput::parse("bytes", "0"), None);
		assert_eq!(Throughput::parse("items", "7"), Some(Throughput::Items(7)));
		assert_eq!(shown(2048e6, "bytes/s"), "1.91 GiB/s");
		assert_eq!(shown(12.0, "bytes/s"), "12.00 B/s");
		assert_eq!(shown(1.5e6, "items/s"), "1.50 M items/s");
		assert_eq!(shown(4.0, "events/byte"), "4 events/byte");
		assert_eq!(shown(1.0 / 16384.0, "allocations/byte"), "6.10e-5 allocations/byte");
		assert_eq!(shown(0.0, "allocations/byte"), "0 allocations/byte");
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
		assert!(report["benchmarks"]["a"]["head"]["estimated cycles/byte"].is_object());
		assert!(report["benchmarks"]["a"]["head"]["data reads/byte"].is_object());
	}

	#[test]
	fn writes_allocations_beside_the_headline_where_a_benchmark_has_them() {
		let mut counts = counts(&[("a", 10), ("b", 20)]);
		let mut peak = Measure::new("peak_live_bytes", 64.0, "bytes");
		peak.spread = Some((32.0, 64.0));
		counts
			.details
			.get_mut("a")
			.unwrap()
			.extend([Measure::new("alloc_count", 2.0, "allocations"), peak]);
		let json: Value = serde_json::from_str(&File::counts(&counts, &sim()).unwrap().json().unwrap()).unwrap();
		let fp = crate::heap::fingerprint("x86_64-0123456789ab", true);
		assert_eq!(json["units"][format!("alloc_count ({fp})")], "allocations");
		assert_eq!(json["units"][format!("peak_live_bytes ({fp})")], "bytes");
		assert!(json["units"].get(format!("alloc_bytes ({fp})")).is_none());
		let a = &json["samples"][0]["metrics"]["a"];
		assert_eq!(a[format!("alloc_count ({fp})")], json!({ "value": 2 }));
		assert_eq!(
			a[format!("peak_live_bytes ({fp})")],
			json!({ "value": 64, "lower_value": 32, "upper_value": 64 })
		);
		assert_eq!(json["samples"][0]["metrics"]["b"].as_object().unwrap().len(), 2);
	}

	#[test]
	fn marks_a_sampled_value_and_compares_it_by_its_spread() {
		let mut sampled = counts(&[("a", 10), ("b", 200)]);
		sampled.spread.insert("b".to_owned(), (190, 210));
		assert_eq!(table(&sampled), "  10  a\n~200  b  (190 to 210)\n");
		assert_eq!(change((100.0, None), (150.0, None)), "+50.00%");
		assert_eq!(change((100.0, Some((90.0, 110.0))), (150.0, None)), "~+50.00%");
		assert_eq!(
			change((100.0, Some((90.0, 110.0))), (105.0, Some((95.0, 115.0)))),
			"~+5.00% (within spread)"
		);
		assert_eq!(
			change((100.0, Some((90.0, 110.0))), (150.0, Some((140.0, 160.0)))),
			"+50.00% (beyond spread)"
		);
		assert_eq!(change((0.0, Some((0.0, 1.0))), (2.0, None)), "~+2 (+1 to +2)");
		assert_eq!(
			change((0.0, Some((0.0, 1.0))), (2.0, Some((2.0, 3.0)))),
			"+2 (+1 to +3) (beyond spread)"
		);
		assert_eq!(change((0.0, None), (2.0, None)), "");
		// A report gives each measure's spread, or with none to hold it, the count's.
		sampled.details.remove("b");
		let report = report(&sampled, None, &sim(), None);
		assert_eq!(
			report.0["benchmarks"]["b"]["head"]["instructions"],
			json!({ "value": 200, "unit": "instructions", "lower": 190, "upper": 210 })
		);
		// Rounded to five digits, by one rule, the bounds still hold the median.
		for (median, least, most) in [
			(408_648.5, 408_648.0, 408_649.0),
			(408_641.5, 408_641.0, 408_642.0),
			(408_649.0, 408_648.6, 408_649.6),
		] {
			let side = side(&[Measure {
				spread: Some((least, most)),
				..Measure::new("instructions:u", median, "events")
			}]);
			let [value, lower, upper] =
				["value", "lower", "upper"].map(|key| side["instructions:u"][key].as_f64().unwrap());
			assert!(lower <= value && value <= upper, "{side}");
		}
	}

	#[test]
	fn refuses_what_kixel_quest_would() {
		let json = |counts: &Counts| File::counts(counts, &sim())?.json();
		assert!(json(&counts(&[])).is_err());
		assert!(json(&counts(&[("a\nb", 1)])).is_err());
		assert!(json(&counts(&[("", 1)])).is_err());
		let long = "x".repeat(NAME_LIMIT + 1);
		assert!(json(&counts(&[(&long, 1)])).is_err());
		let names: Vec<String> = (0..20_000)
			.map(|i| format!("benches/a/very/long/path.rs::group::case[{i}]"))
			.collect();
		let many: Vec<(&str, u64)> = names.iter().map(|name| (name.as_str(), 1 << 50)).collect();
		assert!(json(&counts(&many)).unwrap_err().contains("over the"));
	}

	#[test]
	fn writes_another_commits_sample() {
		let measure = "time (aarch64-0123456789ab)";
		let value = |value: f64| BTreeMap::from([(measure.to_owned(), Number::from_f64(value).unwrap().into())]);
		let base = "a".repeat(40);
		let file = File {
			units: BTreeMap::from([(measure.to_owned(), "ns")]),
			samples: vec![
				Sample {
					commit: Some(base.clone()),
					values: BTreeMap::from([("a".to_owned(), value(2.5))]),
				},
				Sample {
					commit: None,
					values: BTreeMap::from([("a".to_owned(), value(2.0))]),
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
