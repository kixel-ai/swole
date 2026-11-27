//! Each benchmark's simulated costs in a Callgrind profile.
//!
//! A benchmark's hooks zero Callgrind's counters and start instrumenting before it, then stop
//! and dump, naming the dump after the benchmark: a part whose `desc: Trigger:` line is
//! `Client Request: <name>` and whose `totals:` line holds its cost. CodSpeed's instrument
//! hooks also dump a `Metadata: ...` part, and Valgrind dumps once more as the program ends.
//! Each part also lists the cost of every function that ran, which attribute a change to where it
//! happened.

use std::collections::BTreeMap;

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

	/// Adds `other`'s counts to these.
	pub fn add(&mut self, other: &Self) {
		for (count, other) in self.0.iter_mut().zip(other.0) {
			*count += other;
		}
	}

	/// Every measure's name, its count, and its unit: instructions, the estimated cycles, then
	/// each other event. Estimated cycles are linear in the events, so a function's add up to
	/// the benchmark's as each event's do.
	pub fn counts(&self) -> [(&'static str, u64, &'static str); EVENTS.len() + 1] {
		let mut counts = [("instructions", self.ir(), "instructions"); EVENTS.len() + 1];
		counts[1] = (ESTIMATED, self.cycles(), "cycles");
		for (i, ((_, name), &count)) in EVENTS.iter().zip(&self.0).enumerate().skip(1) {
			counts[i + 1] = (name, count, "events");
		}
		counts
	}

	/// Every measure: instructions, the estimated cycles, then each other event.
	pub fn measures(&self) -> Vec<crate::Measure> {
		self.counts()
			.into_iter()
			.map(|(name, count, unit)| crate::Measure::new(name, count as f64, unit))
			.collect()
	}
}

/// What a benchmark's part of one profile measured: its totals, and the self cost of each
/// function that ran, by name, which add up to them.
#[derive(Debug, PartialEq)]
pub struct Benchmark {
	pub name: String,
	pub costs: Costs,
	pub functions: Functions,
}

/// A function a profile names: its demangled name, and the object that defines it, so two
/// static functions of one name in different libraries stay apart.
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct Function {
	pub name: String,
	/// The object's file name, less the hash Cargo suffixes an executable's with, which another
	/// build of the same benchmark names differently.
	pub object: String,
}

/// Each function's self cost, with its recursion levels, which Callgrind writes as `name'2`, merged.
pub type Functions = BTreeMap<Function, Costs>;

/// What `path`'s object is called across builds: its file name without a trailing `-` and 16
/// hex digits.
fn object(path: &str) -> String {
	let file = path.rsplit('/').next().unwrap_or(path);
	match file.rsplit_once('-') {
		Some((stem, hash)) if hash.len() == 16 && hash.bytes().all(|b| b.is_ascii_hexdigit()) => stem.to_owned(),
		_ => file.to_owned(),
	}
}

/// A name written as `(id) name`, `(id)` for one named before, or ` name` outright, which Callgrind
/// numbers per kind of name in `table`.
fn named(table: &mut BTreeMap<u64, String>, spec: &str, what: &str) -> Result<String, String> {
	let spec = spec.trim_start();
	let Some(rest) = spec.strip_prefix('(') else {
		return Ok(spec.to_owned());
	};
	let (id, name) = rest
		.split_once(')')
		.ok_or_else(|| format!("{what}: {spec} is not a name"))?;
	let id: u64 = id.parse().map_err(|_| format!("{what}: {spec} is not a name"))?;
	let name = name.trim_start();
	if name.is_empty() {
		return table
			.get(&id)
			.cloned()
			.ok_or_else(|| format!("{what}: ({id}) is used before it is named"));
	}
	table.insert(id, name.to_owned());
	Ok(name.to_owned())
}

