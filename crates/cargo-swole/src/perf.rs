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

/// What a harness target prints before each counter's value, in the order swole opened them, and
/// the benchmark's name.
const COUNTED: &str = "swole counted ";

/// What a harness target prints before what the process used over the benchmark's calls.
pub const RUSAGE: &str = "swole rusage ";

/// What a harness target prints before the name of a benchmark beside which it had another thread.
const THREADED: &str = "swole threaded ";

/// Where the kernel lists its PMUs.
const PMUS: &str = "/sys/bus/event_source/devices";

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
	/// Whether it counts only the kernel, rather than only user space.
	kernel: bool,
	/// Whether it takes one of the PMU's counters, as every event but the kernel's software ones does.
	hardware: bool,
}

impl Event {
	/// The `perf_event_attr` that opens it, stopped, counting each thread the process starts.
	fn attr(&self) -> Attr {
		Attr {
			kind: self.kind,
			size: size_of::<Attr>() as u32,
			config: self.config,
			read_format: PERF_FORMAT_TOTAL_TIME_ENABLED | PERF_FORMAT_TOTAL_TIME_RUNNING,
			flags: DISABLED | INHERIT | EXCLUDE_HV | if self.kernel { EXCLUDE_USER } else { EXCLUDE_KERNEL },
			config1: self.config1,
			..Attr::default()
		}
	}

	/// What it counts in, as Kixel Quest names a unit.
	pub fn unit(&self) -> &'static str {
		if self.name == "task-clock" { "ns" } else { "events" }
	}
}

