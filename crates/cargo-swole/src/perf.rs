//! Linux perf counters: what this host lets swole count, probed once before anything builds, and
//! the counters it opens on each job before the job's target runs, which the harness starts and
//! stops around the benchmark's calls and reads.
//!
//! swole opens every counter itself, in the job's process between `fork` and `exec`, with the
//! privileges it holds, such as a `CAP_PERFMON` it was granted, which `exec`ing an ordinary
//! benchmark target drops: the counters stay open across `exec`, and the target only starts,
//! stops, and reads them. The probe opens the same counters with the same privileges, so what it
//! finds is what each job gets.

use std::fmt::Write as _;
use std::path::Path;

/// The variable that tells a harness target how many counters swole opened on it.
pub const COUNTERS: &str = "SWOLE_COUNTERS";

/// The variable that has a harness target call its benchmark this many times, each call counted.
pub const ITERATIONS: &str = "SWOLE_ITERATIONS";

/// The variable that tells a harness target every thread inherits the counters swole opened.
pub const INHERIT: &str = "SWOLE_INHERIT";

/// What a harness target prints before each counter's value, in the order swole opened them, and
/// the benchmark's name.
const COUNTED: &str = "swole counted ";

/// What a harness target prints before what the process used over the benchmark's calls.
pub const RUSAGE: &str = "swole rusage ";

/// What a harness target prints before the name of a benchmark beside which it had another thread.
pub const THREADED: &str = "swole threaded ";

/// What a harness target prints before the name of a benchmark beside which it had no other
/// thread; one too old to watch prints neither.
const ALONE: &str = "swole alone ";

/// What a harness target counting over iterations prints before how many calls its count covers.
const CALLS: &str = "swole calls ";

/// What a harness target prints before the loaded libc of the benchmark process.
pub const LIBC: &str = "swole libc ";

/// What a harness target prints, alone on a line, when it counts with counters every thread
/// inherits as [`INHERIT`] asks; one too old to prints nothing, and would fail asked.
pub const INHERITS: &str = "swole inherits";

/// Where the kernel lists its PMUs.
pub const PMUS: &str = "/sys/bus/event_source/devices";

/// The core PMU of a hybrid Intel host's performance cores, which a `bench` times on.
pub const PERFORMANCE: &str = "cpu_core";

/// The most counters of one kind the probe tries to fit in a group, more than any PMU holds.
const MOST_SLOTS: usize = 32;

/// One counter swole can open: an event, counted in user space or the kernel alone.
#[derive(Clone, Debug, PartialEq)]
pub struct Event {
	/// Its name as perf spells it, with `:u` for user space or `:k` for the kernel, such as
	/// `instructions:u`; a software event's name stands alone.
	pub name: String,
	/// Its perf type.
	kind: u32,
	config: u64,
	config1: u64,
	/// Whether a hardware event counts only the kernel, rather than only user space, or a
	/// software one counts the kernel as well.
	kernel: bool,
	/// Whether it takes one of the PMU's counters, as every event but the kernel's software ones does.
	hardware: bool,
	/// What each count it reads stands for: AMD's dispatch slots are its cycles times how many ops
	/// the core dispatches a cycle.
	scale: u64,
}

impl Event {
	/// The `perf_event_attr` that opens it, stopped, counting only the thread that opens it,
	/// since the kernel lets no process read a counter its threads inherit in user space, or with
	/// `every_thread`, every thread it starts too.
	fn attr(&self, every_thread: bool) -> Attr {
		Attr {
			kind: self.kind,
			size: size_of::<Attr>() as u32,
			config: self.config,
			read_format: PERF_FORMAT_TOTAL_TIME_ENABLED | PERF_FORMAT_TOTAL_TIME_RUNNING,
			flags: DISABLED
				| if every_thread { INHERIT_THREADS } else { 0 }
				| EXCLUDE_HV | match (self.hardware, self.kernel) {
				(_, false) => EXCLUDE_KERNEL,
				(true, true) => EXCLUDE_USER,
				(false, true) => 0,
			},
			config1: self.config1,
			..Attr::default()
		}
	}

	/// What it counts in, as Kixel Quest names a unit.
	pub fn unit(&self) -> &'static str {
		"events"
	}
}

/// The counters one process opens: each group's leader then its siblings, which the PMU counts
/// together or not at all, so none is multiplexed against another of its group.
#[derive(Clone, Debug, PartialEq)]
pub struct Pass {
	pub groups: Vec<Vec<Event>>,
	/// Whether the harness reads every counter with `read()`, as Intel's top-down metrics need,
	/// rather than its hardware ones with `rdpmc`, which reads them as fractions of `slots`.
	pub read: bool,
}

impl Pass {
	/// Its events in the order a job opens them, which is the order of their descriptors.
	pub fn events(&self) -> impl Iterator<Item = &Event> {
		self.groups.iter().flatten()
	}

	/// How many counters it opens.
	pub fn len(&self) -> usize {
		self.events().count()
	}

	/// How many of them the harness reads in user space: the hardware counters, which it opens
	/// before its software ones, unless it reads them all with `read()`.
	pub fn hardware(&self) -> usize {
		if self.read {
			return 0;
		}
		self.events().filter(|event| event.hardware).count()
	}
}

/// The core PMU whose counters count every job, and the CPUs the jobs run on.
#[derive(Clone, Debug)]
pub struct Pmu {
	/// Its name, such as `cpu_atom`, or `cpu` on a host whose CPUs all share one.
	pub name: String,
	/// The perf type a generic hardware event names it by in its config's upper half, 0 on a host
	/// with one PMU.
	extended: u32,
	/// Its directory under [`PMUS`], when the kernel lists one, from which it exports its own events.
	dir: Option<std::path::PathBuf>,
	/// The CPUs it counts on that swole may run on, which every job is pinned to.
	pub cpus: Vec<usize>,
}

/// What this host lets swole count, which the native fingerprint holds.
#[derive(Clone)]
pub struct Host {
	/// `kernel.perf_event_paranoid`, when the kernel has one.
	pub paranoid: Option<i32>,
	/// Whether swole holds `CAP_PERFMON`, or `CAP_SYS_ADMIN`, which also grants it.
	pub perfmon: bool,
	/// The PMUs the kernel lists.
	pub pmus: Vec<String>,
	/// The counters swole can open, or why it can open none.
	pub perf: Result<Perf, String>,
}

/// The counters this host lets swole open on a job.
#[derive(Clone)]
pub struct Perf {
	pub pmu: Pmu,
	/// Whether it counts the kernel as well as user space, as tier L1 lets bench.
	pub kernel: bool,
	/// How many general-purpose counters the PMU schedules at once, when branch counters can probe it.
	pub slots: Option<usize>,
	/// Every event it counts, in the passes that count them all: hardware ones packed into groups
	/// that each fit the PMU at once, and software ones beside the first.
	pub passes: Vec<Pass>,
	/// The events this PMU, or at tier L2 the kernel, does not count.
	pub absent: Vec<String>,
}

impl Host {
	/// Probes this host's counters for `mode`, quick's or bench's, once, as each job will open
	/// them: on a thread pinned to the PMU's CPUs, with swole's own privileges.
	pub fn probe(mode: crate::Mode) -> Result<Self, String> {
		let pmus = match std::fs::read_dir(PMUS) {
			Ok(entries) => {
				let mut names = entries
					.map(|entry| entry.map(|entry| entry.file_name().to_string_lossy().into_owned()))
					.collect::<Result<Vec<_>, _>>()
					.map_err(|e| format!("cannot list {PMUS}: {e}"))?;
				names.sort();
				names
			}
			Err(_) => Vec::new(),
		};
		let paranoid = std::fs::read_to_string("/proc/sys/kernel/perf_event_paranoid")
			.ok()
			.map(|text| {
				text.trim()
					.parse()
					.map_err(|_| format!("kernel.perf_event_paranoid is {text:?}, not a number"))
			})
			.transpose()?;
		let perfmon = perfmon()?;
		let perf = Perf::probe(paranoid, perfmon, mode == crate::Mode::Quick);
		Ok(Self {
			paranoid,
			perfmon,
			pmus,
			perf,
		})
	}

	/// What the probe found, a line each, as the fingerprint hashes it and the run prints it.
	pub fn describe(&self) -> String {
		let mut out = String::new();
		let paranoid = self.paranoid.map_or("(none)".to_owned(), |level| level.to_string());
		writeln!(out, "kernel.perf_event_paranoid: {paranoid}").expect("a String takes every write");
		let perfmon = if self.perfmon { "yes" } else { "no" };
		writeln!(out, "CAP_PERFMON: {perfmon}").expect("a String takes every write");
		writeln!(out, "PMUs: {}", self.pmus.join(" ")).expect("a String takes every write");
		match &self.perf {
			Ok(perf) => {
				let tier = if self.perfmon || self.paranoid.is_some_and(|level| level <= 1) {
					"L1"
				} else {
					"L2"
				};
				let rings = if perf.kernel {
					"user space and the kernel"
				} else {
					"user space"
				};
				writeln!(out, "perf: tier {tier}, counting {rings}").expect("a String takes every write");
				match perf.slots {
					Some(slots) => writeln!(
						out,
						"PMU: {} on CPUs {}, {slots} general-purpose counters",
						perf.pmu.name,
						list(&perf.pmu.cpus),
					),
					None => writeln!(out, "PMU: {} on CPUs {}", perf.pmu.name, list(&perf.pmu.cpus)),
				}
				.expect("a String takes every write");
				for (i, pass) in perf.passes.iter().enumerate() {
					let names: Vec<&str> = pass.events().map(|event| event.name.as_str()).collect();
					// A pass the harness reads with `read()` alone, which changes what its counts hold.
					let read = if pass.read { " (read)" } else { "" };
					writeln!(out, "pass {}{read}: {}", i + 1, names.join(" ")).expect("a String takes every write");
				}
				if !perf.absent.is_empty() {
					writeln!(out, "absent: {}", perf.absent.join(" ")).expect("a String takes every write");
				}
			}
			Err(why) => writeln!(out, "perf: none, {why}").expect("a String takes every write"),
		}
		out
	}
}

