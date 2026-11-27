//! Each benchmark's simulated costs in a Callgrind profile.
//!
//! A benchmark's hooks zero Callgrind's counters and start instrumenting before it, then stop
//! and dump, naming the dump after the benchmark: a part whose `desc: Trigger:` line is
//! `Client Request: <name>` and whose `totals:` line holds its cost. CodSpeed's instrument
//! hooks also dump a `Metadata: ...` part, and Valgrind dumps once more as the program ends.

/// What a `desc: Trigger:` line names a dump that measures nothing.
const TERMINATION: &str = "Program termination";

/// The prefix of a dump a client request asked for.
const REQUEST: &str = "Client Request: ";

/// The prefix of the dump CodSpeed's hooks write to name their integration, so reserved: a
/// benchmark cannot be told from it by name.
const METADATA: &str = "Metadata: ";

/// How Callgrind simulates each count's caches and branches: CodSpeed's geometry, a 32 KiB 8-way
/// L1 for instructions and one for data and an 8 MiB 16-way last level, all of 64-byte lines,
/// named outright so Valgrind never sizes them by the host's CPUID, and its branch predictor.
pub const SIMULATION: [&str; 5] = [
	"--cache-sim=yes",
	"--branch-sim=yes",
	"--I1=32768,8,64",
	"--D1=32768,8,64",
	"--LL=8388608,16,64",
];

/// The estimated cycles' weights: a cycle an instruction, then a first-level miss's trip to
/// the last level, a last-level miss's to memory, and a mispredicted branch's flushed pipeline.
const L1_MISS: u64 = 10;
const LL_MISS: u64 = 100;
const MISPREDICT: u64 = 20;

/// The model [`Costs::cycles`] estimates with, as the fingerprint records it.
pub const CYCLES: &str = "Ir + 10 (I1mr + D1mr + D1mw) + 100 (ILmr + DLmr + DLmw) + 20 (Bcm + Bim)";

/// Every event [`SIMULATION`] counts, by Callgrind's name, and the measure each reports as.
pub const EVENTS: [(&str, &str); 13] = [
	("Ir", "instructions"),
	("Dr", "data reads"),
	("Dw", "data writes"),
	("I1mr", "I1 misses"),
	("D1mr", "D1 read misses"),
	("D1mw", "D1 write misses"),
	("ILmr", "LL instruction misses"),
	("DLmr", "LL read misses"),
	("DLmw", "LL write misses"),
	("Bc", "conditional branches"),
	("Bcm", "conditional mispredicts"),
	("Bi", "indirect branches"),
	("Bim", "indirect mispredicts"),
];

/// The measure estimated cycles report as.
pub const ESTIMATED: &str = "estimated cycles";

/// One benchmark's count of each of [`EVENTS`], in its order.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Costs(pub [u64; EVENTS.len()]);

impl Costs {
	/// The count of the event Callgrind names `event`.
	fn get(&self, event: &str) -> u64 {
		let index = EVENTS
			.iter()
			.position(|(name, _)| *name == event)
			.expect("every event asked for is one of EVENTS");
		self.0[index]
	}

	/// Instructions executed.
	pub fn ir(&self) -> u64 {
		self.get("Ir")
	}

	/// The cycles a core like the simulated one would take, by [`CYCLES`]'s model.
	pub fn cycles(&self) -> u64 {
		let sum = |events: &[&str]| events.iter().map(|event| self.get(event)).sum::<u64>();
		self.ir()
			+ L1_MISS * sum(&["I1mr", "D1mr", "D1mw"])
			+ LL_MISS * sum(&["ILmr", "DLmr", "DLmw"])
			+ MISPREDICT * sum(&["Bcm", "Bim"])
	}

	/// Every measure: instructions, the estimated cycles, then each other event.
	pub fn measures(&self) -> Vec<crate::Measure> {
		let mut measures = vec![
			crate::Measure::new("instructions", self.ir() as f64, "instructions"),
			crate::Measure::new(ESTIMATED, self.cycles() as f64, "cycles"),
		];
		for ((_, name), &count) in EVENTS.iter().zip(&self.0).skip(1) {
			measures.push(crate::Measure::new(name, count as f64, "events"));
		}
		measures
	}
}