/// Reads a part's cost lines into the self cost of each function.
#[derive(Default)]
struct Reader {
	/// The functions and objects Callgrind named so far, by the ID it abbreviates each later mention to.
	names: BTreeMap<u64, String>,
	objects: BTreeMap<u64, String>,
	/// The object whose functions are being read.
	object: String,
	/// How many positions lead a cost line (`line`, or `instr line`), from `positions:`.
	positions: Option<usize>,
	/// For each count a cost line holds, which of [`EVENTS`] it is, from `events:`.
	columns: Vec<Option<usize>>,
	/// The function whose cost lines are being read.
	current: Option<Function>,
	/// Whether the next cost line is a call's inclusive cost, which is not the caller's own.
	call: bool,
	functions: Functions,
	/// Why the part's lines did not read, reported only if the part measures a benchmark, since
	/// the dump of a program's end is no concern of ours.
	failed: Option<String>,
}

impl Reader {
	/// Reads one line of a part, or notes why it cannot.
	fn line(&mut self, line: &str, what: &str) {
		if self.failed.is_none()
			&& let Err(err) = self.read(line, what)
		{
			self.failed = Some(err);
		}
	}

	fn read(&mut self, line: &str, what: &str) -> Result<(), String> {
		if let Some(list) = line.strip_prefix("positions:") {
			self.positions = Some(list.split_whitespace().count());
		} else if let Some(list) = line.strip_prefix("events:") {
			self.columns = list
				.split_whitespace()
				.map(|name| EVENTS.iter().position(|(event, _)| *event == name))
				.collect();
		} else if let Some(spec) = line.strip_prefix("ob=") {
			self.object = object(&named(&mut self.objects, spec, what)?);
		} else if let Some(spec) = line.strip_prefix("cob=") {
			// Only to learn the object an ID stands for, since a callee's first mention names it.
			named(&mut self.objects, spec, what)?;
		} else if let Some(spec) = line.strip_prefix("fn=") {
			let name = named(&mut self.names, spec, what)?;
			// A recursion level is a function of its own to Callgrind, `name'2`.
			let name = match name.rsplit_once('\'') {
				Some((base, level)) if !level.is_empty() && level.bytes().all(|b| b.is_ascii_digit()) => {
					base.to_owned()
				}
				_ => name,
			};
			self.current = Some(Function {
				name,
				object: self.object.clone(),
			});
		} else if let Some(spec) = line.strip_prefix("cfn=") {
			// Likewise a function.
			named(&mut self.names, spec, what)?;
		} else if line.starts_with("calls=") {
			self.call = true;
		} else if line.starts_with(|c: char| c.is_ascii_digit() || matches!(c, '+' | '-' | '*')) {
			// A cost line is its positions, then a count of each event; Callgrind drops trailing zeros.
			if std::mem::take(&mut self.call) {
				return Ok(());
			}
			let Some(positions) = self.positions else {
				return Err(format!("{what} has a cost before its positions: {line}"));
			};
			let Some(function) = &self.current else {
				return Err(format!("{what} has a cost before any function: {line}"));
			};
			let mut costs = Costs::default();
			for (column, count) in self.columns.iter().zip(line.split_whitespace().skip(positions)) {
				let count = count
					.parse::<u64>()
					.map_err(|_| format!("{what}: {count} in {} is not a count", function.name))?;
				if let Some(column) = column {
					costs.0[*column] = count;
				}
			}
			self.functions.entry(function.clone()).or_default().add(&costs);
		}
		Ok(())
	}
}