impl Perf {
	/// The counters this host lets swole open, or why none: for a `quick` count, only [`QUICK`]'s.
	#[cfg(target_os = "linux")]
	fn probe(paranoid: Option<i32>, perfmon: bool, quick: bool) -> Result<Self, String> {
		let kernel = match paranoid {
			_ if perfmon => true,
			Some(level) if level <= 1 => true,
			Some(level) if level <= 2 => false,
			Some(level) => {
				return Err(format!(
					"kernel.perf_event_paranoid is {level}, above 2, and swole lacks CAP_PERFMON: grant it as the README's \
					 Privileges says, or lower the setting"
				));
			}
			None => return Err("this kernel has no perf events".to_owned()),
		};
		let pmu = Pmu::find(quick)?;
		let cpus = pmu.cpus.clone();
		// Pinned as a job is, since a PMU of a hybrid host counts only on its own CPUs.
		std::thread::scope(|scope| {
			scope
				.spawn(|| {
					pin(&cpus).map_err(|e| format!("cannot pin the perf probe to CPUs {}: {e}", list(&cpus)))?;
					Self::fit(pmu, kernel, quick)
				})
				.join()
				.expect("the perf probe never panics")
		})
	}

	#[cfg(not(target_os = "linux"))]
	fn probe(_: Option<i32>, _: bool, _: bool) -> Result<Self, String> {
		Err("perf counters are Linux's".to_owned())
	}

	/// Which events `pmu` counts on this thread, every one or `quick`'s alone, and how they pack
	/// into groups it schedules at once.
	#[cfg(target_os = "linux")]
	fn fit(pmu: Pmu, kernel: bool, quick: bool) -> Result<Self, String> {
		let instructions = generic(&pmu, "instructions", PERF_TYPE_HARDWARE, 1, false);
		// The one counter every tier needs: its failure is the host's, not one event's.
		trial(std::slice::from_ref(&instructions)).map_err(|err| refused(&err))?;
		readable(&instructions, &pmu)?;
		let slots = (1..=MOST_SLOTS)
			.take_while(|&n| {
				let branches = vec![generic(&pmu, "branches", PERF_TYPE_HARDWARE, 4, false); n];
				matches!(trial(&branches), Ok(true))
			})
			.last();
		let rings: &[bool] = if kernel && !quick { &[false, true] } else { &[false] };
		let mut absent = Vec::new();
		// Each generic event the PMU counts, in each ring it counts it in, in `GENERIC`'s order.
		let mut opened: Vec<Event> = Vec::new();
		for (name, kind, config) in GENERIC {
			if quick && !QUICK.contains(&name) {
				continue;
			}
			for &ring in rings {
				let event = generic(&pmu, name, kind, config, ring);
				match trial(std::slice::from_ref(&event)) {
					Ok(true) => opened.push(event),
					Ok(false) => return Err(format!("PMU {} cannot schedule {} alone", pmu.name, event.name)),
					Err(err) if unsupported(&err) => {
						if !ring {
							absent.push(name.to_owned());
						}
						break;
					}
					Err(err) => return Err(format!("cannot open a perf counter of {}: {err}", event.name)),
				}
			}
		}
		// A ring's clock events count in one group, so instructions a cycle and cycles a reference
		// cycle each divide counts of one window, then every other event alone.
		let mut units: Vec<Vec<Event>> = Vec::new();
		for ring in ["u", "k"] {
			let clock: Vec<Event> = opened
				.iter()
				.filter(|event| CLOCK.iter().any(|name| event.name == format!("{name}:{ring}")))
				.cloned()
				.collect();
			if clock.is_empty() {
				continue;
			}
			match trial(&clock) {
				Ok(true) => units.push(clock),
				// Counted apart, they derive no ratio.
				_ => units.extend(clock.into_iter().map(|event| vec![event])),
			}
		}
		units.extend(
			opened
				.into_iter()
				.filter(|event| !CLOCK.iter().any(|name| event.name.starts_with(&format!("{name}:"))))
				.map(|event| vec![event]),
		);
		// Intel's top-down metrics, which `rdpmc` reads as fractions of `slots`, take a pass the
		// harness reads with `read()`; a PMU that counts its breakdown as events packs it as any.
		let mut metrics = None;
		// `quick` counts no breakdown, so it reads none of the PMU's event formats either.
		let breakdown = if quick { None } else { topdown(&pmu)? };
		match breakdown {
			None => {}
			Some(Topdown::Metrics(group)) => match trial(&group) {
				Ok(true) => {
					metrics = Some(Pass {
						groups: vec![group],
						read: true,
					});
				}
				Ok(false) => absent.push("topdown".to_owned()),
				Err(err) if unsupported(&err) => absent.push("topdown".to_owned()),
				Err(err) => return Err(format!("cannot open PMU {}'s top-down metrics: {err}", pmu.name)),
			},
			Some(Topdown::Events(breakdown)) => {
				for (level, mut unit) in breakdown {
					loop {
						let smt = unit.last().is_some_and(|event| event.name == SMT);
						match trial(&unit) {
							Ok(true) => units.push(unit),
							// A PMU with a counter fewer, such as one the NMI watchdog holds, counts
							// the breakdown without the SMT sibling's share, whether the group fails to
							// schedule or the kernel refuses it at open.
							Ok(false) if smt => {
								unit.pop();
								continue;
							}
							Err(err) if smt && unsupported(&err) => {
								unit.pop();
								continue;
							}
							Ok(false) => absent.push(level.to_owned()),
							Err(err) if unsupported(&err) => absent.push(level.to_owned()),
							Err(err) => {
								return Err(format!("cannot open PMU {}'s {level} counters: {err}", pmu.name));
							}
						}
						break;
					}
				}
				absent.dedup();
			}
		}
		// Each unit in the first group it fits beside, so no PMU counter idles while a later pass
		// needs one.
		let mut groups: Vec<Vec<Event>> = Vec::new();
		'units: for unit in units {
			for group in &mut groups {
				let mut wider = group.clone();
				wider.extend(unit.iter().cloned());
				if matches!(trial(&wider), Ok(true)) {
					*group = wider;
					continue 'units;
				}
			}
			groups.push(unit);
		}
		let mut passes: Vec<Pass> = groups
			.into_iter()
			.map(|group| Pass {
				groups: vec![group],
				read: false,
			})
			.collect();
		for (name, config, scheduler) in SOFTWARE.into_iter().filter(|_| !quick) {
			// The scheduler counts its events in the kernel, so they need its ring, as only tier L1
			// opens; a page fault counts in the ring that took it.
			if scheduler && !kernel {
				absent.push(name.to_owned());
				continue;
			}
			let event = Event {
				name: name.to_owned(),
				kind: PERF_TYPE_SOFTWARE,
				config,
				config1: 0,
				kernel,
				hardware: false,
				scale: 1,
			};
			trial(std::slice::from_ref(&event))
				.map_err(|err| format!("cannot open a perf counter of {name}: {err}"))?;
			passes[0].groups.push(vec![event]);
		}
		passes.extend(metrics);
		Ok(Self {
			pmu,
			kernel: kernel && !quick,
			slots,
			passes,
			absent,
		})
	}
}

/// The events a quick count reads: the core ones, in user space alone.
const QUICK: [&str; 6] = [
	"instructions",
	"cycles",
	"branches",
	"branch-misses",
	"L1-dcache-load-misses",
	"LLC-load-misses",
];

/// The generic hardware events: each name, perf type, and config.
const GENERIC: [(&str, u32, u64); 12] = [
	("instructions", PERF_TYPE_HARDWARE, 1),
	("cycles", PERF_TYPE_HARDWARE, 0),
	// Cycles at the core's base frequency, whatever it runs at: Intel's fixed reference counter,
	// or AMD's `ls_not_halted_p0_cyc` from Zen 4 on.
	("ref-cycles", PERF_TYPE_HARDWARE, 9),
	("branches", PERF_TYPE_HARDWARE, 4),
	("branch-misses", PERF_TYPE_HARDWARE, 5),
	("stalled-cycles-frontend", PERF_TYPE_HARDWARE, 7),
	("stalled-cycles-backend", PERF_TYPE_HARDWARE, 8),
	// A cache event's config is the cache, then the operation (read, 0) a byte up, then the
	// result (miss, 1) two bytes up.
	("L1-dcache-load-misses", PERF_TYPE_HW_CACHE, 1 << 16),
	("L1-icache-load-misses", PERF_TYPE_HW_CACHE, 1 | 1 << 16),
	("LLC-load-misses", PERF_TYPE_HW_CACHE, 2 | 1 << 16),
	("dTLB-load-misses", PERF_TYPE_HW_CACHE, 3 | 1 << 16),
	("iTLB-load-misses", PERF_TYPE_HW_CACHE, 4 | 1 << 16),
];

