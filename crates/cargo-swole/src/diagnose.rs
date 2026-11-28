//! What a benchmark's counts say about why it runs as it does: ratios derived from the measures a
//! run took, and the likely cause of a regression between two sides.

use crate::Measure;
use crate::metrics::shown;

/// A measure's value, and the least and most it may be when sampled.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Span {
	value: f64,
	least: f64,
	most: f64,
	sampled: bool,
}

impl Span {
	fn of(measure: &Measure) -> Self {
		let (least, most) = measure.spread.unwrap_or((measure.value, measure.value));
		Self {
			value: measure.value,
			least,
			most,
			sampled: measure.spread.is_some(),
		}
	}

	fn add(self, other: Self) -> Self {
		Self {
			value: self.value + other.value,
			least: self.least + other.least,
			most: self.most + other.most,
			sampled: self.sampled || other.sampled,
		}
	}

	/// `self` over `other`, times `scale`, its spread the widest the sides' allow; none where
	/// `other` may be zero.
	fn over(self, other: Self, scale: f64) -> Option<Self> {
		(other.least > 0.0).then(|| Self {
			value: scale * self.value / other.value,
			least: scale * self.least / other.most,
			most: scale * self.most / other.least,
			sampled: self.sampled || other.sampled,
		})
	}

	/// `self`'s share of itself and `other`, as a percentage, its spread the widest they allow.
	fn share(self, other: Self) -> Option<Self> {
		let share = |part: f64, rest: f64| 100.0 * part / (part + rest);
		(self.least + other.least > 0.0 && self.value + other.value > 0.0).then(|| Self {
			value: share(self.value, other.value),
			least: share(self.least, other.most),
			most: share(self.most, other.least),
			sampled: self.sampled || other.sampled,
		})
	}

	fn measure(self, name: &str, unit: &'static str) -> Measure {
		Measure {
			spread: self.sampled.then_some((self.least, self.most)),
			..Measure::new(name, self.value, unit)
		}
	}
}

/// The value of the measure `name` counts in, among `measures`: a count, not one derived per byte.
fn find(measures: &[Measure], name: &str) -> Option<Span> {
	measures
		.iter()
		.find(|measure| measure.name == name && matches!(measure.unit, "events" | "instructions" | "ns"))
		.map(Span::of)
}

/// The sum of the measures `names`, when every one is among `measures`.
fn sum(measures: &[Measure], names: &[&str]) -> Option<Span> {
	names
		.iter()
		.map(|name| find(measures, name))
		.reduce(|a, b| Some(a?.add(b?)))
		.flatten()
}

/// Each native cache or branch miss counter, and its misses per thousand instructions.
const NATIVE: [(&str, &str); 6] = [
	("L1-dcache-load-misses", "L1d MPKI"),
	("L1-icache-load-misses", "L1i MPKI"),
	("LLC-load-misses", "LLC MPKI"),
	("dTLB-load-misses", "dTLB MPKI"),
	("iTLB-load-misses", "iTLB MPKI"),
	("branch-misses", "branch MPKI"),
];

/// Each of Callgrind's simulated misses, and its misses per thousand instructions.
const SIMULATED: [(&[&str], &str); 4] = [
	(&["I1 misses"], "L1i MPKI"),
	(&["D1 read misses", "D1 write misses"], "L1d MPKI"),
	(
		&["LL instruction misses", "LL read misses", "LL write misses"],
		"LL MPKI",
	),
	(&["conditional mispredicts", "indirect mispredicts"], "branch MPKI"),
];

