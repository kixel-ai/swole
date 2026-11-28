//! Where a benchmark's change happened: the functions whose own cost moved most, from the
//! per-function costs of the Callgrind profiles sim already reads.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write;

use crate::Counts;
use crate::callgrind::{Costs, Function, Functions};

/// The most functions shown for one measure of one benchmark.
const MOST: usize = 5;

/// The fewest shown, when it has that many that moved with the benchmark, before the ones shown
/// explain the majority of its change.
const LEAST: usize = 3;

/// For each benchmark both sides counted and some measure of which changed, the functions whose
/// self cost moved that measure most, a Markdown table; empty when no benchmark has any.
///
/// Self costs partition a benchmark's count, so the functions' changes add up to the benchmark's
/// exactly, which a function's inclusive cost, double-counting a recursion, would not. A
/// benchmark counted more than once is marked, since its functions' costs, from its first
/// run alone, may move with the scheduler against the median.
pub fn table(before: &Counts, after: &Counts) -> String {
	let mut out = String::new();
	for (name, base) in &before.functions {
		let Some(head) = after.functions.get(name) else {
			continue;
		};
		let (base_total, head_total) = (sum(base), sum(head));
		// A name two objects define is shown with its object, to tell them apart.
		let mut objects: BTreeMap<&str, BTreeSet<&str>> = BTreeMap::new();
		for function in base.keys().chain(head.keys()) {
			objects.entry(&function.name).or_default().insert(&function.object);
		}
		let ambiguous = |function: &Function| objects[function.name.as_str()].len() > 1;
		let shown = name.replace('|', "\\|");
		// A benchmark counted more than once reports the median of its runs, but these functions
		// are the first run's, which the scheduler may have moved the other way.
		let shown = if before.spread.contains_key(name) || after.spread.contains_key(name) {
			format!("{shown} (first run)")
		} else {
			shown
		};
		let totals = base_total.counts().into_iter().zip(head_total.counts());
		for (index, ((measure, was, _), (_, now, _))) in totals.enumerate() {
			if was == now {
				continue;
			}
			for (function, before, after) in shifted(base, head, index, was, now) {
				if out.is_empty() {
					out.push_str(
						"\nFunctions behind each change, by self cost, up to five for each measure:\n\n\
						 | benchmark | measure | function | base | head | change (of benchmark) |\n\
						 |---|---|---|---:|---:|---:|\n",
					);
				}
				let change = after as i128 - before as i128;
				// A function one side's profile lacks entirely, not merely idle there, may only have gone unseen.
				let kind = match (base.contains_key(function), head.contains_key(function)) {
					(false, _) => " (not in base)",
					(_, false) => " (not in head)",
					_ => "",
				};
				let qualified = if ambiguous(function) {
					format!("{} ({})", function.name, function.object)
				} else {
					function.name.clone()
				};
				let function = qualified.replace('|', "\\|");
				// A measure the base never had has no share to speak of.
				let share = if was == 0 {
					String::new()
				} else {
					format!(" ({:+.2}%)", change as f64 / was as f64 * 100.0)
				};
				writeln!(
					out,
					"| {shown} | {measure} | `{function}`{kind} | {before} | {after} | {change:+}{share} |"
				)
				.expect("a String takes every write");
			}
		}
	}
	out
}

/// The benchmark's own cost of each measure: its functions' added.
fn sum(functions: &Functions) -> Costs {
	let mut total = Costs::default();
	for costs in functions.values() {
		total.add(costs);
	}
	total
}

/// The functions that moved measure `index` the way the benchmark did, from `was` to `now`,
/// with their base and head costs, largest first: at least [`LEAST`] and at most [`MOST`], and
/// past the least only until they explain the majority of the benchmark's change.
fn shifted<'a>(
	base: &'a Functions,
	head: &'a Functions,
	index: usize,
	was: u64,
	now: u64,
) -> Vec<(&'a Function, u64, u64)> {
	let value =
		|functions: &Functions, function: &Function| functions.get(function).map_or(0, |costs| costs.counts()[index].1);
	let names: BTreeSet<&Function> = base.keys().chain(head.keys()).collect();
	let mut moved: Vec<(&Function, u64, u64)> = names
		.into_iter()
		.map(|function| (function, value(base, function), value(head, function)))
		// What moved with the benchmark; a function that moved against it only offset some.
		.filter(|(_, before, after)| if now > was { after > before } else { after < before })
		.collect();
	moved.sort_by_key(|(name, before, after)| (std::cmp::Reverse(before.abs_diff(*after)), *name));
	let net = was.abs_diff(now);
	let mut explained = 0u64;
	let mut shown = Vec::new();
	for (name, before, after) in moved {
		if shown.len() >= MOST || (shown.len() >= LEAST && explained * 2 > net) {
			break;
		}
		explained += before.abs_diff(after);
		shown.push((name, before, after));
	}
	shown
}