/// The generic events each ring counts in one group, so the ratios between them hold.
const CLOCK: [&str; 3] = ["instructions", "cycles", "ref-cycles"];

/// The software events: each name, config, and whether the scheduler counts it.
/// The calling thread's time on a CPU, task-clock, is not among them: read in user space, the
/// counters cannot keep their own reads out of it.
const SOFTWARE: [(&str, u64, bool); 3] = [
	("page-faults", 2, false),
	("context-switches", 3, true),
	("cpu-migrations", 4, true),
];

/// The top-down breakdown, as a PMU exports it among its own events: the first level, then the
/// second level's share of each.
const TOPDOWN: [&str; 8] = [
	"topdown-retiring",
	"topdown-bad-spec",
	"topdown-fe-bound",
	"topdown-be-bound",
	"topdown-heavy-ops",
	"topdown-br-mispredict",
	"topdown-fetch-lat",
	"topdown-mem-bound",
];

/// What `slots` is named, the dispatch slots a breakdown divides.
pub const SLOTS: &str = "slots:u";

/// AMD's events of its breakdown's first level, after the slots, by perf's names.
pub const FRONTEND: &str = "de_no_dispatch_per_slot.no_ops_from_frontend:u";
pub const BACKEND: &str = "de_no_dispatch_per_slot.backend_stalls:u";
pub const DISPATCHED: &str = "de_src_op_disp.all:u";
pub const RETIRED: &str = "ex_ret_ops:u";
/// The slots the SMT sibling took, the first level's last event, which a PMU a counter short
/// leaves out.
pub const SMT: &str = "de_no_dispatch_per_slot.smt_contention:u";
/// AMD's events of its breakdown's second level, a pair each: ops retired from microcode and all
/// of them, cycles and those no op came from the frontend at all, mispredicted branches and
/// resyncs, and stalls on loads and on anything.
pub const MICROCODE: &str = "ex_ret_ucode_ops:u";
/// `ex_ret_ops` again, counted beside the microcode's ops rather than the first level.
pub const RETIRED_BESIDE: &str = "ex_ret_ops,microcode:u";
pub const CYCLES: &str = "ls_not_halted_cyc:u";
pub const FRONTEND_IDLE: &str = "de_no_dispatch_per_slot.no_ops_from_frontend,cmask:u";
pub const MISPREDICTS: &str = "ex_ret_brn_misp:u";
pub const RESYNCS: &str = "resyncs:u";
pub const LOAD_STALLS: &str = "ex_no_retire.load_not_complete:u";
pub const STALLS: &str = "ex_no_retire.not_complete:u";

/// The top-down breakdown a PMU counts.
enum Topdown {
	/// Intel's metrics, a group led by `slots`.
	Metrics(Vec<Event>),
	/// Events the PMU counts as any other, a group of each level, named for what it is.
	Events(Vec<(&'static str, Vec<Event>)>),
}

/// A generic event of `pmu`, in user space or, when `kernel`, the kernel.
fn generic(pmu: &Pmu, name: &str, kind: u32, config: u64, kernel: bool) -> Event {
	Event {
		name: format!("{name}:{}", if kernel { "k" } else { "u" }),
		kind,
		config: u64::from(pmu.extended) << PERF_PMU_TYPE_SHIFT | config,
		config1: 0,
		kernel,
		hardware: true,
		scale: 1,
	}
}

/// The top-down breakdown `pmu` counts in user space: the events it exports, led by its `slots`
/// event where it has one, as a PMU that computes them from it requires, or on AMD's Zen 4 and 5,
/// which export none, the events perf's own metrics divide.
fn topdown(pmu: &Pmu) -> Result<Option<Topdown>, String> {
	let Some(dir) = &pmu.dir else {
		return Ok(None);
	};
	let kind = read(&dir.join("type"))?
		.parse()
		.map_err(|_| format!("{}/type is no perf type", dir.display()))?;
	let event = |name: &str, spec: &str, scale: u64| -> Result<Event, String> {
		let (config, config1) = sysfs(dir, spec).map_err(|err| format!("{name}'s {spec}: {err}"))?;
		Ok(Event {
			name: name.to_owned(),
			kind,
			config,
			config1,
			kernel: false,
			hardware: true,
			scale,
		})
	};
	if pmu.name == "cpu"
		&& let Some(zen) = Zen::of(&std::fs::read_to_string("/proc/cpuinfo").unwrap_or_default())
	{
		let width = zen.width();
		let first = vec![
			event(SLOTS, "event=0x76", width)?,
			event(FRONTEND, "event=0x1a0,umask=0x01", 1)?,
			event(BACKEND, "event=0x1a0,umask=0x1e", 1)?,
			event(DISPATCHED, "event=0xaa,umask=0x07", 1)?,
			event(RETIRED, "event=0xc1", 1)?,
			event(SMT, "event=0x1a0,umask=0x60", 1)?,
		];
		let second = [
			vec![
				event(MICROCODE, "event=0x1c1", 1)?,
				event(RETIRED_BESIDE, "event=0xc1", 1)?,
			],
			vec![
				event(CYCLES, "event=0x76", 1)?,
				event(FRONTEND_IDLE, &format!("event=0x1a0,umask=0x01,cmask={width}"), 1)?,
			],
			vec![event(MISPREDICTS, "event=0xc3", 1)?, event(RESYNCS, zen.resyncs(), 1)?],
			vec![
				event(LOAD_STALLS, "event=0xd6,umask=0xa2", 1)?,
				event(STALLS, "event=0xd6,umask=0x02", 1)?,
			],
		];
		let mut levels = vec![("topdown", first)];
		levels.extend(second.into_iter().map(|pair| ("topdown level 2", pair)));
		return Ok(Some(Topdown::Events(levels)));
	}
	let mut events = Vec::new();
	for name in std::iter::once("slots").chain(TOPDOWN) {
		let path = dir.join("events").join(name);
		let Ok(spec) = std::fs::read_to_string(&path) else {
			continue;
		};
		// The kernel pads some specs with NULs.
		let spec = spec.trim_matches(|c: char| c == '\0' || c.is_whitespace());
		events.push(event(&format!("{name}:u"), spec, 1).map_err(|err| format!("{}: {err}", path.display()))?);
	}
	// `slots` alone is no breakdown.
	if !events.iter().any(|event| event.name != SLOTS) {
		return Ok(None);
	}
	Ok(Some(if events[0].name == SLOTS {
		Topdown::Metrics(events)
	} else {
		Topdown::Events(vec![("topdown", events)])
	}))
}

/// An AMD core whose breakdown swole knows, as perf's own event lists name each.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Zen {
	Four,
	Five,
}

impl Zen {
	/// The core `/proc/cpuinfo` describes, when it is one: Zen 4 is family 25 but Zen 3's models,
	/// and Zen 5 family 26 but Zen 6's.
	fn of(cpuinfo: &str) -> Option<Self> {
		let field = |key: &str| {
			cpuinfo.lines().find_map(|line| {
				let (name, value) = line.split_once(':')?;
				(name.trim() == key).then(|| value.trim())
			})
		};
		if field("vendor_id")? != "AuthenticAMD" {
			return None;
		}
		let family: u32 = field("cpu family")?.parse().ok()?;
		let model: u32 = field("model")?.parse().ok()?;
		match (family, model >> 4) {
			(25, 0 | 2..=5) => None,
			(25, _) => Some(Self::Four),
			(26, 0..=2 | 4 | 6 | 7) => Some(Self::Five),
			_ => None,
		}
	}

	/// How many ops it dispatches a cycle, which are its slots.
	fn width(self) -> u64 {
		match self {
			Self::Four => 6,
			Self::Five => 8,
		}
	}

	/// Its event of the pipeline restarts a resync takes.
	fn resyncs(self) -> &'static str {
		match self {
			Self::Four => "event=0x96",
			Self::Five => "event=0x9f,umask=0x01",
		}
	}
}

/// The config words a PMU's event spec, such as `event=0xc2,umask=0x02`, sets, by the bit ranges
/// the PMU's `format` directory gives each term, such as `config:0-7`.
fn sysfs(dir: &Path, spec: &str) -> Result<(u64, u64), String> {
	let mut words = [0u64; 2];
	for term in spec.split(',').map(str::trim).filter(|term| !term.is_empty()) {
		let (key, value) = term.split_once('=').unwrap_or((term, "1"));
		let value = match value.strip_prefix("0x") {
			Some(hex) => u64::from_str_radix(hex, 16),
			None => value.parse(),
		}
		.map_err(|_| format!("{term} sets no number"))?;
		let format = read(&dir.join("format").join(key))?;
		let (word, ranges) = format.split_once(':').ok_or(format!("format {key} is {format}"))?;
		let word = match word {
			"config" => &mut words[0],
			"config1" => &mut words[1],
			_ => return Err(format!("format {key} sets {word}, which swole does not")),
		};
		let mut rest = value;
		for range in ranges.split(',') {
			let bit = |n: &str| n.parse::<u32>().map_err(|_| format!("format {key} is {format}"));
			let (low, high) = match range.split_once('-') {
				Some((low, high)) => (bit(low)?, bit(high)?),
				None => (bit(range)?, bit(range)?),
			};
			let width = high - low + 1;
			let mask = if width == 64 { u64::MAX } else { (1 << width) - 1 };
			*word |= (rest & mask) << low;
			rest = rest.checked_shr(width).unwrap_or(0);
		}
	}
	Ok((words[0], words[1]))
}