/// The ratios that derive from a benchmark's `measures`, each where the run took what it divides:
/// misses per thousand instructions and the branch miss rate, natively in each ring or as
/// Callgrind simulates them, and the kernel's share of the instructions; and when `timed`, as
/// `bench` is, the clock its cycles over its user time estimate where it counted no reference
/// cycles. A ratio of a sampled measure is sampled, its spread the widest its inputs'
/// allow.
pub fn derive(measures: &[Measure], timed: bool) -> Vec<Measure> {
	let mut out = Vec::new();
	let mut per_thousand = |misses: Option<Span>, instructions: Option<Span>, name: &str| {
		if let Some(mpki) = misses.zip(instructions).and_then(|(m, i)| m.over(i, 1000.0)) {
			out.push(mpki.measure(name, "MPKI"));
		}
	};
	for ring in ["u", "k"] {
		let instructions = find(measures, &format!("instructions:{ring}"));
		for (event, name) in NATIVE {
			per_thousand(
				find(measures, &format!("{event}:{ring}")),
				instructions,
				&format!("{name}:{ring}"),
			);
		}
	}
	let instructions = find(measures, "instructions");
	for (events, name) in SIMULATED {
		per_thousand(sum(measures, events), instructions, name);
	}
	let rate = |misses: Option<Span>, branches: Option<Span>, name: &str| {
		Some(misses?.over(branches?, 100.0)?.measure(name, "%"))
	};
	for ring in ["u", "k"] {
		out.extend(rate(
			find(measures, &format!("branch-misses:{ring}")),
			find(measures, &format!("branches:{ring}")),
			&format!("branch miss rate:{ring}"),
		));
	}
	out.extend(rate(
		sum(measures, &["conditional mispredicts", "indirect mispredicts"]),
		sum(measures, &["conditional branches", "indirect branches"]),
		"branch miss rate",
	));
	let share = |part: Option<Span>, rest: Option<Span>, name: &str| Some(part?.share(rest?)?.measure(name, "%"));
	out.extend(share(
		find(measures, "instructions:k"),
		find(measures, "instructions:u"),
		"kernel instruction share",
	));
	// Without reference cycles, user time is the only clock: it covers what a sample ran between
	// calls, and the timed process rather than the counted one, so it only estimates.
	if timed && !measures.iter().any(|measure| measure.name == "cycles/ref-cycles:u") {
		out.extend(
			find(measures, "cycles:u")
				.zip(find(measures, "user time"))
				.and_then(|(cycles, user)| cycles.over(user, 1.0))
				.map(|clock| clock.measure("estimated clock:u", "GHz")),
		);
	}
	out
}

/// How busy the CPUs were over a benchmark's samples, each the CPU time its process used, user
/// and system, and the wall clock that covers: their total over their total, sampled within the
/// least and most one sample was, since the scheduler moves it. Under one, it waited off the CPU;
/// over one, it ran in parallel.
pub fn busy(samples: impl IntoIterator<Item = (u64, u128)>) -> Option<Measure> {
	let (mut cpu, mut wall) = (0u128, 0u128);
	let (mut least, mut most) = (f64::INFINITY, 0.0f64);
	for (cpu_ns, wall_ns) in samples.into_iter().filter(|&(_, wall_ns)| wall_ns > 0) {
		let busy = cpu_ns as f64 / wall_ns as f64;
		(cpu, wall) = (cpu + u128::from(cpu_ns), wall + wall_ns);
		(least, most) = (least.min(busy), most.max(busy));
	}
	(wall > 0).then(|| Measure {
		spread: Some((least, most)),
		..Measure::new("CPUs busy", cpu as f64 / wall as f64, "CPUs")
	})
}

/// The kernel's share of a timed benchmark's CPU time over its samples, each the user and system
/// time its process used: their totals' share, sampled within the least and most one sample's
/// own pair gave, since the tick splits a sample's CPU time between them. A count's rusage covers
/// one call, so only a timed run's many samples say how the kernel shares it.
pub fn kernel_share(samples: impl IntoIterator<Item = (u64, u64)>) -> Option<Measure> {
	let share = |user: f64, system: f64| 100.0 * system / (user + system);
	let (mut user, mut system) = (0.0, 0.0);
	let (mut least, mut most) = (f64::INFINITY, 0.0f64);
	for (user_ns, system_ns) in samples.into_iter().filter(|&(user, system)| user + system > 0) {
		let (u, s) = (user_ns as f64, system_ns as f64);
		(user, system) = (user + u, system + s);
		(least, most) = (least.min(share(u, s)), most.max(share(u, s)));
	}
	(user + system > 0.0).then(|| Measure {
		spread: Some((least, most)),
		..Measure::new("kernel time share", share(user, system), "%")
	})
}

