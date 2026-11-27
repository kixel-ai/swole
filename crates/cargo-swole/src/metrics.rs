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
	// Every measure, each with its own spread, or Valgrind's instructions alone, with the spread of
	// the count.
	let point = |counts: &Counts, name: &str| -> Option<Value> {
		let count = *counts.counts.get(name)?;
		Some(side(&match counts.details.get(name) {
			Some(details) => details.clone(),
			None => vec![Measure {
				spread: counts
					.spread
					.get(name)
					.map(|&(least, most)| (least as f64, most as f64)),
				..Measure::new("instructions", count as f64, "instructions")
			}],
		}))
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

/// A change from `base` to `head` as a table shows it: exact, or between two sampled values, the
/// change from their median to median, and the least and most their spreads allow, marked sampled,
/// in absolute terms where a base sample of zero leaves no percentage. An exact base is nonzero.
pub fn change(
	(base, base_spread): (f64, Option<(f64, f64)>),
	(head, head_spread): (f64, Option<(f64, f64)>),
) -> String {
	let percent = |base: f64, head: f64| format!("{:+.2}%", (head - base) / base * 100.0);
	if base_spread.is_none() && head_spread.is_none() {
		return percent(base, head);
	}
	let (base_least, base_most) = base_spread.unwrap_or((base, base));
	let (head_least, head_most) = head_spread.unwrap_or((head, head));
	if base_least <= 0.0 {
		let delta = |base: f64, head: f64| {
			let delta = head - base;
			if delta.fract() == 0.0 {
				format!("{delta:+}")
			} else {
				format!("{delta:+.3}")
			}
		};
		return format!(
			"{SAMPLED}{} ({} to {})",
			delta(base, head),
			delta(base_most, head_least),
			delta(base_least, head_most)
		);
	}
	format!(
		"{SAMPLED}{} ({} to {})",
		percent(base, head),
		percent(base_most, head_least),
		percent(base_least, head_most)
	)
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
			let find = |side: &[Measure]| side.iter().find(|m| m.name == measure).map(|m| (m.value, m.spread));
			let cell = |found: Option<(f64, Option<(f64, f64)>)>| match found {
				Some((value, Some((least, most)))) => format!(
					"{SAMPLED}{} ({} to {})",
					shown(value, unit),
					shown(least, unit),
					shown(most, unit)
				),
				Some((value, None)) => shown(value, unit),
				None => "-".to_owned(),
			};
			match base {
				Some(base) => {
					let change = match (find(base), find(head)) {
						// A zero base leaves no percentage unless a spread gives an absolute range.
						(Some(b), Some(a)) if b.0 != 0.0 || b.1.is_some() || a.1.is_some() => change(b, a),
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
			details(Some(&base), &head),
			"\n| benchmark | measure | base | head | change |\n|---|---|---:|---:|---:|\n\
			 | a | cycles:u | 100 | 150 | +50.00% |\n| a | user time | 1.500 ns | 1.500 ns | +0.00% |\n\
			 | gone | cycles:u | 1 | - | removed |\n| gone | user time | 1.500 ns | - | removed |\n"
		);
		assert_eq!(details(None, &BTreeMap::new()), "");
	}

	#[test]
	fn marks_a_sampled_value_and_compares_it_by_its_spread() {
		let mut sampled = counts(&[("a", 10), ("b", 200)]);
		sampled.spread.insert("b".to_owned(), (190, 210));
		assert_eq!(table(&sampled), "  10  a\n~200  b  (190 to 210)\n");
		assert_eq!(change((100.0, None), (150.0, None)), "+50.00%");
		// From the base's most to the head's least, and its least to the head's most.
		assert_eq!(
			change((100.0, Some((90.0, 110.0))), (150.0, None)),
			"~+50.00% (+36.36% to +66.67%)"
		);
		// A base sample of zero leaves no percentage, so the change and its range are absolute.
		assert_eq!(
			change((1.0, Some((0.0, 2.0))), (2.0, Some((1.0, 3.0)))),
			"~+1 (-1 to +3)"
		);
		assert_eq!(change((0.0, Some((0.0, 0.5))), (2.0, None)), "~+2 (+1.500 to +2)");
		let base = BTreeMap::from([(
			"a".to_owned(),
			vec![Measure {
				spread: Some((90.0, 110.0)),
				..Measure::new("cycles:u", 100.0, "events")
			}],
		)]);
		let head = BTreeMap::from([("a".to_owned(), vec![Measure::new("cycles:u", 150.0, "events")])]);
		assert_eq!(
			details(Some(&base), &head),
			"\n| benchmark | measure | base | head | change |\n|---|---|---:|---:|---:|\n\
			 | a | cycles:u | ~100 (90 to 110) | 150 | ~+50.00% (+36.36% to +66.67%) |\n"
		);
		// A report gives each measure's spread, or with none to hold it, the count's.
		let counter = Counter {
			kind: crate::CounterKind::Valgrind,
			fingerprint: String::new(),
			host: String::new(),
		};
		let report = report(&sampled, None, &counter, None);
		assert_eq!(
			report.0["benchmarks"]["b"]["head"]["instructions"],
			json!({ "value": 200, "unit": "instructions", "lower": 190, "upper": 210 })
		);
		sampled.details.insert("b".to_owned(), base["a"].clone());
		let report = super::report(&sampled, None, &counter, None);
		assert_eq!(
			report.0["benchmarks"]["b"]["head"],
			json!({ "cycles:u": { "value": 100, "unit": "events", "lower": 90, "upper": 110 } })
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