/// Whether opening an event failed because the PMU lacks it, rather than for want of permission.
#[cfg(target_os = "linux")]
fn unsupported(err: &std::io::Error) -> bool {
	matches!(err.raw_os_error(), Some(ENOENT | EOPNOTSUPP | EINVAL))
}

/// Why the first counter failed to open, and what to change.
#[cfg(target_os = "linux")]
fn refused(err: &std::io::Error) -> String {
	format!(
		"cannot open a perf counter of user-space instructions ({err}): this host exposes no counters to a process, \
		 as a VM without its PMU passed through does"
	)
}

impl Pmu {
	/// The core PMU of the most CPUs swole may run on, or for a `bench`, which times on one
	/// core, the performance cores' when swole may run on one. On a host whose CPUs are of several
	/// kinds, such as Intel's P- and E-cores, each kind has its own PMU, which counts only on its CPUs.
	#[cfg(target_os = "linux")]
	fn find(quick: bool) -> Result<Self, String> {
		let allowed = allowed()?;
		let mut cores = Vec::new();
		let entries = std::fs::read_dir(PMUS).map_err(|e| format!("cannot list {PMUS}: {e}"))?;
		for entry in entries {
			let dir = entry.map_err(|e| format!("cannot list {PMUS}: {e}"))?.path();
			// Only a core PMU of a host with several lists the CPUs it counts on; an uncore one
			// names a `cpumask` instead.
			let Ok(list) = std::fs::read_to_string(dir.join("cpus")) else {
				continue;
			};
			let name = dir.file_name().expect("listed").to_string_lossy().into_owned();
			let extended = read(&dir.join("type"))?
				.parse()
				.map_err(|_| format!("{}/type is no perf type", dir.display()))?;
			let cpus: Vec<usize> = cpus(&list)
				.map_err(|err| format!("{}/cpus: {err}", dir.display()))?
				.into_iter()
				.filter(|cpu| allowed.contains(cpu))
				.collect();
			cores.push(Pmu {
				name,
				extended,
				dir: Some(dir),
				cpus,
			});
		}
		// The most CPUs, then the first name, so a host always picks the same one.
		cores.sort_by(|a, b| b.cpus.len().cmp(&a.cpus.len()).then(a.name.cmp(&b.name)));
		if !quick
			&& let Some(i) = cores
				.iter()
				.position(|pmu| pmu.name == PERFORMANCE && !pmu.cpus.is_empty())
		{
			let performance = cores.remove(i);
			cores.insert(0, performance);
		}
		let single = Path::new(PMUS).join("cpu");
		let pmu = match cores.len() {
			0 => Pmu {
				name: "cpu".to_owned(),
				extended: 0,
				dir: single.is_dir().then_some(single),
				cpus: allowed,
			},
			// A host with one names it by its CPU's generic events.
			1 => Pmu {
				extended: 0,
				..cores.remove(0)
			},
			_ => cores.remove(0),
		};
		if pmu.cpus.is_empty() {
			return Err(format!("swole may run on none of the CPUs PMU {} counts on", pmu.name));
		}
		Ok(pmu)
	}
}

/// Pins `command` to `cpus` just before it `exec`s.
pub fn pin_command(command: &mut std::process::Command, cpus: &[usize]) {
	#[cfg(target_os = "linux")]
	{
		use std::os::unix::process::CommandExt;
		let mask = mask(cpus);
		// SAFETY: between fork and exec it makes only one system call, which allocates nothing.
		unsafe {
			command.pre_exec(move || {
				if sched_setaffinity(0, mask.len() * 8, mask.as_ptr()) != 0 {
					return Err(std::io::Error::last_os_error());
				}
				Ok(())
			});
		}
	}
	#[cfg(not(target_os = "linux"))]
	let _ = (command, cpus);
}

/// How a job's counters launch.
#[derive(Clone, Copy, Debug)]
pub struct Launch {
	/// Whether the job runs at the addresses of its last run.
	pub fixed: bool,
	/// Whether every thread the job starts inherits its counters, which the harness then reads
	/// with `read()` alone, rather than only the thread that runs the benchmark counting.
	pub every_thread: bool,
}

/// Has `command` run on `cpus`, which the PMU counting `pass` must count on, and open `pass`'s
/// counters on itself just before it `exec`s, as `how` says, telling the harness how many and how
/// they count.
pub fn launch(command: &mut std::process::Command, cpus: &[usize], pass: &Pass, how: Launch) {
	let Launch { fixed, every_thread } = how;
	let hardware = pass.hardware();
	command.env(COUNTERS, format!("{hardware}+{}", pass.len() - hardware));
	if every_thread {
		command.env(INHERIT, "1");
	} else {
		command.env_remove(INHERIT);
	}
	#[cfg(target_os = "linux")]
	{
		use std::os::unix::process::CommandExt;
		let mask = mask(cpus);
		// Each event's attributes, and whether it leads its group, built before the fork.
		let attrs: Vec<(Attr, bool)> = pass
			.groups
			.iter()
			.flat_map(|group| {
				group
					.iter()
					.enumerate()
					.map(move |(i, event)| (event.attr(every_thread), i == 0))
			})
			.collect();
		// SAFETY: between fork and exec it makes only system calls, which allocate nothing.
		unsafe {
			command.pre_exec(move || {
				if sched_setaffinity(0, mask.len() * 8, mask.as_ptr()) != 0 {
					return Err(std::io::Error::last_os_error());
				}
				if fixed {
					let persona = personality(0xffff_ffff);
					if persona == -1 || personality(persona as u64 | ADDR_NO_RANDOMIZE) == -1 {
						return Err(std::io::Error::last_os_error());
					}
				}
				let mut leader = -1;
				for (attr, leads) in &attrs {
					// Not closed on exec, so the target holds them; each takes the lowest free
					// descriptor, so the target finds them in this order.
					let fd = open(attr, if *leads { -1 } else { leader }, 0)?;
					if *leads {
						leader = fd;
					}
				}
				Ok(())
			});
		}
	}
	#[cfg(not(target_os = "linux"))]
	let _ = (command, cpus, fixed);
}

/// What one benchmark's count read: its counters' values in the order of its pass, what the
/// process used over its calls, whether another thread was there, and the benchmark's loaded libc.
#[derive(Debug, PartialEq)]
pub struct Reading {
	pub name: String,
	pub values: Vec<u64>,
	/// How many calls the values cover, when the harness said, as one counting over iterations
	/// does.
	pub calls: Option<u64>,
	pub usage: Option<Usage>,
	pub threaded: bool,
	pub libc: Option<String>,
	/// Whether its harness can count with counters every thread inherits.
	pub inherits: bool,
}

/// What a harness target's output says each benchmark it counted with `counters` counters read.
pub fn readings(stdout: &str, counters: usize) -> Result<Vec<Reading>, String> {
	let threads = threads(stdout);
	let inherits = stdout.lines().any(|line| line == INHERITS);
	let mut usage = None;
	let mut calls = None;
	let mut libc = None;
	let mut readings = Vec::new();
	for line in stdout.lines() {
		if let Some(fields) = line.strip_prefix(RUSAGE) {
			usage = Some(Usage::parse(fields)?);
		} else if let Some(rest) = line.strip_prefix(LIBC) {
			libc = Some(rest.to_owned());
		} else if let Some(rest) = line.strip_prefix(CALLS) {
			let (n, name) = rest
				.split_once(' ')
				.and_then(|(n, name)| Some((n.parse::<u64>().ok().filter(|&n| n > 0)?, name)))
				.ok_or_else(|| format!("a harness target printed {CALLS}{rest}, which no harness prints"))?;
			calls = Some((name, n));
		} else if let Some(rest) = line.strip_prefix(COUNTED) {
			let malformed =
				|| format!("a harness target printed {COUNTED}{rest}, which no harness prints for {counters} counters");
			let mut fields = rest.splitn(counters + 1, ' ');
			let values = fields
				.by_ref()
				.take(counters)
				.map(|n| n.parse().map_err(|_| malformed()))
				.collect::<Result<Vec<u64>, String>>()?;
			let name = fields.next().filter(|name| !name.is_empty()).ok_or_else(malformed)?;
			if values.len() != counters {
				return Err(malformed());
			}
			// A harness that counts says whether another thread ran, unless a line the benchmark
			// printed without a newline swallowed it, which must not pass for none having run.
			let threaded = *threads.get(name).ok_or_else(|| {
				format!(
					"a harness target counted {name} but said neither {THREADED}nor {ALONE}of it: a line the benchmark \
					 printed without a newline ran into it"
				)
			})?;
			readings.push(Reading {
				name: name.to_owned(),
				values,
				// Only its own benchmark's record, which a benchmark's own line cannot pass for.
				calls: calls.take().filter(|&(of, _)| of == name).map(|(_, n)| n),
				usage: usage.take(),
				threaded,
				libc: libc.clone(),
				inherits,
			});
		}
	}
	// A threaded case's harness reads no counters of the calling thread alone, which swole would
	// discard, and says only that it was threaded.
	for (name, &threaded) in &threads {
		if threaded && !readings.iter().any(|reading| reading.name == *name) {
			readings.push(Reading {
				name: name.clone(),
				values: Vec::new(),
				calls: None,
				usage: None,
				threaded,
				libc: libc.clone(),
				inherits,
			});
		}
	}
	Ok(readings)
}