/// How far a measure must move, by each rule, to say why a benchmark regressed: far past what a
/// rerun moves it, so a rule fires on a cause rather than on noise.
mod moved {
	/// Points a top-down share must gain.
	pub const TOPDOWN: f64 = 3.0;
	/// Points the kernel's share must gain.
	pub const KERNEL: f64 = 5.0;
	/// Points the branch miss rate must gain.
	pub const MISS_RATE: f64 = 0.5;
	/// The share misses per thousand instructions must grow by, and the least they must gain.
	pub const MPKI: (f64, f64) = (0.25, 0.1);
	/// The share instructions must grow by, past which more work explains a slower time.
	pub const INSTRUCTIONS: f64 = 0.01;
	/// The share cycles a reference cycle must fall by.
	pub const CLOCK: f64 = 0.02;
	/// What the CPUs busy must lose, and stay under, for a benchmark to wait off the CPU.
	pub const BUSY: (f64, f64) = (0.1, 0.9);
	/// The share of the off-CPU time a call gained that one kind of wait must hold to be named.
	pub const WAIT: f64 = 0.5;
	/// How little a count may move to be the same, and an MPKI.
	pub const SAME: f64 = 0.005;
	pub const SAME_MPKI: (f64, f64) = (0.1, 0.05);
	/// The time change a code-alignment shift moves a benchmark by, at most.
	pub const ALIGNMENT: (f64, f64) = (0.01, 0.03);
}

/// One measure on both sides.
struct Moved {
	name: String,
	unit: &'static str,
	base: Span,
	head: Span,
}

impl Moved {
	/// Whether the head is past the base rather than within their spreads, an unsampled side's
	/// spread its one value.
	fn beyond(&self) -> bool {
		self.head.least > self.base.most || self.head.most < self.base.least
	}

	fn delta(&self) -> f64 {
		self.head.value - self.base.value
	}

	/// The measure's two values, as a cause cites them.
	fn cited(&self) -> String {
		format!(
			"{} {} → {}",
			self.name,
			shown(self.base.value, self.unit),
			shown(self.head.value, self.unit)
		)
	}
}

/// The sides of a benchmark, which find each measure on both.
struct Sides<'a> {
	base: &'a [Measure],
	head: &'a [Measure],
}

impl Sides<'_> {
	fn get(&self, name: &str) -> Option<Moved> {
		let find = |side: &[Measure]| side.iter().find(|measure| measure.name == name).cloned();
		let (base, head) = (find(self.base)?, find(self.head)?);
		(base.unit == head.unit).then(|| Moved {
			name: name.to_owned(),
			unit: head.unit,
			base: Span::of(&base),
			head: Span::of(&head),
		})
	}

	/// The first of `names` that gained `points` percentage points or more.
	fn gained(&self, names: &[&str], points: f64) -> Option<String> {
		names.iter().find_map(|name| {
			let moved = self.get(name)?;
			(moved.beyond() && moved.delta() >= points).then(|| moved.cited())
		})
	}

	/// The first of `names` that grew by `share` of itself and by `least` or more.
	fn grew(&self, names: &[&str], (share, least): (f64, f64)) -> Option<String> {
		names.iter().find_map(|name| {
			let moved = self.get(name)?;
			let delta = moved.delta();
			(moved.beyond() && delta >= least && delta >= share * moved.base.value).then(|| moved.cited())
		})
	}

	/// The first of `names` that fell by `share` of itself or more.
	fn fell(&self, names: &[&str], share: f64) -> Option<String> {
		names.iter().find_map(|name| {
			let moved = self.get(name)?;
			(moved.beyond() && -moved.delta() >= share * moved.base.value).then(|| moved.cited())
		})
	}

	/// The narrowest kind of wait that holds half or more of the off-CPU time a call gained, as a
	/// trace splits it, and how it moved: a lock's wait lies inside a futex's, inside the time it
	/// blocked.
	fn waited(&self) -> Option<(&'static str, Moved)> {
		let gained = |name: &str| self.get(name).map_or(0.0, |moved| moved.delta());
		let off = gained("blocked time") + gained("runnable time");
		[
			("lock contention", "lock wait time"),
			("futex wait", "futex wait time"),
			("blocked", "blocked time"),
			("runnable", "runnable time"),
		]
		.into_iter()
		.find_map(|(label, name)| {
			let moved = self.get(name)?;
			(off > 0.0 && moved.beyond() && moved.delta() >= moved::WAIT * off).then_some((label, moved))
		})
	}

	/// Whether `name` held, within `share` of itself or `least`, on both sides.
	fn held(&self, name: &str, (share, least): (f64, f64)) -> bool {
		self.get(name)
			.is_some_and(|moved| moved.delta().abs() <= least.max(share * moved.base.value.abs()))
	}
}