/// Each benchmark one Callgrind profile measured, by name, with its count of each of [`EVENTS`]
/// and each function's share of them. `what` names the profile in errors.
pub fn parse(text: &str, what: &str) -> Result<Vec<Benchmark>, String> {
	let mut lines = text.lines();
	if lines.next() != Some("# callgrind format") {
		return Err(format!("{what} is not a Callgrind profile"));
	}
	struct Part<'a> {
		trigger: Option<&'a str>,
		events: Option<&'a str>,
		totals: Option<&'a str>,
		reader: Reader,
	}
	let mut parts: Vec<Part> = Vec::new();
	// A profile may name its events and positions once, before its first part.
	let mut events = None;
	let mut header = Reader::default();
	// Only a part's header lines begin with these keys; its cost lines begin with numbers,
	// `(`, or a `key=` such as `fn=`.
	for line in lines {
		if line.starts_with("part:") {
			// Each part names its functions afresh, since a dump stands alone.
			parts.push(Part {
				trigger: None,
				events,
				totals: None,
				reader: Reader {
					positions: header.positions,
					columns: header.columns.clone(),
					..Reader::default()
				},
			});
			continue;
		}
		let part = match parts.last_mut() {
			Some(part) => &mut part.reader,
			None => &mut header,
		};
		part.line(line, what);
		if let Some(list) = line.strip_prefix("events:") {
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
		if let Some(err) = part.reader.failed {
			return Err(err);
		}
		// What attributes a change to a function is only as good as its costs adding up to the count.
		let mut sum = Costs::default();
		for function in part.reader.functions.values() {
			sum.add(function);
		}
		if sum != costs {
			return Err(format!(
				"{what}: {name}'s functions cost {:?}, not its totals {:?}",
				sum.0, costs.0
			));
		}
		measured.push(Benchmark {
			name: name.to_owned(),
			costs,
			functions: part.reader.functions,
		});
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
			 fn=(1) main\n0 {totals}\n\ntotals: {totals}\n"
		)
	}

	fn profile(parts: &[String]) -> String {
		format!("{HEADER}{}", parts.concat())
	}

	fn ir(found: Vec<Benchmark>) -> Vec<(String, u64)> {
		found.into_iter().map(|b| (b.name, b.costs.ir())).collect()
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
			 desc: Trigger: Client Request: a\npositions: line\nfn=(1) f\n0 13 12 11 10 9 8 7 6 5 4 3 2 1000\n\ntotals: 13 12 11 10 9 8 7 6 5 4 3 2 1000\n"
		);
		let [Benchmark { name, costs, .. }] = parse(&text, "p").unwrap().try_into().unwrap();
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

	/// A part's body of `fn`s, with Callgrind's compressed names and cost lines, then its totals.
	fn functions(body: &str, totals: &str) -> String {
		format!(
			"{HEADER}positions: line\n{EVENTS_LINE}\npart: 1\ndesc: Trigger: Client Request: a\n\n\
			 ob=(1) /out/bench-0123456789abcdef\nfl=(1) f.rs\n{body}\ntotals: {totals}\n"
		)
	}

	/// The names of the functions, in order, each beside its object.
	fn names(found: &Benchmark) -> Vec<(&str, &str)> {
		found
			.functions
			.keys()
			.map(|f| (f.name.as_str(), f.object.as_str()))
			.collect()
	}

	/// The instructions and data reads of the function `name` in `object`.
	fn self_of(found: &[Benchmark], object: &str, name: &str) -> [u64; 2] {
		let function = Function {
			name: name.to_owned(),
			object: object.to_owned(),
		};
		let costs = found[0].functions[&function];
		[costs.ir(), costs.0[1]]
	}

	#[test]
	fn reads_each_functions_self_cost_by_name_not_its_calls() {
		// `(2)` is named by the call that first mentions it, and `calls=`'s line is the callee's
		// inclusive cost, which `main` does not own. `fi=` and `fe=` switch files without
		// changing function; `*`, `+1` and `-1` are compressed positions.
		let text = functions(
			"fn=(1) main\n0 10 1\ncfn=(2) helper\ncalls=3 0\n0 600 60\n+1 5 1\nfi=(2) inlined.rs\n-1 7\nfe=(1)\n*\n\n\
			 fn=(2)\n3 100 20\n\nfn=(1)\n4 1 0\n",
			"123 22",
		);
		let found = parse(&text, "p").unwrap();
		// `main`'s lines under its two mentions add; `*` repeats the position and has no cost.
		assert_eq!(self_of(&found, "bench", "main"), [23, 2]);
		assert_eq!(self_of(&found, "bench", "helper"), [100, 20]);
		assert_eq!(found[0].functions.len(), 2);
	}

	#[test]
	fn merges_recursion_levels_and_skips_positions_beyond_line() {
		let text = functions("fn=(1) f'2\n1 10\n\nfn=(2) f\n1 5\n\nfn=(3) g'x\n1 1\n", "16");
		let found = parse(&text, "p").unwrap();
		assert_eq!(names(&found[0]), [("f", "bench"), ("g'x", "bench")]);
		assert_eq!(self_of(&found, "bench", "f"), [15, 0]);
		// `positions: instr line`.
		let text = functions("fn=(1) f\n0x1000 3 7 2\n", "7 2").replace("positions: line", "positions: instr line");
		assert_eq!(self_of(&parse(&text, "p").unwrap(), "bench", "f"), [7, 2]);
	}

	#[test]
	fn keeps_same_named_functions_of_different_objects_apart() {
		let text = functions(
			"fn=(1) decode\n0 5\n\nob=(2) /lib/libother.so.1\nfn=(1)\n0 7\ncob=(3) /lib/libc.so.6\ncfn=(3) memcpy\ncalls=1 0\n0 90\n\nob=(3)\nfn=(3)\n0 4\n",
			"16",
		);
		let found = parse(&text, "p").unwrap();
		assert_eq!(
			names(&found[0]),
			[
				("decode", "bench"),
				("decode", "libother.so.1"),
				("memcpy", "libc.so.6")
			]
		);
		assert_eq!(self_of(&found, "libother.so.1", "decode")[0], 7);
	}

	#[test]
	fn names_an_object_alike_across_builds() {
		assert_eq!(object("/t/release/deps/x-c40201029a1ad706"), "x");
		assert_eq!(object("/t/release/deps/my-bench-0123456789ABCDEF"), "my-bench");
		assert_eq!(object("/lib/libc.so.6"), "libc.so.6");
		assert_eq!(object("/lib/lib-deadbeef.so"), "lib-deadbeef.so");
	}

	#[test]
	fn names_each_parts_functions_afresh_and_ignores_a_terminations_oddities() {
		let one = |number: u32, trigger: &str, body: &str, totals: &str| {
			format!(
				"part: {number}\ndesc: Trigger: {trigger}\npositions: line\n{EVENTS_LINE}\n{body}\ntotals: {totals}\n"
			)
		};
		let text = profile(&[
			one(1, "Client Request: a", "fn=(1) a\n0 4\n", "4"),
			// This part reuses ID 1 for another function, and the next is unreadable.
			one(2, "Client Request: b", "fn=(1) b\n0 5\n", "5"),
			one(3, "Program termination", "fn=(9)\n0 5\n", "5"),
		]);
		let found = parse(&text, "p").unwrap();
		assert_eq!(names(&found[0]), [("a", "")]);
		assert_eq!(names(&found[1]), [("b", "")]);
	}

	#[test]
	fn refuses_functions_that_do_not_add_up_to_the_totals() {
		let err = parse(&functions("fn=(1) f\n0 3\n", "4"), "p").unwrap_err();
		assert!(err.contains("functions cost"), "{err}");
		let err = parse(&functions("fn=(1)\n0 4\n", "4"), "p").unwrap_err();
		assert!(err.contains("before it is named"), "{err}");
		let err = parse(&functions("0 4\n", "4"), "p").unwrap_err();
		assert!(err.contains("before any function"), "{err}");
		let err = parse(&functions("fn=(1) f\n0 x\n", "4"), "p").unwrap_err();
		assert!(err.contains("not a count"), "{err}");
	}
}