#[cfg(test)]
mod tests {
	use super::*;

	/// A benchmark's functions, each its instructions, which cost a quarter as many data reads. A
	/// name `function@object` is in that object, else in `bin`.
	fn functions(costs: &[(&str, u64)]) -> Functions {
		costs
			.iter()
			.map(|(name, ir)| {
				let mut costs = Costs::default();
				costs.0[0] = *ir;
				costs.0[1] = *ir / 4;
				let (name, object) = name.split_once('@').unwrap_or((name, "bin"));
				(
					Function {
						name: name.to_owned(),
						object: object.to_owned(),
					},
					costs,
				)
			})
			.collect()
	}

	fn sides(base: &[(&str, u64)], head: &[(&str, u64)]) -> (Counts, Counts) {
		let counter = |costs: &[(&str, u64)]| Counts {
			fingerprint: String::new(),
			host: String::new(),
			counts: BTreeMap::new(),
			spread: BTreeMap::new(),
			threaded: BTreeSet::new(),
			details: BTreeMap::new(),
			throughput: BTreeMap::new(),
			functions: BTreeMap::from([("bench".to_owned(), functions(costs))]),
		};
		(counter(base), counter(head))
	}

	#[test]
	fn names_the_functions_behind_a_change_largest_first() {
		let (before, after) = sides(
			&[("decode", 1000), ("alloc", 500), ("main", 100), ("gone", 40)],
			&[("decode", 1400), ("alloc", 560), ("main", 100), ("new", 30)],
		);
		let table = table(&before, &after);
		let rows: Vec<&str> = table.lines().filter(|row| row.contains("instructions")).collect();
		// +400 and +60 and +30 and -40 make +450: decode alone is the majority.
		assert_eq!(rows.len(), 3, "{table}");
		assert!(rows[0].contains("`decode` | 1000 | 1400 | +400 (+24.39%)"), "{table}");
		assert!(rows[1].contains("`alloc` | 500 | 560 | +60"), "{table}");
		assert!(rows[2].contains("`new` (not in base) | 0 | 30 | +30"), "{table}");
		assert!(!table.contains("gone"), "{table}");
	}

	#[test]
	fn names_a_removed_function_when_the_benchmark_shrank_and_skips_an_unchanged_measure() {
		let (before, after) = sides(&[("a", 400), ("b", 100)], &[("a", 400)]);
		let table = table(&before, &after);
		assert!(
			table.contains("`b` (not in head) | 100 | 0 | -100 (-20.00%)"),
			"{table}"
		);
		assert!(!table.contains("`a`"), "{table}");
		// Identical sides print nothing.
		let (before, after) = sides(&[("a", 400)], &[("a", 400)]);
		assert_eq!(super::table(&before, &after), "");
	}

	#[test]
	fn lists_more_than_the_biggest_until_the_majority_is_explained() {
		let spread: Vec<(String, u64)> = (0..8).map(|i| (format!("f{i}"), 100)).collect();
		let head: Vec<(String, u64)> = spread.iter().map(|(name, ir)| (name.clone(), ir + 10)).collect();
		fn as_refs(list: &[(String, u64)]) -> Vec<(&str, u64)> {
			list.iter().map(|(n, v)| (n.as_str(), *v)).collect()
		}
		let (before, after) = sides(&as_refs(&spread), &as_refs(&head));
		// Eight functions grew by the same amount: five show, since three do not make a majority.
		let table = table(&before, &after);
		assert_eq!(
			table.lines().filter(|row| row.contains("| instructions |")).count(),
			5,
			"{table}"
		);
	}

	#[test]
	fn shows_the_object_of_a_name_two_objects_define() {
		let (before, after) = sides(
			&[("decode@liba", 1000), ("decode@libb", 1000), ("main", 10)],
			&[("decode@liba", 1500), ("decode@libb", 950), ("main", 10)],
		);
		let table = table(&before, &after);
		// Merged by name alone, +500 and -50 would read as +450 of one function.
		assert!(table.contains("`decode (liba)` | 1000 | 1500 | +500"), "{table}");
		assert!(!table.contains("libb"), "{table}");
		assert!(!table.contains("`main"), "{table}");
	}

	#[test]
	fn is_empty_for_a_benchmark_one_side_lacks_and_marks_one_counted_more_than_once() {
		let (before, mut after) = sides(&[("a", 400)], &[("a", 500)]);
		after.functions.insert("other".to_owned(), functions(&[("a", 1)]));
		after.spread.insert("bench".to_owned(), (1, 2));
		let table = table(&before, &after);
		assert!(table.contains("| bench (first run) |"), "{table}");
		assert!(!table.contains("| other"), "{table}");
	}
}
