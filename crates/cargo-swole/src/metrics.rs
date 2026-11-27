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

impl From<Number> for Point {
	fn from(value: Number) -> Self {
		Self { value, spread: None }
	}
}

impl File {
	/// `counts` as one sample of the run's own commit: each series `counter` writes, of each
	/// benchmark.
	pub fn counts(counts: &Counts, counter: &Counter) -> Result<Self, String> {
		let series = counter.series(&counts.fingerprint);
		let mut values = BTreeMap::new();
		for (name, details) in &counts.details {
			let mut measures = BTreeMap::new();
			for (detail, measure, _) in &series {
				let found = details
					.iter()
					.find(|found| found.name == *detail)
					.ok_or_else(|| format!("{name} has no {detail}"))?;
				measures.insert(
					measure.clone(),
					Point {
						value: whole(found.value)?,
						spread: found
							.spread
							.map(|(least, most)| Ok::<_, String>((whole(least)?, whole(most)?)))
							.transpose()?,
					},
				);
			}
			values.insert(name.clone(), measures);
		}
		Ok(Self {
			units: series.into_iter().map(|(_, measure, unit)| (measure, unit)).collect(),
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
		let mut value = json!({ "value": number(measure.value), "unit": measure.unit });
		if let Some((least, most)) = measure.spread {
			value["lower"] = number(least);
			value["upper"] = number(most);
		}
		side.insert(measure.name.clone(), value);
	}
	Value::Object(side)
}

/// The report of a count, quick's or sim's, of the working tree, against `base`'s counts, from the
/// commit `sha`, when given.
pub fn report(counts: &Counts, base: Option<&Counts>, counter: &Counter, sha: Option<&str>) -> Report {
	let point = |counts: &Counts, name: &str| -> Option<Value> { Some(side(counts.details.get(name)?)) };
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
		"events" | "instructions" | "cycles" | "instructions/cycle" => number,
		unit => format!("{number} {unit}"),
	}
}

/// Every measure of each benchmark beyond its headline, a row each, as a Markdown table: with
/// `before`, the base's beside the head's, and the change; empty when no benchmark has any.
pub fn details(before: Option<&BTreeMap<String, Vec<Measure>>>, after: &BTreeMap<String, Vec<Measure>>) -> String {
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
			if !measures.iter().any(|(name, _)| *name == measure.name) {
				measures.push((&measure.name, measure.unit));
			}
		}
		for (measure, unit) in measures {
			fn find<'a>(side: &'a [Measure], name: &str) -> Option<&'a Measure> {
				side.iter().find(|m| m.name == name)
			}
			let cell = |found: Option<&Measure>| match found {
				Some(Measure {
					value,
					spread: Some((least, most)),
					..
				}) => format!(
					"{} ({} to {})",
					shown(*value, unit),
					shown(*least, unit),
					shown(*most, unit)
				),
				Some(found) => shown(found.value, unit),
				None => "-".to_owned(),
			};
			match base {
				Some(base) => {
					let change = match (
						find(base, measure).map(|m| m.value),
						find(head, measure).map(|m| m.value),
					) {
						(Some(b), Some(a)) if b != 0.0 => format!("{:+.2}%", (a - b) / b * 100.0),
						(Some(_), Some(_)) => String::new(),
						(None, Some(_)) => "added".to_owned(),
						(Some(_), None) => "removed".to_owned(),
						(None, None) => unreachable!("every measure comes from one side"),
					};
					writeln!(
						out,
						"| {shown_name} | {measure} | {} | {} | {change} |",
						cell(find(base, measure)),
						cell(find(head, measure))
					)
				}
				None if before.is_some() => {
					writeln!(
						out,
						"| {shown_name} | {measure} | - | {} | added |",
						cell(find(head, measure))
					)
				}
				None => writeln!(out, "| {shown_name} | {measure} | {} |", cell(find(head, measure))),
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
			details(None, &counts.details),
			"\n| benchmark | measure | value |\n|---|---|---:|\n| a | instructions | 10 |\n\
			 | a | estimated cycles | 30 (27 to 33) |\n| a | data reads | 1 |\n"
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
			details(Some(&base), &head),
			"\n| benchmark | measure | base | head | change |\n|---|---|---:|---:|---:|\n\
			 | a | cycles:u | 100 | 150 | +50.00% |\n| a | user time | 1.500 ns | 1.500 ns | +0.00% |\n\
			 | gone | cycles:u | 1 | - | removed |\n| gone | user time | 1.500 ns | - | removed |\n"
		);
		assert_eq!(details(None, &BTreeMap::new()), "");
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