/// Each benchmark a harness target's output says whether it had another thread beside, and
/// what it says.
pub fn threads(stdout: &str) -> std::collections::BTreeMap<String, bool> {
	stdout
		.lines()
		.filter_map(|line| {
			(line.strip_prefix(THREADED).map(|name| (name, true)))
				.or_else(|| line.strip_prefix(ALONE).map(|name| (name, false)))
		})
		.map(|(name, threaded)| (name.to_owned(), threaded))
		.collect()
}

/// What a process used over a benchmark's calls, or a sample's, as `getrusage` accounts it.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Usage {
	pub user_ns: u64,
	pub system_ns: u64,
	/// The process's peak resident memory, in bytes.
	pub max_rss: u64,
	pub minor_faults: u64,
	pub major_faults: u64,
	pub voluntary_switches: u64,
	pub involuntary_switches: u64,
}

impl Usage {
	/// The usage a harness target prints after [`RUSAGE`].
	pub fn parse(fields: &str) -> Result<Self, String> {
		let values: Vec<u64> = fields
			.split(' ')
			.map(str::parse)
			.collect::<Result<_, _>>()
			.map_err(|_| format!("a harness target printed {RUSAGE}{fields}, which no harness prints"))?;
		let [
			user_ns,
			system_ns,
			max_rss,
			minor_faults,
			major_faults,
			voluntary_switches,
			involuntary_switches,
		] = values[..]
		else {
			return Err(format!(
				"a harness target printed {RUSAGE}{fields}, which no harness prints"
			));
		};
		Ok(Self {
			user_ns,
			system_ns,
			max_rss,
			minor_faults,
			major_faults,
			voluntary_switches,
			involuntary_switches,
		})
	}

	/// Adds what `other` used, keeping the larger peak.
	pub fn add(&mut self, other: &Self) {
		self.user_ns += other.user_ns;
		self.system_ns += other.system_ns;
		self.max_rss = self.max_rss.max(other.max_rss);
		self.minor_faults += other.minor_faults;
		self.major_faults += other.major_faults;
		self.voluntary_switches += other.voluntary_switches;
		self.involuntary_switches += other.involuntary_switches;
	}

	/// Each measure over `calls` calls, a call's share of each but the peak, which is the process's.
	pub fn measures(&self, calls: u64) -> Vec<crate::Measure> {
		let per = |value: u64| value as f64 / calls as f64;
		vec![
			crate::Measure::new("user time", per(self.user_ns), "ns"),
			crate::Measure::new("system time", per(self.system_ns), "ns"),
			crate::Measure::new("max RSS", self.max_rss as f64, "bytes"),
			crate::Measure::new("minor faults", per(self.minor_faults), "faults"),
			crate::Measure::new("major faults", per(self.major_faults), "faults"),
			crate::Measure::new("voluntary switches", per(self.voluntary_switches), "switches"),
			crate::Measure::new("involuntary switches", per(self.involuntary_switches), "switches"),
		]
	}
}

/// Each counter `passes` read, a call's share over each pass's calls, then what derives from
/// counters that ran in the same group, over the same window: instructions a cycle and cycles a
/// reference cycle in each ring, and the top-down breakdown.
pub fn measures(passes: &[Pass], values: &[(Vec<u64>, u64)]) -> Vec<crate::Measure> {
	// Each group's events by name, a call's share of each, its scale applied.
	let groups: Vec<Vec<(&str, f64)>> = passes
		.iter()
		.zip(values)
		.flat_map(|(pass, (values, calls))| {
			let mut values = values.iter();
			pass.groups.iter().map(move |group| {
				group
					.iter()
					.zip(values.by_ref())
					.map(|(event, &value)| (event.name.as_str(), (value * event.scale) as f64 / *calls as f64))
					.collect::<Vec<_>>()
			})
		})
		.collect();
	let mut out: Vec<crate::Measure> = passes
		.iter()
		.flat_map(Pass::events)
		.zip(groups.iter().flatten())
		.map(|(event, &(name, value))| crate::Measure::new(name, value, event.unit()))
		.collect();
	let together = Together(&groups);
	for ring in ["u", "k"] {
		let [instructions, cycles, reference] =
			["instructions", "cycles", "ref-cycles"].map(|name| format!("{name}:{ring}"));
		if let Some([instructions, cycles]) = together.find([instructions.as_str(), cycles.as_str()])
			&& cycles > 0.0
		{
			out.push(crate::Measure::new(
				&format!("IPC:{ring}"),
				instructions / cycles,
				"instructions/cycle",
			));
		}
		if let Some([cycles, reference]) = together.find([cycles.as_str(), reference.as_str()])
			&& reference > 0.0
		{
			out.push(crate::Measure::new(
				&format!("cycles/ref-cycles:{ring}"),
				cycles / reference,
				"ratio",
			));
		}
	}
	out.extend(breakdown(&together));
	out
}

/// Whether `name` is a ratio `measures` derives, which a count leaves out where its denominator
/// counted nothing, unlike a counter, which every count of a pass reads.
pub fn derived(name: &str) -> bool {
	["IPC:", "cycles/ref-cycles:", "topdown "]
		.iter()
		.any(|prefix| name.starts_with(prefix))
}