/// Each benchmark one Callgrind profile measured, by name, with its count of each of [`EVENTS`].
/// `what` names the profile in errors.
pub fn parse(text: &str, what: &str) -> Result<Vec<(String, Costs)>, String> {
	let mut lines = text.lines();
	if lines.next() != Some("# callgrind format") {
		return Err(format!("{what} is not a Callgrind profile"));
	}
	struct Part<'a> {
		trigger: Option<&'a str>,
		events: Option<&'a str>,
		totals: Option<&'a str>,
	}
	let mut parts: Vec<Part> = Vec::new();
	// A profile may name its events once, before its first part.
	let mut events = None;
	// Only a part's header lines begin with these keys; its cost lines begin with numbers,
	// `(`, or a `key=` such as `fn=`.
	for line in lines {
		if line.starts_with("part:") {
			parts.push(Part {
				trigger: None,
				events,
				totals: None,
			});
		} else if let Some(list) = line.strip_prefix("events:") {
			events = Some(list);
			if let Some(part) = parts.last_mut() {
				part.events = events;
			}
		} else if let Some(part) = parts.last_mut() {
			if let Some(trigger) = line.strip_prefix("desc: Trigger: ") {
				part.trigger = Some(trigger);
			} else if let Some(totals) = line.strip_prefix("totals:") {
				part.totals.get_or_insert(totals);
			}
		}
	}
	if parts.is_empty() {
		return Err(format!("{what} holds no part"));
	}
	let mut measured = Vec::new();
	for part in parts {
		let Some(trigger) = part.trigger else {
			return Err(format!("{what} has a part without a trigger"));
		};
		if trigger == TERMINATION {
			continue;
		}
		let Some(name) = trigger.strip_prefix(REQUEST) else {
			return Err(format!("{what} has a dump by an unknown trigger: {trigger}"));
		};
		if name.starts_with(METADATA) {
			continue;
		}
		let Some(events) = part.events else {
			return Err(format!("{what}: {name} names no events"));
		};
		let events: Vec<&str> = events.split_whitespace().collect();
		let Some(totals) = part.totals else {
			return Err(format!("{what}: {name} has no totals"));
		};
		let totals = totals
			.split_whitespace()
			.map(str::parse::<u64>)
			.collect::<Result<Vec<_>, _>>()
			.map_err(|_| format!("{what}: {name}'s totals are not counts: {totals}"))?;
		let mut costs = Costs::default();
		for ((event, _), cost) in EVENTS.iter().zip(&mut costs.0) {
			let Some(index) = events.iter().position(|name| name == event) else {
				return Err(format!(
					"{what}: {name} counts no {event}, which Callgrind's cache and branch simulation does"
				));
			};
			// Callgrind drops a line's trailing zero costs.
			*cost = totals.get(index).copied().unwrap_or(0);
		}
		if costs.ir() == 0 {
			return Err(format!("{what}: {name} ran no instructions"));
		}
		measured.push((name.to_owned(), costs));
	}
	Ok(measured)
}

#[cfg(test)]
mod tests {
	use super::*;

	const HEADER: &str = "# callgrind format\nversion: 1\ncreator: callgrind-3.27.1\npid: 7\ncmd: ./bench\n";

	/// The events line Callgrind writes under [`SIMULATION`].
	const EVENTS_LINE: &str = "events: Ir Dr Dw I1mr D1mr D1mw ILmr DLmr DLmw Bc Bcm Bi Bim";

	fn part(number: u32, trigger: &str, totals: &str) -> String {
		format!(
			"part: {number}\n\ndesc: I1 cache: 32768 B, 64 B, 8-way associative\ndesc: Timerange: Basic block 0 - 9\n\
			 desc: Trigger: {trigger}\n\npositions: line\n{EVENTS_LINE}\nsummary: 0\n\nob=(1) /bench\nfl=(1) ???\n\
			 fn=(1) main\n0 12 3\n\ntotals: {totals}\n"
		)
	}

	fn profile(parts: &[String]) -> String {
		format!("{HEADER}{}", parts.concat())
	}

	fn ir(found: Vec<(String, Costs)>) -> Vec<(String, u64)> {
		found.into_iter().map(|(name, costs)| (name, costs.ir())).collect()
	}