/// The counters one process opens: each group's leader then its siblings, which the PMU counts
/// together or not at all, so none is multiplexed against another of its group.
#[derive(Clone, Debug, PartialEq)]
pub struct Pass {
	pub groups: Vec<Vec<Event>>,
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
pub struct Perf {
	pub pmu: Pmu,
	/// Whether it counts the kernel as well as user space, as tier L1 lets bench.
	pub kernel: bool,
	/// How many general-purpose counters the PMU schedules at once.
	pub slots: usize,
	/// Every event it counts, in the passes that count them all: hardware ones packed into groups
	/// that each fit the PMU at once, and software ones beside the first.
	pub passes: Vec<Pass>,
	/// The generic events this PMU does not count.
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
				writeln!(
					out,
					"PMU: {} on CPUs {}, {} general-purpose counters",
					perf.pmu.name,
					list(&perf.pmu.cpus),
					perf.slots
				)
				.expect("a String takes every write");
				for (i, pass) in perf.passes.iter().enumerate() {
					let names: Vec<&str> = pass.events().map(|event| event.name.as_str()).collect();
					writeln!(out, "pass {}: {}", i + 1, names.join(" ")).expect("a String takes every write");
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
		let pmu = Pmu::find()?;
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
		let slots = (1..=MOST_SLOTS)
			.take_while(|&n| {
				let branches = vec![generic(&pmu, "branches", PERF_TYPE_HARDWARE, 4, false); n];
				matches!(trial(&branches), Ok(true))
			})
			.last()
			.ok_or_else(|| format!("PMU {} schedules not even one branch counter", pmu.name))?;
		let mut units: Vec<Vec<Event>> = Vec::new();
		let mut absent = Vec::new();
		for (name, kind, config) in GENERIC {
			if quick && !QUICK.contains(&name) {
				continue;
			}
			let rings: &[bool] = if kernel && !quick { &[false, true] } else { &[false] };
			for &ring in rings {
				let event = generic(&pmu, name, kind, config, ring);
				match trial(std::slice::from_ref(&event)) {
					Ok(true) => units.push(vec![event]),
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
		if let Some(topdown) = topdown(&pmu)?.filter(|_| !quick) {
			match trial(&topdown) {
				Ok(true) => units.push(topdown),
				Ok(_) => absent.push("topdown".to_owned()),
				Err(err) if unsupported(&err) => absent.push("topdown".to_owned()),
				Err(err) => return Err(format!("cannot open PMU {}'s top-down counters: {err}", pmu.name)),
			}
		}
		let mut groups: Vec<Vec<Event>> = Vec::new();
		for unit in units {
			if let Some(group) = groups.last_mut() {
				let mut wider = group.clone();
				wider.extend(unit.iter().cloned());
				if matches!(trial(&wider), Ok(true)) {
					*group = wider;
					continue;
				}
			}
			groups.push(unit);
		}
		let mut passes: Vec<Pass> = groups.into_iter().map(|group| Pass { groups: vec![group] }).collect();
		for (name, config) in SOFTWARE.into_iter().filter(|_| !quick) {
			let event = Event {
				name: name.to_owned(),
				kind: PERF_TYPE_SOFTWARE,
				config,
				config1: 0,
				// At L2 the kernel takes no counter of its own work; a software event counts
				// what the kernel does for the process either way.
				kernel: false,
				hardware: false,
			};
			trial(std::slice::from_ref(&event))
				.map_err(|err| format!("cannot open a perf counter of {name}: {err}"))?;
			passes[0].groups.push(vec![event]);
		}
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
const GENERIC: [(&str, u32, u64); 11] = [
	("instructions", PERF_TYPE_HARDWARE, 1),
	("cycles", PERF_TYPE_HARDWARE, 0),
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

/// The software events: each name and config.
const SOFTWARE: [(&str, u64); 4] = [
	("task-clock", 1),
	("page-faults", 2),
	("context-switches", 3),
	("cpu-migrations", 4),
];

/// The first level of the top-down breakdown, as a PMU exports it among its own events.
const TOPDOWN: [&str; 4] = [
	"topdown-retiring",
	"topdown-bad-spec",
	"topdown-fe-bound",
	"topdown-be-bound",
];

/// A generic event of `pmu`, in user space or, when `kernel`, the kernel.
fn generic(pmu: &Pmu, name: &str, kind: u32, config: u64, kernel: bool) -> Event {
	Event {
		name: format!("{name}:{}", if kernel { "k" } else { "u" }),
		kind,
		config: u64::from(pmu.extended) << PERF_PMU_TYPE_SHIFT | config,
		config1: 0,
		kernel,
		hardware: true,
	}
}

/// The top-down events `pmu` exports, in user space, as one group: led by its `slots` event
/// where it has one, as a PMU that computes them from it requires.
fn topdown(pmu: &Pmu) -> Result<Option<Vec<Event>>, String> {
	let Some(dir) = &pmu.dir else {
		return Ok(None);
	};
	let kind = read(&dir.join("type"))?
		.parse()
		.map_err(|_| format!("{}/type is no perf type", dir.display()))?;
	let mut events = Vec::new();
	for name in std::iter::once("slots").chain(TOPDOWN) {
		let path = dir.join("events").join(name);
		let Ok(spec) = std::fs::read_to_string(&path) else {
			continue;
		};
		// The kernel pads some specs with NULs.
		let spec = spec.trim_matches(|c: char| c == '\0' || c.is_whitespace());
		let (config, config1) = sysfs(dir, spec).map_err(|err| format!("{}: {err}", path.display()))?;
		events.push(Event {
			name: format!("{name}:u"),
			kind,
			config,
			config1,
			kernel: false,
			hardware: true,
		});
	}
	// `slots` alone is no breakdown.
	Ok(events.iter().any(|event| event.name != "slots:u").then_some(events))
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
	/// The core PMU of the most CPUs swole may run on. On a host whose CPUs are of several kinds,
	/// such as Intel's P- and E-cores, each kind has its own PMU, which counts only on its CPUs.
	#[cfg(target_os = "linux")]
	fn find() -> Result<Self, String> {
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

/// Has `command` run on `pmu`'s CPUs, at the addresses of its last run when `fixed`, and open
/// `pass`'s counters on itself just before it `exec`s, telling the harness how many.
pub fn launch(command: &mut std::process::Command, pmu: &Pmu, pass: &Pass, fixed: bool) {
	command.env(COUNTERS, pass.len().to_string());
	#[cfg(target_os = "linux")]
	{
		use std::os::unix::process::CommandExt;
		let mask = mask(&pmu.cpus);
		// Each event's attributes, and whether it leads its group, built before the fork.
		let attrs: Vec<(Attr, bool)> = pass
			.groups
			.iter()
			.flat_map(|group| group.iter().enumerate().map(|(i, event)| (event.attr(), i == 0)))
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
	let _ = (command, pmu, fixed);
}

/// What one benchmark's count read: its counters' values in the order of its pass, what the
/// process used over its calls, and whether another thread was there.
#[derive(Debug, PartialEq)]
pub struct Reading {
	pub name: String,
	pub values: Vec<u64>,
	pub usage: Option<Usage>,
	pub threaded: bool,
}

/// What a harness target's output says each benchmark it counted with `counters` counters read.
pub fn readings(stdout: &str, counters: usize) -> Result<Vec<Reading>, String> {
	let threaded = threaded(stdout);
	let mut usage = None;
	let mut readings = Vec::new();
	for line in stdout.lines() {
		if let Some(fields) = line.strip_prefix(RUSAGE) {
			usage = Some(Usage::parse(fields)?);
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
			readings.push(Reading {
				name: name.to_owned(),
				values,
				usage: usage.take(),
				threaded: threaded.contains(name),
			});
		}
	}
	Ok(readings)
}

/// The benchmarks a harness target's output says it had another thread beside.
pub fn threaded(stdout: &str) -> std::collections::BTreeSet<String> {
	stdout
		.lines()
		.filter_map(|line| line.strip_prefix(THREADED))
		.map(str::to_owned)
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

/// Each counter `passes` read, a call's share over `calls` calls, then what derives from them:
/// instructions a cycle, in each ring both count.
pub fn measures(passes: &[Pass], values: &[Vec<u64>], calls: u64) -> Vec<crate::Measure> {
	let mut out: Vec<crate::Measure> = passes
		.iter()
		.zip(values)
		.flat_map(|(pass, values)| pass.events().zip(values))
		.map(|(event, &value)| crate::Measure::new(&event.name, value as f64 / calls as f64, event.unit()))
		.collect();
	for ring in ["u", "k"] {
		let find = |name: &str| out.iter().find(|m| m.name == format!("{name}:{ring}")).map(|m| m.value);
		if let (Some(instructions), Some(cycles)) = (find("instructions"), find("cycles"))
			&& cycles > 0.0
		{
			out.push(crate::Measure::new(
				&format!("IPC:{ring}"),
				instructions / cycles,
				"instructions/cycle",
			));
		}
	}
	out
}

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
	// SAFETY: getauxval reads the auxiliary vector alone.
	if unsafe { getauxval(AT_SECURE) } != 0 {
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
fn cpus(list: &str) -> Result<Vec<usize>, String> {
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
const INHERIT: u64 = 1 << 1;
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
		pub fn sched_setaffinity(pid: i32, size: usize, mask: *const u64) -> i32;
		pub fn sched_getaffinity(pid: i32, size: usize, mask: *mut u64) -> i32;
		pub fn personality(persona: u64) -> i32;
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
		let fd = open(&event.attr(), group, PERF_FLAG_FD_CLOEXEC)?;
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
fn allowed() -> Result<Vec<usize>, String> {
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
		let out = "swole threaded a.rs::g\nswole rusage 1 2 3 4 5 6 7\nswole counted 4096 9 a.rs::f[1 2]\n\
		           swole ran a.rs::f[1 2]\nown output\nswole counted 12 0 a.rs::g\n";
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
					usage: Some(usage),
					threaded: false
				},
				Reading {
					name: "a.rs::g".to_owned(),
					values: vec![12, 0],
					usage: None,
					threaded: true
				}
			]
		);
		assert!(readings("swole counted 1 a.rs::f\n", 2).is_err());
		assert!(readings("swole counted x 1 a.rs::f\n", 2).is_err());
		assert!(readings("swole counted 1 2\n", 2).is_err());
		assert!(readings("swole rusage 1 2\n", 2).is_err());
	}

	#[test]
	fn derives_instructions_a_cycle() {
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
			]],
		};
		let measures = measures(&[pass], &[vec![300, 100]], 2);
		let shown: Vec<(&str, f64)> = measures.iter().map(|m| (m.name.as_str(), m.value)).collect();
		assert_eq!(shown, [("instructions:u", 150.0), ("cycles:u", 50.0), ("IPC:u", 3.0)]);
	}
}