/// Why a benchmark likely regressed from `base` to `head`, in a phrase or two citing the measures
/// that say so, or none when none says: with `timed`, the time's relative change, as `bench`
/// pairs it; without, a count's regression, whose headline is its instructions. A sampled measure
/// counts only past its spread.
pub fn cause(base: &[Measure], head: &[Measure], timed: Option<f64>) -> Option<String> {
	let sides = Sides { base, head };
	let mut causes: Vec<String> = Vec::new();
	let mut add = |label: &str, evidence: Option<String>| {
		if let Some(evidence) = evidence {
			causes.push(format!("{label} ({evidence})"));
		}
	};
	if timed.is_some() {
		// Only a clock counted beside its reference: the estimate's user time is another process's,
		// and holds what a sample runs between calls, which a slower hook moves at the same clock.
		add("throttled", sides.fell(&["cycles/ref-cycles:u"], moved::CLOCK));
		if let Some(busy) = sides.get("CPUs busy").filter(|busy| {
			let (lost, under) = moved::BUSY;
			busy.beyond() && -busy.delta() >= lost && busy.head.value < under
		}) {
			match sides.waited() {
				Some((label, wait)) => add(label, Some(format!("{}, {}", busy.cited(), wait.cited()))),
				None => add("off-CPU", Some(busy.cited())),
			}
		}
		add(
			"more instructions",
			sides.grew(&["instructions:u"], (moved::INSTRUCTIONS, 0.0)),
		);
	}
	// Misses first, which say where the time went, then the breakdown's share, which says only how
	// much of it. A parent share names only its broad category: backend bound rises with core-bound
	// arithmetic too, and bad speculation with machine clears.
	let memory = sides
		.grew(
			&["LLC MPKI:u", "LL MPKI", "L1d MPKI:u", "L1d MPKI", "dTLB MPKI:u"],
			moved::MPKI,
		)
		.or_else(|| sides.gained(&["topdown memory bound"], moved::TOPDOWN));
	let backend = memory
		.is_none()
		.then(|| sides.gained(&["topdown backend bound"], moved::TOPDOWN))
		.flatten();
	add("memory-bound", memory);
	add("backend-bound", backend);
	let mispredicts = sides
		.gained(&["branch miss rate:u", "branch miss rate"], moved::MISS_RATE)
		.or_else(|| sides.grew(&["branch MPKI:u", "branch MPKI"], moved::MPKI))
		.or_else(|| sides.gained(&["topdown branch mispredicts"], moved::TOPDOWN));
	let speculation = mispredicts
		.is_none()
		.then(|| sides.gained(&["topdown bad speculation"], moved::TOPDOWN))
		.flatten();
	add("branch mispredicts", mispredicts);
	add("bad speculation", speculation);
	add(
		"frontend-bound",
		sides
			.grew(&["L1i MPKI:u", "L1i MPKI", "iTLB MPKI:u"], moved::MPKI)
			.or_else(|| sides.gained(&["topdown fetch latency", "topdown frontend bound"], moved::TOPDOWN)),
	);
	// The kernel's share last: it moves with the faults a larger input takes, beside the misses
	// that explain more.
	add(
		"kernel",
		sides.gained(&["kernel instruction share", "kernel time share"], moved::KERNEL),
	);
	// The same work, branches, and misses a little slower, the frontend busier where the PMU says:
	// code that moved to other addresses, as an unrelated change shifts it.
	if let Some(change) = timed
		&& causes.is_empty()
		&& (moved::ALIGNMENT.0..=moved::ALIGNMENT.1).contains(&change)
		&& ["instructions:u", "branches:u"]
			.iter()
			.all(|name| sides.held(name, (moved::SAME, 0.0)))
		// The same misses, which says nothing unless both sides counted some.
		&& {
			let misses: Vec<String> = NATIVE
				.iter()
				.map(|(_, name)| format!("{name}:u"))
				.filter(|name| sides.get(name).is_some())
				.collect();
			!misses.is_empty() && misses.iter().all(|name| sides.held(name, moved::SAME_MPKI))
		} {
		let frontend = sides.get("topdown frontend bound");
		if frontend
			.as_ref()
			.is_none_or(|frontend| frontend.beyond() && frontend.delta() > 0.0)
		{
			let evidence = frontend.map_or(String::new(), |frontend| format!("; {}", frontend.cited()));
			causes.push(format!(
				"likely code alignment (same instructions, branches, and misses{evidence})"
			));
		}
	}
	(!causes.is_empty()).then(|| causes.into_iter().take(2).collect::<Vec<_>>().join("; "))
}