	#[test]
	fn reads_each_benchmark_and_skips_metadata_and_termination() {
		let text = profile(&[
			part(1, "Client Request: Metadata: codspeed-rust 6.0.0", "0"),
			part(2, "Client Request: benches/varint.rs::encode[1]", "12345 3"),
			part(3, "Client Request: benches/varint.rs::decode", "6789"),
			part(4, "Program termination", "0"),
		]);
		assert_eq!(
			ir(parse(&text, "p").unwrap()),
			[
				("benches/varint.rs::encode[1]".to_owned(), 12345),
				("benches/varint.rs::decode".to_owned(), 6789)
			]
		);
	}

	#[test]
	fn reads_every_event_by_name_and_estimates_cycles() {
		// Named in another order than Callgrind writes them, before the first part.
		let text = format!(
			"{HEADER}events: Bim Bi Bcm Bc DLmw DLmr ILmr D1mw D1mr I1mr Dw Dr Ir\npart: 1\n\
			 desc: Trigger: Client Request: a\ntotals: 13 12 11 10 9 8 7 6 5 4 3 2 1000\n"
		);
		let [(name, costs)] = parse(&text, "p").unwrap().try_into().unwrap();
		assert_eq!(name, "a");
		assert_eq!(costs.0, [1000, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13]);
		assert_eq!(
			costs.cycles(),
			1000 + 10 * (4 + 5 + 6) + 100 * (7 + 8 + 9) + 20 * (11 + 13)
		);
		let measures = costs.measures();
		assert_eq!(measures.len(), EVENTS.len() + 1);
		assert_eq!(
			measures[..3]
				.iter()
				.map(|m| (m.name.as_str(), m.value))
				.collect::<Vec<_>>(),
			[
				("instructions", 1000.0),
				(ESTIMATED, costs.cycles() as f64),
				("data reads", 2.0)
			]
		);
	}

	#[test]
	fn a_termination_dump_alone_measures_nothing() {
		assert_eq!(
			parse(&profile(&[part(1, "Program termination", "0")]), "p").unwrap(),
			[]
		);
	}

	#[test]
	fn refuses_an_unknown_trigger() {
		let err = parse(&profile(&[part(1, "Client Request", "5")]), "p").unwrap_err();
		assert!(err.contains("unknown trigger"), "{err}");
		let err = parse(&profile(&[part(1, "Interactive", "5")]), "p").unwrap_err();
		assert!(err.contains("unknown trigger"), "{err}");
	}

	#[test]
	fn refuses_zero_instructions_including_a_dropped_trailing_zero() {
		let err = parse(&profile(&[part(1, "Client Request: a", "0 4")]), "p").unwrap_err();
		assert!(err.contains("ran no instructions"), "{err}");
		let text = format!(
			"{HEADER}events: Dr Dw I1mr D1mr D1mw ILmr DLmr DLmw Bc Bcm Bi Bim Ir\npart: 1\n\
			 desc: Trigger: Client Request: a\ntotals: 4\n"
		);
		let err = parse(&text, "p").unwrap_err();
		assert!(err.contains("ran no instructions"), "{err}");
	}

	#[test]
	fn refuses_a_benchmark_without_totals_or_an_event() {
		let text = format!("{HEADER}{EVENTS_LINE}\npart: 1\ndesc: Trigger: Client Request: a\nsummary: 0\n");
		assert!(parse(&text, "p").unwrap_err().contains("no totals"));
		// Counted without the simulation.
		let text = format!("{HEADER}events: Ir\npart: 1\ndesc: Trigger: Client Request: a\ntotals: 4\n");
		let err = parse(&text, "p").unwrap_err();
		assert!(err.contains("counts no Dr"), "{err}");
		let text = format!("{HEADER}{EVENTS_LINE}\npart: 1\ndesc: Trigger: Client Request: a\ntotals: 4 x\n");
		assert!(parse(&text, "p").unwrap_err().contains("not counts"));
	}

	#[test]
	fn refuses_what_is_not_a_profile() {
		assert!(parse("", "p").unwrap_err().contains("not a Callgrind profile"));
		assert!(parse(HEADER, "p").unwrap_err().contains("no part"));
		let text = format!("{HEADER}part: 1\ntotals: 4\n");
		assert!(parse(&text, "p").unwrap_err().contains("without a trigger"));
	}
}