/// Counters by group, which find the values of events counted together.
struct Together<'a>(&'a [Vec<(&'a str, f64)>]);

impl Together<'_> {
	/// The values of `names`, in their order, from the first group that counted every one.
	fn find<const N: usize>(&self, names: [&str; N]) -> Option<[f64; N]> {
		self.0.iter().find_map(|group| {
			let mut found = [0.0; N];
			for (slot, name) in found.iter_mut().zip(names) {
				*slot = group.iter().find(|(event, _)| *event == name)?.1;
			}
			Some(found)
		})
	}
}

/// The top-down breakdown, each a percentage of the dispatch slots: the first level's retiring,
/// bad speculation, frontend and backend bound, and where the PMU counts it, the second level's
/// split of each. Intel's divide its `slots`, Intel's E-cores', which have none, their first
/// level's sum, and AMD's the slots its cycles hold, as perf's own metrics do. AMD's second level
/// divides each first-level share as a pair counted together splits it, with the pair's group
/// counted in a pass of its own.
fn breakdown(together: &Together) -> Vec<crate::Measure> {
	let mut out = Vec::new();
	// A share is a difference of counts or of shares at most, so one under a billionth is rounding.
	let percent = |name: &str, share: f64| {
		let share = if share < 1e-9 { 0.0 } else { share };
		crate::Measure::new(name, 100.0 * share, "%")
	};
	// A second-level share and what is left of its parent.
	let split = |out: &mut Vec<crate::Measure>, [part, rest]: [&str; 2], parent: f64, share: f64| {
		let parent = parent.max(0.0);
		let share = share.clamp(0.0, parent);
		out.push(percent(part, share));
		out.push(percent(rest, parent - share));
	};
	let names = |names: [&str; 4]| names.map(|name| format!("{name}:u"));
	let [retiring, bad, frontend, backend] = names([
		"topdown-retiring",
		"topdown-bad-spec",
		"topdown-fe-bound",
		"topdown-be-bound",
	]);
	let first = |shares: [f64; 4], out: &mut Vec<crate::Measure>| {
		for (name, share) in LEVEL_ONE.into_iter().zip(shares) {
			out.push(percent(name, share));
		}
	};
	if let Some([slots, r, b, f, e]) = together.find([
		SLOTS,
		retiring.as_str(),
		bad.as_str(),
		frontend.as_str(),
		backend.as_str(),
	]) {
		if slots <= 0.0 {
			return out;
		}
		let [r, b, f, e] = [r, b, f, e].map(|value| value / slots);
		first([r, b, f, e], &mut out);
		let [heavy, mispredict, fetch, memory] = names([
			"topdown-heavy-ops",
			"topdown-br-mispredict",
			"topdown-fetch-lat",
			"topdown-mem-bound",
		]);
		for (child, parent, pair) in [
			(&heavy, r, LEVEL_TWO[0]),
			(&mispredict, b, LEVEL_TWO[1]),
			(&fetch, f, LEVEL_TWO[2]),
			(&memory, e, LEVEL_TWO[3]),
		] {
			if let Some([slots, child]) = together.find([SLOTS, child.as_str()]) {
				split(&mut out, pair, parent, child / slots);
			}
		}
	} else if let Some(shares) = together.find([retiring.as_str(), bad.as_str(), frontend.as_str(), backend.as_str()]) {
		let total: f64 = shares.iter().sum();
		if total > 0.0 {
			first(shares.map(|value| value / total), &mut out);
		}
	} else if let Some([slots, f, e, dispatched, retired]) =
		together.find([SLOTS, FRONTEND, BACKEND, DISPATCHED, RETIRED])
	{
		if slots <= 0.0 {
			return out;
		}
		let [r, b, f, e] = [retired, dispatched - retired, f, e].map(|value| value / slots);
		first([r, b, f, e], &mut out);
		if let Some([slots, smt]) = together.find([SLOTS, SMT]) {
			out.push(percent("topdown SMT contention", smt / slots));
		}
		if let Some([microcode, retired]) = together.find([MICROCODE, RETIRED_BESIDE])
			&& retired > 0.0
		{
			split(&mut out, LEVEL_TWO[0], r, r * microcode / retired);
		}
		// Cycles no op came from the frontend at all, every slot of each, are its latency.
		if let Some([cycles, idle]) = together.find([CYCLES, FRONTEND_IDLE])
			&& cycles > 0.0
		{
			split(&mut out, LEVEL_TWO[2], f, idle / cycles);
		}
		if let Some([mispredicts, resyncs]) = together.find([MISPREDICTS, RESYNCS])
			&& mispredicts + resyncs > 0.0
		{
			split(&mut out, LEVEL_TWO[1], b, b * mispredicts / (mispredicts + resyncs));
		}
		if let Some([loads, stalls]) = together.find([LOAD_STALLS, STALLS])
			&& stalls > 0.0
		{
			split(&mut out, LEVEL_TWO[3], e, e * loads / stalls);
		}
	}
	out
}

/// The breakdown's first level, as swole names each share.
pub const LEVEL_ONE: [&str; 4] = [
	"topdown retiring",
	"topdown bad speculation",
	"topdown frontend bound",
	"topdown backend bound",
];

/// The breakdown's second level, each first-level share's split, the counted part first.
pub const LEVEL_TWO: [[&str; 2]; 4] = [
	["topdown heavy operations", "topdown light operations"],
	["topdown branch mispredicts", "topdown machine clears"],
	["topdown fetch latency", "topdown fetch bandwidth"],
	["topdown memory bound", "topdown core bound"],
];

/// Clears the capabilities swole would hand every program it runs: a wrapper such as NixOS's
/// raises the ones it grants into the ambient set, which survives `exec` into an ordinary
/// program, and a benchmark target must get none.
pub fn drop_ambient() -> Result<(), String> {
	#[cfg(target_os = "linux")]
	{
		// SAFETY: these options take integers alone.
		if unsafe { prctl(PR_CAP_AMBIENT, PR_CAP_AMBIENT_CLEAR_ALL, 0, 0, 0) } != 0 {
			let err = std::io::Error::last_os_error();
			// A kernel before 4.3 has no ambient set to clear.
			if err.raw_os_error() != Some(EINVAL) {
				return Err(format!("cannot clear the ambient capabilities: {err}"));
			}
		}
	}
	Ok(())
}

/// The executable swole runs its own helpers through: itself, or, when it holds file
/// capabilities, the `cargo-swole` beside it, since Valgrind refuses to run a capability-bearing
/// executable and glibc strips a launch's `LD_LIBRARY_PATH` before such a runner can record it.
pub fn helper() -> Result<std::path::PathBuf, String> {
	let exe = std::env::current_exe().map_err(|e| format!("cannot locate swole itself: {e}"))?;
	#[cfg(target_os = "linux")]
	if secure(&exe) {
		let companion = exe.with_file_name("cargo-swole");
		if !companion.is_file() {
			return Err(format!(
				"{} holds capabilities, so swole runs its helpers through {}, which is missing: install both of swole's binaries side by side",
				exe.display(),
				companion.display()
			));
		}
		return Ok(companion);
	}
	Ok(exe)
}

/// Whether Valgrind would refuse `exe` and glibc strip what a runner records: it ran in secure
/// mode, or carries file capabilities the kernel ignored, as on a `nosuid` mount.
#[cfg(target_os = "linux")]
fn secure(exe: &std::path::Path) -> bool {
	use std::os::unix::ffi::OsStrExt;
	// SAFETY: getauxval reads the auxiliary vector alone.
	if unsafe { getauxval(AT_SECURE) } != 0 {
		return true;
	}
	let Ok(path) = std::ffi::CString::new(exe.as_os_str().as_bytes()) else {
		return false;
	};
	// SAFETY: both names are C strings, and a null buffer of no size only asks the length.
	unsafe { getxattr(path.as_ptr(), c"security.capability".as_ptr(), std::ptr::null_mut(), 0) >= 0 }
}

/// Whether this process holds `CAP_PERFMON` or `CAP_SYS_ADMIN`, by its effective set.
fn perfmon() -> Result<bool, String> {
	let Ok(status) = std::fs::read_to_string("/proc/self/status") else {
		return Ok(false);
	};
	let caps = status
		.lines()
		.find_map(|line| line.strip_prefix("CapEff:"))
		.ok_or("/proc/self/status names no effective capabilities")?;
	let caps = u64::from_str_radix(caps.trim(), 16).map_err(|_| format!("CapEff is {caps:?}"))?;
	Ok(caps & (1 << CAP_PERFMON | 1 << CAP_SYS_ADMIN) != 0)
}

const CAP_SYS_ADMIN: u32 = 21;
const CAP_PERFMON: u32 = 38;

/// A CPU list as the kernel writes one, such as `0-7,16`.
pub fn list(cpus: &[usize]) -> String {
	let mut out = Vec::new();
	let mut i = 0;
	while i < cpus.len() {
		let mut j = i;
		while j + 1 < cpus.len() && cpus[j + 1] == cpus[j] + 1 {
			j += 1;
		}
		out.push(if i == j {
			cpus[i].to_string()
		} else {
			format!("{}-{}", cpus[i], cpus[j])
		});
		i = j + 1;
	}
	out.join(",")
}

fn read(path: &Path) -> Result<String, String> {
	std::fs::read_to_string(path)
		.map(|text| text.trim().to_owned())
		.map_err(|e| format!("cannot read {}: {e}", path.display()))
}

/// The CPUs a list such as `0-7,16` names.
pub fn cpus(list: &str) -> Result<Vec<usize>, String> {
	let mut cpus = Vec::new();
	for range in list.trim().split(',').filter(|range| !range.is_empty()) {
		let number = |n: &str| n.parse::<usize>().map_err(|_| format!("{list:?} is no CPU list"));
		match range.split_once('-') {
			Some((first, last)) => cpus.extend(number(first)?..=number(last)?),
			None => cpus.push(number(range)?),
		}
	}
	Ok(cpus)
}

/// `perf_event_attr`'s first 64 bytes, `PERF_ATTR_SIZE_VER0`, all a count needs.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
struct Attr {
	kind: u32,
	size: u32,
	config: u64,
	sample_period: u64,
	sample_type: u64,
	read_format: u64,
	flags: u64,
	wakeup_events: u32,
	bp_type: u32,
	config1: u64,
}

const PERF_TYPE_HARDWARE: u32 = 0;
const PERF_TYPE_SOFTWARE: u32 = 1;
const PERF_TYPE_HW_CACHE: u32 = 3;
/// Where a hardware event's config names the PMU on a host with several.
const PERF_PMU_TYPE_SHIFT: u32 = 32;
const PERF_FORMAT_TOTAL_TIME_ENABLED: u64 = 1;
const PERF_FORMAT_TOTAL_TIME_RUNNING: u64 = 2;
const DISABLED: u64 = 1 << 0;
/// `inherit`: every thread and child the task starts from then on counts a copy of the event.
const INHERIT_THREADS: u64 = 1 << 1;
const EXCLUDE_USER: u64 = 1 << 4;
const EXCLUDE_KERNEL: u64 = 1 << 5;
const EXCLUDE_HV: u64 = 1 << 6;

#[cfg(target_os = "linux")]
mod sys {
	pub const ADDR_NO_RANDOMIZE: u64 = 0x0040000;
	pub const PERF_FLAG_FD_CLOEXEC: u64 = 1 << 3;
	pub const PERF_EVENT_IOC_ENABLE: u64 = 0x2400;
	pub const PERF_EVENT_IOC_DISABLE: u64 = 0x2401;
	pub const PERF_IOC_FLAG_GROUP: u64 = 1;
	pub const AT_SECURE: u64 = 23;
	pub const PR_CAP_AMBIENT: i32 = 47;
	pub const PR_CAP_AMBIENT_CLEAR_ALL: u64 = 4;
	#[cfg(target_arch = "x86_64")]
	pub const PROT_READ: i32 = 1;
	#[cfg(target_arch = "x86_64")]
	pub const MAP_SHARED: i32 = 1;
	#[cfg(target_arch = "x86_64")]
	pub const MAP_FAILED: *mut std::ffi::c_void = !0 as *mut std::ffi::c_void;
	/// One page, what the kernel maps for a counter without a ring buffer.
	#[cfg(target_arch = "x86_64")]
	pub const PAGE: usize = 4096;
	/// Where `perf_event_mmap_page` holds its capabilities, and the bit saying `rdpmc` reads it.
	#[cfg(target_arch = "x86_64")]
	pub const CAPABILITIES: usize = 40;
	#[cfg(target_arch = "x86_64")]
	pub const CAP_USER_RDPMC: u64 = 1 << 2;
	pub const ENOENT: i32 = 2;
	pub const EINVAL: i32 = 22;
	pub const EOPNOTSUPP: i32 = 95;
	#[cfg(target_arch = "x86_64")]
	pub const SYS_PERF_EVENT_OPEN: i64 = 298;
	#[cfg(target_arch = "aarch64")]
	pub const SYS_PERF_EVENT_OPEN: i64 = 241;

	unsafe extern "C" {
		pub fn syscall(number: i64, ...) -> i64;
		pub fn ioctl(fd: i32, request: u64, ...) -> i32;
		pub fn prctl(option: i32, ...) -> i32;
		pub fn getauxval(kind: u64) -> u64;
		pub fn getxattr(
			path: *const std::ffi::c_char,
			name: *const std::ffi::c_char,
			value: *mut std::ffi::c_void,
			size: usize,
		) -> isize;
		pub fn sched_setaffinity(pid: i32, size: usize, mask: *const u64) -> i32;
		pub fn sched_getaffinity(pid: i32, size: usize, mask: *mut u64) -> i32;
		pub fn personality(persona: u64) -> i32;
		#[cfg(target_arch = "x86_64")]
		pub fn mmap(
			addr: *mut std::ffi::c_void,
			len: usize,
			prot: i32,
			flags: i32,
			fd: i32,
			offset: i64,
		) -> *mut std::ffi::c_void;
		#[cfg(target_arch = "x86_64")]
		pub fn munmap(addr: *mut std::ffi::c_void, len: usize) -> i32;
	}
}

#[cfg(target_os = "linux")]
use sys::*;

/// Opens the counter `attr` of the calling thread, in the group `group` leads, or leading its own
/// at -1.
#[cfg(target_os = "linux")]
fn open(attr: &Attr, group: i32, flags: u64) -> std::io::Result<i32> {
	#[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
	{
		// SAFETY: `attr` is a `perf_event_attr` of the size it states, read during the call.
		let fd = unsafe { syscall(SYS_PERF_EVENT_OPEN, attr as *const Attr, 0i32, -1i32, group, flags) };
		if fd < 0 {
			return Err(std::io::Error::last_os_error());
		}
		Ok(fd as i32)
	}
	#[cfg(not(any(target_arch = "x86_64", target_arch = "aarch64")))]
	{
		let _ = (attr, group, flags);
		Err(std::io::Error::new(
			std::io::ErrorKind::Unsupported,
			"swole opens perf counters on x86_64 and aarch64",
		))
	}
}

/// Opens `events` as one group on the calling thread, counts a loop with them, and says whether
/// the PMU scheduled every one the whole time, or why one would not open.
#[cfg(target_os = "linux")]
fn trial(events: &[Event]) -> std::io::Result<bool> {
	use std::io::Read;
	use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
	let mut fds: Vec<OwnedFd> = Vec::new();
	for event in events {
		let group = fds.first().map_or(-1, AsRawFd::as_raw_fd);
		let fd = open(&event.attr(false), group, PERF_FLAG_FD_CLOEXEC)?;
		// SAFETY: the kernel returned a new descriptor, which nothing else owns.
		fds.push(unsafe { OwnedFd::from_raw_fd(fd) });
	}
	let leader = fds[0].as_raw_fd();
	// SAFETY: the leader is a perf event descriptor; the flag is an integer.
	if unsafe { ioctl(leader, PERF_EVENT_IOC_ENABLE, PERF_IOC_FLAG_GROUP) } != 0 {
		return Err(std::io::Error::last_os_error());
	}
	let mut sum = 0u64;
	for i in 0..100_000u64 {
		sum = std::hint::black_box(sum.wrapping_add(i));
	}
	// SAFETY: as above.
	unsafe { ioctl(leader, PERF_EVENT_IOC_DISABLE, PERF_IOC_FLAG_GROUP) };
	for fd in &fds {
		let mut bytes = [0u8; 24];
		std::fs::File::from(fd.try_clone()?).read_exact(&mut bytes)?;
		let [_, enabled, running] =
			std::array::from_fn(|i| u64::from_ne_bytes(bytes[i * 8..i * 8 + 8].try_into().expect("8 bytes")));
		if running == 0 || running != enabled {
			return Ok(false);
		}
	}
	Ok(true)
}

/// Fails unless the harness can read `event` of `pmu` in user space, as it reads every hardware
/// counter: with `rdpmc`, through the page the kernel maps for it.
#[cfg(target_os = "linux")]
fn readable(event: &Event, pmu: &Pmu) -> Result<(), String> {
	#[cfg(target_arch = "x86_64")]
	{
		use std::os::fd::{FromRawFd, OwnedFd};
		let fd = open(&event.attr(false), -1, PERF_FLAG_FD_CLOEXEC).map_err(|err| refused(&err))?;
		// SAFETY: the kernel returned a new descriptor, which nothing else owns.
		let fd = unsafe { OwnedFd::from_raw_fd(fd) };
		// SAFETY: maps the counter's one page, read-only and shared, and unmaps it below.
		let page = unsafe {
			mmap(
				std::ptr::null_mut(),
				PAGE,
				PROT_READ,
				MAP_SHARED,
				std::os::fd::AsRawFd::as_raw_fd(&fd),
				0,
			)
		};
		if page == MAP_FAILED {
			return Err(format!(
				"cannot map a perf counter's page, which swole reads counts through: {}",
				std::io::Error::last_os_error()
			));
		}
		// SAFETY: the page holds `perf_event_mmap_page`'s capabilities at this offset.
		let capabilities = unsafe { std::ptr::read_volatile(page.cast::<u8>().add(CAPABILITIES).cast::<u64>()) };
		// SAFETY: the mapping made above, of this length.
		unsafe { munmap(page, PAGE) };
		if capabilities & CAP_USER_RDPMC == 0 {
			return Err(format!(
				"this host does not let a process read PMU {}'s counters in user space (cap_user_rdpmc is off), which \
				 swole counts with, so the system calls that start and stop counting stay out of a count: root turns it \
				 on by writing 1 to {PMUS}/{}/rdpmc",
				pmu.name, pmu.name
			));
		}
		Ok(())
	}
	#[cfg(not(target_arch = "x86_64"))]
	{
		let _ = (event, pmu);
		Err("swole reads perf counters in user space with x86_64's rdpmc alone".to_owned())
	}
}

/// Pins the calling thread to `cpus`.
#[cfg(target_os = "linux")]
fn pin(cpus: &[usize]) -> std::io::Result<()> {
	let mask = mask(cpus);
	// SAFETY: the mask is as large as it says.
	if unsafe { sched_setaffinity(0, mask.len() * 8, mask.as_ptr()) } != 0 {
		return Err(std::io::Error::last_os_error());
	}
	Ok(())
}

/// The affinity mask of `cpus`.
fn mask(cpus: &[usize]) -> Vec<u64> {
	let mut mask = vec![0u64; cpus.iter().max().map_or(1, |max| max / 64 + 1)];
	for cpu in cpus {
		mask[cpu / 64] |= 1 << (cpu % 64);
	}
	mask
}

/// The CPUs swole may run on.
#[cfg(target_os = "linux")]
pub fn allowed() -> Result<Vec<usize>, String> {
	let mut mask = [0u64; 64];
	// SAFETY: the mask is as large as it says.
	if unsafe { sched_getaffinity(0, size_of_val(&mask), mask.as_mut_ptr()) } != 0 {
		return Err(format!(
			"cannot read the CPUs swole may run on: {}",
			std::io::Error::last_os_error()
		));
	}
	Ok((0..mask.len() * 64)
		.filter(|cpu| mask[cpu / 64] & 1 << (cpu % 64) != 0)
		.collect())
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn reads_and_writes_a_cpu_list() {
		assert_eq!(cpus("0-3,8,10-11\n").unwrap(), [0, 1, 2, 3, 8, 10, 11]);
		assert_eq!(cpus("").unwrap(), Vec::<usize>::new());
		assert!(cpus("0-x").is_err());
		assert_eq!(list(&[0, 1, 2, 3, 8, 10, 11]), "0-3,8,10-11");
		assert_eq!(list(&[]), "");
	}

	#[test]
	fn sets_an_event_spec_by_its_format() {
		let dir = std::env::temp_dir().join(format!("swole-format-{}", std::process::id()));
		std::fs::create_dir_all(dir.join("format")).unwrap();
		std::fs::write(dir.join("format/event"), "config:0-7\n").unwrap();
		std::fs::write(dir.join("format/umask"), "config:8-15,32-35\n").unwrap();
		std::fs::write(dir.join("format/edge"), "config:18\n").unwrap();
		std::fs::write(dir.join("format/ldlat"), "config1:0-15\n").unwrap();
		assert_eq!(
			sysfs(&dir, "event=0xc2,umask=0x302,edge,ldlat=3").unwrap(),
			(0xc2 | 0x02 << 8 | 0x3 << 32 | 1 << 18, 3)
		);
		assert!(sysfs(&dir, "cmask=1").is_err());
		assert!(sysfs(&dir, "event=x").is_err());
		std::fs::remove_dir_all(dir).unwrap();
	}

	#[test]
	fn reads_what_a_harness_counted() {
		let out = "swole libc glibc 2.44 (build-id 133180439d23)\nswole inherits\nswole threaded a.rs::g\nswole alone a.rs::f[1 2]\nswole rusage 1 2 3 4 5 6 7\nswole counted 4096 9 a.rs::f[1 2]\n\
		           swole ran a.rs::f[1 2]\nown output\nswole calls 3 a.rs::g\nswole counted 12 0 a.rs::g\n";
		let usage = Usage {
			user_ns: 1,
			system_ns: 2,
			max_rss: 3,
			minor_faults: 4,
			major_faults: 5,
			voluntary_switches: 6,
			involuntary_switches: 7,
		};
		assert_eq!(
			readings(out, 2).unwrap(),
			[
				Reading {
					name: "a.rs::f[1 2]".to_owned(),
					values: vec![4096, 9],
					calls: None,
					usage: Some(usage),
					threaded: false,
					libc: Some("glibc 2.44 (build-id 133180439d23)".to_owned()),
					inherits: true,
				},
				Reading {
					name: "a.rs::g".to_owned(),
					values: vec![12, 0],
					calls: Some(3),
					usage: None,
					threaded: true,
					libc: Some("glibc 2.44 (build-id 133180439d23)".to_owned()),
					inherits: true,
				}
			]
		);
		// A threaded case's harness reads no counters, and says it was threaded.
		assert_eq!(
			readings("\nswole libc static\nswole threaded a.rs::h\nswole ran a.rs::h\n", 2).unwrap(),
			[Reading {
				name: "a.rs::h".to_owned(),
				values: Vec::new(),
				calls: None,
				usage: None,
				threaded: true,
				libc: Some("static".to_owned()),
				inherits: false,
			}]
		);
		// A thread record a fragment without a newline swallowed is no record.
		assert!(readings("own outputswole threaded a.rs::f\nswole counted 1 2 a.rs::f\n", 2).is_err());
		assert!(readings("swole counted 1 a.rs::f\n", 2).is_err());
		assert!(
			readings(
				"swole alone a.rs::f\nswole calls 0 a.rs::f\nswole counted 1 2 a.rs::f\n",
				2
			)
			.is_err()
		);
		// A count record of another name, such as a benchmark's own line, covers nothing.
		let other = readings("swole alone a.rs::f\nswole calls 5 b\nswole counted 1 2 a.rs::f\n", 2).unwrap();
		assert_eq!(other[0].calls, None);
		assert!(readings("swole counted x 1 a.rs::f\n", 2).is_err());
		assert!(readings("swole counted 1 2\n", 2).is_err());
		assert!(readings("swole rusage 1 2\n", 2).is_err());
	}

	#[test]
	fn derives_instructions_a_cycle_only_within_one_group() {
		let pmu = Pmu {
			name: "cpu".to_owned(),
			extended: 0,
			dir: None,
			cpus: vec![0],
		};
		let pass = Pass {
			groups: vec![vec![
				generic(&pmu, "instructions", PERF_TYPE_HARDWARE, 1, false),
				generic(&pmu, "cycles", PERF_TYPE_HARDWARE, 0, false),
				generic(&pmu, "ref-cycles", PERF_TYPE_HARDWARE, 9, false),
			]],
			read: false,
		};
		let same = measures(&[pass], &[(vec![300, 100, 80], 2)]);
		let shown: Vec<(&str, f64)> = same.iter().map(|m| (m.name.as_str(), m.value)).collect();
		assert_eq!(
			shown,
			[
				("instructions:u", 150.0),
				("cycles:u", 50.0),
				("ref-cycles:u", 40.0),
				("IPC:u", 3.0),
				("cycles/ref-cycles:u", 1.25)
			]
		);

		// Separate passes (different processes) must not derive IPC across them.
		let split = [
			Pass {
				groups: vec![vec![generic(&pmu, "instructions", PERF_TYPE_HARDWARE, 1, false)]],
				read: false,
			},
			Pass {
				groups: vec![
					vec![generic(&pmu, "cycles", PERF_TYPE_HARDWARE, 0, false)],
					vec![generic(&pmu, "ref-cycles", PERF_TYPE_HARDWARE, 9, false)],
				],
				read: false,
			},
		];
		let split_measures = measures(&split, &[(vec![300], 2), (vec![100, 80], 2)]);
		let split_shown: Vec<(&str, f64)> = split_measures.iter().map(|m| (m.name.as_str(), m.value)).collect();
		assert_eq!(
			split_shown,
			[("instructions:u", 150.0), ("cycles:u", 50.0), ("ref-cycles:u", 40.0)]
		);
	}

	/// An event of a PMU's own, as the breakdown names them.
	fn own(name: &str, scale: u64) -> Event {
		Event {
			name: name.to_owned(),
			kind: 4,
			config: 0,
			config1: 0,
			kernel: false,
			hardware: true,
			scale,
		}
	}

	/// Each measure that `passes` derive, counting `values`, as a percentage.
	fn percentages(passes: &[Pass], values: &[(Vec<u64>, u64)]) -> Vec<(String, f64)> {
		measures(passes, values)
			.into_iter()
			.filter(|measure| measure.unit == "%")
			.map(|measure| (measure.name, (measure.value * 1000.0).round() / 1000.0))
			.collect()
	}

	fn named(shares: &[(&str, f64)]) -> Vec<(String, f64)> {
		shares.iter().map(|&(name, share)| (name.to_owned(), share)).collect()
	}

	#[test]
	fn breaks_intels_metrics_down_by_their_slots() {
		let names = [
			"slots",
			"topdown-retiring",
			"topdown-bad-spec",
			"topdown-fe-bound",
			"topdown-be-bound",
		];
		let mut group: Vec<Event> = names.iter().map(|name| own(&format!("{name}:u"), 1)).collect();
		group.extend(
			[
				"topdown-heavy-ops",
				"topdown-br-mispredict",
				"topdown-fetch-lat",
				"topdown-mem-bound",
			]
			.map(|name| own(&format!("{name}:u"), 1)),
		);
		let pass = Pass {
			groups: vec![group],
			read: true,
		};
		// The harness reads them all with `read()`.
		assert_eq!(pass.hardware(), 0);
		assert_eq!(
			percentages(&[pass], &[(vec![1000, 400, 100, 300, 200, 50, 80, 120, 150], 4)]),
			named(&[
				("topdown retiring", 40.0),
				("topdown bad speculation", 10.0),
				("topdown frontend bound", 30.0),
				("topdown backend bound", 20.0),
				("topdown heavy operations", 5.0),
				("topdown light operations", 35.0),
				("topdown branch mispredicts", 8.0),
				("topdown machine clears", 2.0),
				("topdown fetch latency", 12.0),
				("topdown fetch bandwidth", 18.0),
				("topdown memory bound", 15.0),
				("topdown core bound", 5.0),
			])
		);
		// Intel's E-cores count the first level as events, with no slots: their sum is.
		let atom = Pass {
			groups: vec![names[1..].iter().map(|name| own(&format!("{name}:u"), 1)).collect()],
			read: false,
		};
		assert_eq!(
			percentages(&[atom], &[(vec![50, 25, 15, 10], 1)]),
			named(&[
				("topdown retiring", 50.0),
				("topdown bad speculation", 25.0),
				("topdown frontend bound", 15.0),
				("topdown backend bound", 10.0),
			])
		);
	}

	#[test]
	fn breaks_amds_dispatch_slots_down_as_perfs_metrics_do() {
		// Zen 5's eight slots a cycle, and the second level's pairs in a pass of their own.
		let first = Pass {
			groups: vec![vec![
				own(SLOTS, 8),
				own(FRONTEND, 1),
				own(BACKEND, 1),
				own(DISPATCHED, 1),
				own(RETIRED, 1),
				own(SMT, 1),
			]],
			read: false,
		};
		let second = Pass {
			groups: vec![
				vec![own(MICROCODE, 1), own(RETIRED_BESIDE, 1)],
				vec![own(CYCLES, 1), own(FRONTEND_IDLE, 1)],
				vec![own(MISPREDICTS, 1), own(RESYNCS, 1)],
				vec![own(LOAD_STALLS, 1), own(STALLS, 1)],
			],
			read: false,
		};
		assert_eq!(
			percentages(
				&[first, second],
				&[
					(vec![100, 240, 160, 400, 320, 80], 2),
					(vec![1, 4, 100, 10, 3, 1, 30, 40], 2)
				]
			),
			named(&[
				("topdown retiring", 40.0),
				("topdown bad speculation", 10.0),
				("topdown frontend bound", 30.0),
				("topdown backend bound", 20.0),
				("topdown SMT contention", 10.0),
				("topdown heavy operations", 10.0),
				("topdown light operations", 30.0),
				("topdown fetch latency", 10.0),
				("topdown fetch bandwidth", 20.0),
				("topdown branch mispredicts", 7.5),
				("topdown machine clears", 2.5),
				("topdown memory bound", 15.0),
				("topdown core bound", 5.0),
			])
		);
		// The slots are the cycles they stand for.
		let slots = measures(
			&[Pass {
				groups: vec![vec![own(SLOTS, 8)]],
				read: false,
			}],
			&[(vec![100], 2)],
		);
		assert_eq!(slots[0].value, 400.0);
	}

	#[test]
	fn knows_zen_4_and_5_by_family_and_model() {
		let cpu = |vendor: &str, family: u32, model: u32| {
			Zen::of(&format!(
				"processor\t: 0\nvendor_id\t: {vendor}\ncpu family\t: {family}\nmodel\t\t: {model}\n"
			))
		};
		assert_eq!(cpu("AuthenticAMD", 26, 0x44), Some(Zen::Five));
		assert_eq!(cpu("AuthenticAMD", 26, 0x50), None);
		assert_eq!(cpu("AuthenticAMD", 25, 0x61), Some(Zen::Four));
		assert_eq!(cpu("AuthenticAMD", 25, 0x21), None);
		assert_eq!(cpu("AuthenticAMD", 25, 0x01), None);
		assert_eq!(cpu("AuthenticAMD", 23, 0x71), None);
		assert_eq!(cpu("GenuineIntel", 6, 0xc6), None);
		assert_eq!(Zen::Four.width(), 6);
		assert_eq!(Zen::Five.width(), 8);
	}
}