#[cfg(test)]
mod tests {
	use super::*;

	fn sampled(name: &str, value: f64, spread: (f64, f64), unit: &'static str) -> Measure {
		Measure {
			spread: Some(spread),
			..Measure::new(name, value, unit)
		}
	}

	fn get<'a>(measures: &'a [Measure], name: &str) -> &'a Measure {
		measures
			.iter()
			.find(|measure| measure.name == name)
			.unwrap_or_else(|| panic!("no {name} in {measures:?}"))
	}

	#[test]
	fn derives_each_native_ratio_in_its_ring() {
		let measures = [
			Measure::new("instructions:u", 2000.0, "events"),
			Measure::new("instructions:k", 500.0, "events"),
			Measure::new("LLC-load-misses:u", 3.0, "events"),
			Measure::new("L1-dcache-load-misses:k", 5.0, "events"),
			Measure::new("branches:u", 400.0, "events"),
			Measure::new("branch-misses:u", 8.0, "events"),
			Measure::new("cycles:u", 1000.0, "events"),
			Measure::new("user time", 250.0, "ns"),
			Measure::new("system time", 750.0, "ns"),
			// A count per byte divides nothing.
			Measure::new("instructions:u/byte", 1.0, "events/byte"),
		];
		let derived = derive(&measures, true);
		let value = |name: &str| get(&derived, name).value;
		assert_eq!(value("LLC MPKI:u"), 1.5);
		assert_eq!(value("L1d MPKI:k"), 10.0);
		assert_eq!(value("branch MPKI:u"), 4.0);
		assert_eq!(value("branch miss rate:u"), 2.0);
		assert_eq!(value("kernel instruction share"), 20.0);
		assert_eq!(value("estimated clock:u"), 4.0);
		assert_eq!(get(&derived, "estimated clock:u").unit, "GHz");
		assert!(derived.iter().all(|measure| measure.spread.is_none()));
		// A count has no time to estimate a clock from, nor a run that counted reference cycles.
		assert!(derive(&measures, false).iter().all(|m| m.name != "estimated clock:u"));
		let referenced = [&measures[..], &[Measure::new("cycles/ref-cycles:u", 1.1, "ratio")]].concat();
		assert!(derive(&referenced, true).iter().all(|m| m.name != "estimated clock:u"));
		// Nothing to divide by, nothing derived.
		assert!(derive(&[Measure::new("instructions:u", 0.0, "events")], true).is_empty());
	}

	#[test]
	fn derives_simulated_ratios_exactly() {
		let measures = [
			Measure::new("instructions", 10_000.0, "instructions"),
			Measure::new("I1 misses", 1.0, "events"),
			Measure::new("D1 read misses", 20.0, "events"),
			Measure::new("D1 write misses", 10.0, "events"),
			Measure::new("LL instruction misses", 1.0, "events"),
			Measure::new("LL read misses", 2.0, "events"),
			Measure::new("LL write misses", 2.0, "events"),
			Measure::new("conditional branches", 900.0, "events"),
			Measure::new("conditional mispredicts", 9.0, "events"),
			Measure::new("indirect branches", 100.0, "events"),
			Measure::new("indirect mispredicts", 1.0, "events"),
		];
		let derived = derive(&measures, false);
		let value = |name: &str| get(&derived, name).value;
		assert_eq!(value("L1i MPKI"), 0.1);
		assert_eq!(value("L1d MPKI"), 3.0);
		assert_eq!(value("LL MPKI"), 0.5);
		assert_eq!(value("branch MPKI"), 1.0);
		assert_eq!(value("branch miss rate"), 1.0);
		assert_eq!(derived.len(), 5);
	}

	#[test]
	fn a_ratio_of_a_sampled_measure_is_sampled_with_the_widest_spread() {
		let measures = [
			sampled("instructions:u", 1000.0, (900.0, 1100.0), "events"),
			Measure::new("L1-dcache-load-misses:u", 10.0, "events"),
			sampled("instructions:k", 100.0, (0.0, 200.0), "events"),
		];
		let derived = derive(&measures, false);
		let mpki = get(&derived, "L1d MPKI:u");
		assert_eq!(mpki.value, 10.0);
		let (least, most) = mpki.spread.unwrap();
		assert!(
			(least - 10.0 / 1.1).abs() < 1e-9 && (most - 10.0 / 0.9).abs() < 1e-9,
			"{mpki:?}"
		);
		let share = get(&derived, "kernel instruction share");
		assert_eq!(share.spread, Some((0.0, 100.0 * 200.0 / 1100.0)));
		// A sampled denominator that may be zero bounds nothing.
		assert!(
			derive(
				&[
					sampled("instructions:u", 10.0, (0.0, 20.0), "events"),
					Measure::new("LLC-load-misses:u", 1.0, "events"),
				],
				false
			)
			.is_empty()
		);
	}

	#[test]
	fn kernel_time_share_spans_each_samples_own_pair() {
		// One sample the tick charged only to user time, another only to system time.
		let share = kernel_share([(100, 0), (0, 100), (0, 0)]).unwrap();
		assert_eq!(share.value, 50.0);
		assert_eq!(share.spread, Some((0.0, 100.0)));
		let share = kernel_share([(300, 100), (100, 100)]).unwrap();
		assert_eq!(share.value, 100.0 * 200.0 / 600.0);
		assert_eq!(share.spread, Some((25.0, 50.0)));
		assert!(kernel_share([(0, 0)]).is_none());
	}

	/// A side of a timed benchmark with each measure named.
	fn side(measures: &[(&str, f64, &'static str)]) -> Vec<Measure> {
		measures
			.iter()
			.map(|&(name, value, unit)| Measure::new(name, value, unit))
			.collect()
	}

	#[test]
	fn names_the_cause_its_measures_show() {
		let base = side(&[
			("instructions:u", 1000.0, "events"),
			("branches:u", 100.0, "events"),
			("LLC MPKI:u", 0.8, "MPKI"),
			("branch miss rate:u", 1.0, "%"),
			("cycles/ref-cycles:u", 1.2, "ratio"),
			("CPUs busy", 0.98, "CPUs"),
			("topdown frontend bound", 10.0, "%"),
		]);
		let with = |name: &str, value: f64| {
			let mut head = base.clone();
			head.iter_mut().find(|measure| measure.name == name).unwrap().value = value;
			head
		};
		assert_eq!(cause(&base, &base, Some(0.1)), None);
		assert_eq!(
			cause(&base, &with("LLC MPKI:u", 2.1), Some(0.1)).unwrap(),
			"memory-bound (LLC MPKI:u 0.800 → 2.100)"
		);
		// Under a quarter more, or a tenth of a miss, is no cause.
		assert_eq!(cause(&base, &with("LLC MPKI:u", 0.95), Some(0.1)), None);
		assert_eq!(
			cause(&base, &with("branch miss rate:u", 3.4), Some(0.1)).unwrap(),
			"branch mispredicts (branch miss rate:u 1.00% → 3.40%)"
		);
		assert_eq!(
			cause(&base, &with("cycles/ref-cycles:u", 1.1), Some(0.05)).unwrap(),
			"throttled (cycles/ref-cycles:u 1.200 → 1.100)"
		);
		assert_eq!(
			cause(&base, &with("CPUs busy", 0.7), Some(0.3)).unwrap(),
			"off-CPU (CPUs busy 0.980 CPUs → 0.700 CPUs)"
		);
		assert_eq!(
			cause(&base, &with("instructions:u", 1200.0), Some(0.2)).unwrap(),
			"more instructions (instructions:u 1000 → 1200)"
		);
		// A trace names the wait the time off the CPU went to, the narrowest that holds most of it.
		let traced = [
			&base[..],
			&side(&[
				("blocked time", 1000.0, "ns"),
				("runnable time", 0.0, "ns"),
				("futex wait time", 800.0, "ns"),
				("lock wait time", 10.0, "ns"),
			]),
		]
		.concat();
		let waited = |waits: [f64; 4]| {
			let mut head = traced.clone();
			head.iter_mut().find(|m| m.name == "CPUs busy").unwrap().value = 0.7;
			for (name, value) in ["blocked time", "runnable time", "futex wait time", "lock wait time"]
				.into_iter()
				.zip(waits)
			{
				head.iter_mut().find(|m| m.name == name).unwrap().value = value;
			}
			cause(&traced, &head, Some(0.3)).unwrap()
		};
		assert_eq!(
			waited([3000.0, 0.0, 2800.0, 1900.0]),
			"lock contention (CPUs busy 0.980 CPUs → 0.700 CPUs, lock wait time 10 ns → 1900 ns)"
		);
		// Workers parked for work wait on a futex, but take no lock.
		assert_eq!(
			waited([3000.0, 0.0, 2800.0, 10.0]),
			"futex wait (CPUs busy 0.980 CPUs → 0.700 CPUs, futex wait time 800 ns → 2800 ns)"
		);
		assert_eq!(
			waited([3000.0, 0.0, 800.0, 10.0]),
			"blocked (CPUs busy 0.980 CPUs → 0.700 CPUs, blocked time 1000 ns → 3000 ns)"
		);
		assert_eq!(
			waited([1200.0, 1500.0, 800.0, 10.0]),
			"runnable (CPUs busy 0.980 CPUs → 0.700 CPUs, runnable time 0 ns → 1500 ns)"
		);
		// Waits that held name nothing narrower.
		assert_eq!(
			waited([1000.0, 0.0, 800.0, 10.0]),
			"off-CPU (CPUs busy 0.980 CPUs → 0.700 CPUs)"
		);
		// A count's headline is its instructions, which explain nothing more.
		assert_eq!(cause(&base, &with("instructions:u", 1200.0), None), None);
		// Two causes at most, the likelier first.
		let mut head = with("LLC MPKI:u", 2.1);
		head.iter_mut().find(|m| m.name == "cycles/ref-cycles:u").unwrap().value = 1.0;
		head.iter_mut().find(|m| m.name == "branch miss rate:u").unwrap().value = 3.0;
		assert_eq!(
			cause(&base, &head, Some(0.2)).unwrap(),
			"throttled (cycles/ref-cycles:u 1.200 → 1.000); memory-bound (LLC MPKI:u 0.800 → 2.100)"
		);
	}

	#[test]
	fn names_a_likely_code_alignment_shift() {
		let base = side(&[
			("instructions:u", 1000.0, "events"),
			("branches:u", 100.0, "events"),
			("L1d MPKI:u", 2.0, "MPKI"),
			("topdown frontend bound", 10.0, "%"),
		]);
		let mut head = base.clone();
		head[3].value = 12.0;
		assert_eq!(
			cause(&base, &head, Some(0.02)).unwrap(),
			"likely code alignment (same instructions, branches, and misses; topdown frontend bound 10.00% → 12.00%)"
		);
		// Past a few percent, or with more work or misses, or a frontend no busier, it is not one.
		assert_eq!(cause(&base, &head, Some(0.1)), None);
		assert_eq!(cause(&base, &base, Some(0.02)), None);
		let mut more = head.clone();
		more[0].value = 1007.0;
		assert_eq!(cause(&base, &more, Some(0.02)), None);
		let mut missed = head.clone();
		missed[2].value = 2.3;
		assert_eq!(cause(&base, &missed, Some(0.02)), None);
		// A PMU without the breakdown says only the counts held.
		assert_eq!(
			cause(&base[..3], &head[..3], Some(0.02)).unwrap(),
			"likely code alignment (same instructions, branches, and misses)"
		);
		// Nor without a miss counted on both sides to call the same.
		let unmissed = |side: &[Measure]| [side[0].clone(), side[1].clone()];
		assert_eq!(cause(&unmissed(&base), &unmissed(&head), Some(0.02)), None);
		// A count has no time to shift.
		assert_eq!(cause(&base, &head, None), None);
	}

	#[test]
	fn an_estimated_clock_alone_names_no_throttle() {
		let base = [Measure::new("estimated clock:u", 4.0, "GHz")];
		let head = [Measure::new("estimated clock:u", 3.0, "GHz")];
		assert_eq!(cause(&base, &head, Some(0.2)), None);
	}

	#[test]
	fn cpus_busy_spans_its_samples() {
		let busy = busy([(900, 1000), (500, 1000), (0, 0)]).unwrap();
		assert_eq!(busy.value, 0.7);
		assert_eq!(busy.spread, Some((0.5, 0.9)));
		assert!(super::busy([(1, 0)]).is_none());
	}

	#[test]
	fn a_sampled_measure_counts_only_past_its_spread() {
		let base = vec![sampled("LLC MPKI:u", 1.0, (0.5, 2.0), "MPKI")];
		let within = vec![sampled("LLC MPKI:u", 1.9, (1.5, 2.5), "MPKI")];
		let beyond = vec![sampled("LLC MPKI:u", 3.0, (2.5, 3.5), "MPKI")];
		assert_eq!(cause(&base, &within, Some(0.1)), None);
		assert!(cause(&base, &beyond, Some(0.1)).unwrap().starts_with("memory-bound"));
		assert!(cause(&base, &beyond, None).unwrap().starts_with("memory-bound"));
		// A side counted once, as before a benchmark gained a thread, is its one value.
		let once = vec![Measure::new("LLC MPKI:u", 1.0, "MPKI")];
		let wide = vec![sampled("LLC MPKI:u", 2.0, (0.8, 3.0), "MPKI")];
		assert_eq!(cause(&once, &wide, None), None);
		assert!(cause(&once, &beyond, None).unwrap().starts_with("memory-bound"));
	}

	#[test]
	fn a_parent_share_names_only_its_broad_category() {
		let side = |backend, memory, bad, mispredicts| {
			vec![
				Measure::new("topdown backend bound", backend, "%"),
				Measure::new("topdown memory bound", memory, "%"),
				Measure::new("topdown bad speculation", bad, "%"),
				Measure::new("topdown branch mispredicts", mispredicts, "%"),
			]
		};
		let base = side(30.0, 20.0, 5.0, 4.0);
		// Core-bound arithmetic and machine clears, the children that would be specific held.
		assert_eq!(
			cause(&base, &side(40.0, 20.0, 10.0, 4.0), None).unwrap(),
			"backend-bound (topdown backend bound 30.00% → 40.00%); \
			 bad speculation (topdown bad speculation 5.00% → 10.00%)"
		);
		assert_eq!(
			cause(&base, &side(40.0, 30.0, 10.0, 9.0), None).unwrap(),
			"memory-bound (topdown memory bound 20.00% → 30.00%); \
			 branch mispredicts (topdown branch mispredicts 4.00% → 9.00%)"
		);
	}
}
