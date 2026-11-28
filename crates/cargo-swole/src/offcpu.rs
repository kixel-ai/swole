//! Where a benchmark's threads wait off the CPU, beside `bench`'s times: a timed process of its own
//! records each of its threads' context switches, and by the kernel's syscall tracepoints its
//! futex calls, while the harness's clock runs, and splits the time a thread spent off the CPU
//! into blocked and preempted; of the blocked, what it waited in a futex; and of that, what it
//! waited to take a lock, where the code that waited says it was taking one.
//!
//! swole opens the events on each CPU the process runs on whole, just before it `exec`s, keeping the
//! process's records alone, and hands them to itself as a profile's, so it reads their ring buffers
//! while the target holds none; the harness starts and stops them with its clock, as a profile's.
//! Events of the process its threads inherited instead would cost each thread the process starts
//! some 7 µs a tracepoint and CPU, hundreds of microseconds over a spread's CPUs, which would
//! change what a benchmark that starts threads waits on.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use crate::Measure;

/// Where tracefs lists the kernel's tracepoints, mounted on its own or under debugfs.
const TRACEFS: [&str; 2] = ["/sys/kernel/tracing", "/sys/kernel/debug/tracing"];

/// The futex operations that wait, of its command less its private and clock flags: `FUTEX_WAIT`,
/// `FUTEX_LOCK_PI`, `FUTEX_WAIT_BITSET`, `FUTEX_WAIT_REQUEUE_PI`, and `FUTEX_LOCK_PI2`.
const WAITS: [u64; 5] = [0, 6, 9, 11, 13];

/// The futex operations that take a priority-inheriting lock, whoever calls them.
const LOCKS: [u64; 2] = [6, 13];

/// `FUTEX_PRIVATE_FLAG | FUTEX_CLOCK_REALTIME`, which a futex operation's command leaves out.
const FUTEX_FLAGS: u64 = 128 | 256;

/// `PR_TASK_PERF_EVENTS_DISABLE`, which the harness stops its events with as its clock stops.
const STOP: u64 = 31;

/// The largest record a trace's events write, past which a ring may have dropped one: a sample of
/// the futex call's tracepoint, its arguments, and a word of the user stack, with room to spare.
pub const MOST_RECORD: usize = 256;

/// The tracepoints a trace opens, and where their records hold what it reads.
#[derive(Clone, Debug)]
pub struct Tracing {
	/// The IDs of `syscalls:sys_enter_futex`, `sys_exit_futex`, and `sys_enter_prctl`.
	enter: u64,
	exit: u64,
	prctl: u64,
	/// Where in a futex call's tracepoint record its operation lies, and a prctl's option.
	op: usize,
	option: usize,
}

/// Where a futex wait was made: the instruction after its system call, the word atop the user
/// stack there, which a generic wrapper such as libc's `syscall` leaves as its caller's return
/// address, and whether its operation takes a priority-inheriting lock.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Site {
	ip: u64,
	ret: Option<u64>,
	pi: bool,
}

/// What one record of a trace says a thread did.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Event {
	/// It left the CPU, still runnable when preempted.
	Out { preempted: bool },
	/// It came back onto a CPU.
	In,
	/// It entered a futex call, one that waits at a site, or one that does not.
	Enter(Option<Site>),
	/// It returned from a futex call.
	Exit,
	/// The harness stopped the events, closing the window its clock timed.
	Stop,
}

/// One record of a trace, of a thread of the benchmark's process.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Record {
	/// When, in `CLOCK_MONOTONIC` nanoseconds.
	time: u64,
	tid: u32,
	event: Event,
}

/// What a trace's ring buffers held since their last drain.
#[derive(Debug, Default)]
pub struct Drained {
	records: Vec<Record>,
	/// How many rings filled, so the kernel may have dropped records of the window. Nothing frees
	/// a ring's room in a window but the drain after it, so every record dropped in one leaves its
	/// ring full; the record the kernel writes of what it dropped comes only once there is room
	/// again, in a later window, and says nothing of that one.
	pub filled: u64,
}

impl Drained {
	/// What its threads waited.
	pub fn waits(&self) -> Waits {
		account(&self.records)
	}
}

/// The time a trace's threads spent off the CPU, in nanoseconds summed over every thread: from
/// each time one left the CPU in a window its clock timed until it came back or the window closed.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Waits {
	/// Asleep, waiting for anything but a CPU.
	blocked: u64,
	/// Runnable, waiting for a CPU.
	preempted: u64,
	/// Asleep in a futex wait, of the blocked, by where it waited.
	futex: BTreeMap<Site, u64>,
}

impl Waits {
	/// Where its futex waits were made.
	pub fn sites(&self) -> impl Iterator<Item = &Site> {
		self.futex.keys()
	}
}

impl Tracing {
	/// The tracepoints this host lets swole trace its own processes with, opened as a trace opens
	/// them, or why it cannot.
	#[cfg(target_os = "linux")]
	pub fn probe() -> Result<Self, String> {
		let root = TRACEFS
			.iter()
			.map(std::path::Path::new)
			.find(|dir| dir.is_dir())
			.ok_or("this kernel mounts no tracefs, whose tracepoints name a futex call")?;
		let read = |event: &str, file: &str| {
			let path = root.join("events/syscalls").join(event).join(file);
			std::fs::read_to_string(&path).map_err(|err| match err.kind() {
				std::io::ErrorKind::PermissionDenied => format!(
					"swole may not read {}, its tracepoint's ID: grant its user tracefs as the README's Privileges says",
					path.display()
				),
				_ => format!(
					"cannot read {} ({err}), its tracepoint's ID, as a kernel without syscall tracepoints lacks",
					path.display()
				),
			})
		};
		let id = |event: &str| -> Result<u64, String> {
			let text = read(event, "id")?;
			text.trim()
				.parse()
				.map_err(|_| format!("{event}'s tracepoint ID is {text:?}"))
		};
		let tracing = Self {
			enter: id("sys_enter_futex")?,
			exit: id("sys_exit_futex")?,
			prctl: id("sys_enter_prctl")?,
			op: offset(&read("sys_enter_futex", "format")?, "op")?,
			option: offset(&read("sys_enter_prctl", "format")?, "option")?,
		};
		// Opened as a trace opens them, on a CPU swole may run on, and its ring mapped.
		let cpu = *crate::perf::allowed()?
			.first()
			.ok_or("swole may run on no CPU")?;
		let mut fds = [-1i32; Self::EVENTS];
		let opened = tracing.open(cpu as i32, &mut fds);
		// SAFETY: the kernel returned each new descriptor, which nothing else owns.
		let fds: Vec<std::os::fd::OwnedFd> = fds
			.into_iter()
			.filter(|&fd| fd >= 0)
			.map(|fd| unsafe { std::os::fd::FromRawFd::from_raw_fd(fd) })
			.collect();
		opened.map_err(|err| match err.raw_os_error() {
			Some(EACCES | EPERM) => format!(
				"tracing context switches and futex calls needs kernel.perf_event_paranoid at most -1 or CAP_PERFMON, \
				 since it traces whole CPUs ({err})"
			),
			_ => format!("cannot trace context switches and futex calls: {err}"),
		})?;
		Rings::new(&tracing, fds, std::process::id())?;
		Ok(tracing)
	}

	#[cfg(not(target_os = "linux"))]
	pub fn probe() -> Result<Self, String> {
		Err("tracing is Linux's".to_owned())
	}

	/// How many events it opens on each CPU.
	pub const EVENTS: usize = 4;

	/// Opens its events on `cpu` into `fds`: the threads' context switches, then the futex call's
	/// tracepoints and the prctl's, which [`Rings::new`] has write to the first's ring buffer once
	/// it maps it, since the kernel redirects an event only into a mapped one. It makes system
	/// calls alone, as between fork and exec.
	#[cfg(target_os = "linux")]
	pub fn open(&self, cpu: i32, fds: &mut [i32]) -> std::io::Result<()> {
		use crate::perf::{
			Attr, DISABLED, EXCLUDE_HV, EXCLUDE_KERNEL, PERF_SAMPLE_IP, PERF_SAMPLE_TID, PERF_TYPE_SOFTWARE, traced,
		};
		// Each event stopped until the harness's clock starts it, on one clock across CPUs, its
		// records naming their thread and time; a system call's tracepoint takes its user space's
		// registers, which its kernel's exclusion keeps.
		let flags = DISABLED | EXCLUDE_KERNEL | EXCLUDE_HV | SAMPLE_ID_ALL | USE_CLOCKID;
		let base = Attr {
			size: size_of::<Attr>() as u32,
			sample_type: PERF_SAMPLE_TID | PERF_SAMPLE_TIME,
			flags,
			clockid: CLOCK_MONOTONIC,
			..Attr::default()
		};
		let switches = Attr {
			kind: PERF_TYPE_SOFTWARE,
			config: PERF_COUNT_SW_DUMMY,
			flags: flags | CONTEXT_SWITCH,
			..base
		};
		fds[0] = traced(&switches, cpu)?;
		for (fd, id) in fds[1..].iter_mut().zip([self.enter, self.exit, self.prctl]) {
			let tracepoint = Attr {
				kind: PERF_TYPE_TRACEPOINT,
				config: id,
				sample_period: 1,
				sample_type: PERF_SAMPLE_IP | PERF_SAMPLE_TID | PERF_SAMPLE_TIME | PERF_SAMPLE_RAW | PERF_SAMPLE_STACK_USER,
				sample_stack_user: 8,
				..base
			};
			*fd = traced(&tracepoint, cpu)?;
		}
		Ok(())
	}

	/// What `record`, of kind `kind` and header `misc`, says a thread of the process `pid` did:
	/// none for another process's, or a record a trace reads nothing of.
	fn parse(&self, kind: u32, misc: u16, record: &[u8], pid: u32) -> Result<Parsed, String> {
		let malformed = || format!("a trace's ring buffer holds a record of kind {kind} too short to read");
		let word = |at: usize| -> Result<u64, String> {
			Ok(u64::from_ne_bytes(
				record.get(at..at + 8).ok_or_else(malformed)?.try_into().expect("8 bytes"),
			))
		};
		let half = |at: usize| -> Result<u32, String> {
			Ok(u32::from_ne_bytes(
				record.get(at..at + 4).ok_or_else(malformed)?.try_into().expect("4 bytes"),
			))
		};
		match kind {
			PERF_RECORD_SWITCH_CPU_WIDE => {
				// The thread switched to or from, then the record's process and thread, the one
				// switching, and its time, which every event's records end with.
				let (task, tid, time) = (half(16)?, half(20)?, word(24)?);
				if task != pid {
					return Ok(Parsed::Other);
				}
				let event = if misc & PERF_RECORD_MISC_SWITCH_OUT != 0 {
					Event::Out {
						preempted: misc & PERF_RECORD_MISC_SWITCH_OUT_PREEMPT != 0,
					}
				} else {
					Event::In
				};
				Ok(Parsed::Record(Record { time, tid, event }))
			}
			PERF_RECORD_SAMPLE => {
				// The instruction pointer, the process and thread, the time, then the tracepoint's
				// own record, which leads with its ID, then the top of the user stack.
				let (ip, task, tid, time) = (word(8)?, half(16)?, half(20)?, word(24)?);
				if task != pid {
					return Ok(Parsed::Other);
				}
				let size = half(32)? as usize;
				let raw = record.get(36..36 + size).ok_or_else(malformed)?;
				let field = |at: usize| -> Result<u64, String> {
					Ok(u64::from_ne_bytes(
						raw.get(at..at + 8).ok_or_else(malformed)?.try_into().expect("8 bytes"),
					))
				};
				let id = u64::from(u16::from_ne_bytes(
					raw.get(0..2).ok_or_else(malformed)?.try_into().expect("2 bytes"),
				));
				let stack = 36 + size;
				// The word copied, when the stack held one to copy.
				let ret = match (word(stack)?, record.get(stack + 16..stack + 24)) {
					(8, Some(dynamic)) if u64::from_ne_bytes(dynamic.try_into().expect("8 bytes")) >= 8 => {
						Some(word(stack + 8)?)
					}
					_ => None,
				};
				let event = if id == self.enter {
					let op = field(self.op)? & !FUTEX_FLAGS;
					Event::Enter(WAITS.contains(&op).then_some(Site {
						ip,
						ret,
						pi: LOCKS.contains(&op),
					}))
				} else if id == self.exit {
					Event::Exit
				} else if id == self.prctl && field(self.option)? == STOP {
					Event::Stop
				} else {
					return Ok(Parsed::Other);
				};
				Ok(Parsed::Record(Record { time, tid, event }))
			}
			_ => Ok(Parsed::Other),
		}
	}
}

/// What one record of a ring buffer holds.
#[derive(Debug, PartialEq)]
enum Parsed {
	Record(Record),
	Other,
}

/// Where the tracepoint `format` lays out `field`, in its record.
fn offset(format: &str, field: &str) -> Result<usize, String> {
	format
		.lines()
		.find_map(|line| {
			let (declared, rest) = line.trim().strip_prefix("field:")?.split_once(';')?;
			(declared.rsplit([' ', '*']).next() == Some(field)).then_some(rest)?;
			rest.trim().strip_prefix("offset:")?.split(';').next()?.parse().ok()
		})
		.ok_or_else(|| format!("a syscall tracepoint's format lays out no {field}"))
}

/// The waits `records` say the process's threads took, in time order, each from when its thread
/// left the CPU until it came back, or the window it left in closed: a wait that began before its
/// window, as an idle worker's parked since before it, is none of the window's.
fn account(records: &[Record]) -> Waits {
	/// A thread's state, as its records say.
	#[derive(Default)]
	struct Thread {
		/// Since when it is off the CPU, whether preempted, and the futex wait it is blocked in.
		off: Option<(u64, bool, Option<Site>)>,
		/// The futex wait it entered and has not returned from.
		waiting: Option<Site>,
	}
	fn close(waits: &mut Waits, (since, preempted, site): (u64, bool, Option<Site>), until: u64) {
		let span = until.saturating_sub(since);
		if preempted {
			waits.preempted += span;
			return;
		}
		waits.blocked += span;
		if let Some(site) = site {
			*waits.futex.entry(site).or_default() += span;
		}
	}
	let mut waits = Waits::default();
	let mut threads: HashMap<u32, Thread> = HashMap::new();
	for record in records {
		if record.event == Event::Stop {
			for thread in threads.values_mut() {
				if let Some(off) = thread.off.take() {
					close(&mut waits, off, record.time);
				}
			}
			threads.clear();
			continue;
		}
		let thread = threads.entry(record.tid).or_default();
		match record.event {
			Event::Out { preempted } => {
				let site = thread.waiting.filter(|_| !preempted);
				thread.off = Some((record.time, preempted, site));
			}
			Event::In => {
				if let Some(off) = thread.off.take() {
					close(&mut waits, off, record.time);
				}
			}
			Event::Enter(site) => thread.waiting = site,
			Event::Exit => thread.waiting = None,
			Event::Stop => unreachable!("handled above"),
		}
	}
	waits
}

/// Each of `samples`' waits, a call's share over all their calls, within the least and most one
/// sample's calls shared, since the scheduler moves them: blocked, preempted, futex, and lock
/// wait, a futex wait at one of `locks` having waited to take a lock.
pub fn measures(samples: &[(Waits, u64)], locks: &BTreeSet<Site>) -> Vec<Measure> {
	let kinds: [(&str, fn(&Waits, &BTreeSet<Site>) -> u64); 4] = [
		("blocked time", |waits, _| waits.blocked),
		("preempted time", |waits, _| waits.preempted),
		("futex wait time", |waits, _| waits.futex.values().sum()),
		("lock wait time", |waits, locks| {
			waits
				.futex
				.iter()
				.filter(|(site, _)| locks.contains(site))
				.map(|(_, ns)| ns)
				.sum()
		}),
	];
	let calls: u64 = samples.iter().map(|(_, calls)| calls).sum();
	if calls == 0 {
		return Vec::new();
	}
	kinds
		.into_iter()
		.map(|(name, of)| {
			let total: u64 = samples.iter().map(|(waits, _)| of(waits, locks)).sum();
			let (least, most) = samples
				.iter()
				.filter(|(_, calls)| *calls > 0)
				.map(|(waits, calls)| of(waits, locks) as f64 / *calls as f64)
				.fold((f64::INFINITY, 0.0f64), |(least, most), per| (least.min(per), most.max(per)));
			Measure {
				spread: Some((least, most)),
				..Measure::new(name, total as f64 / calls as f64, "ns")
			}
		})
		.collect()
}

/// The sites of `sites` that waited to take a lock, given the process's `/proc/<pid>/maps`: a
/// priority-inheriting lock's, or one whose code is a lock's, as the function its call returns to
/// names it where the call went through a generic wrapper such as libc's `syscall`.
pub fn locks<'a>(sites: impl IntoIterator<Item = &'a Site>, maps: &str) -> Result<BTreeSet<Site>, String> {
	let mappings = crate::profile::mappings(maps)?;
	let mut objects: HashMap<String, Option<crate::profile::Object>> = HashMap::new();
	let mut names = |ip: u64| -> Vec<String> {
		let Some(mapping) = crate::profile::mapped(&mappings, ip) else {
			return Vec::new();
		};
		let object = objects
			.entry(mapping.path.clone())
			.or_insert_with(|| crate::profile::Object::load(&mapping.path));
		object
			.as_ref()
			.map(|object| object.names(ip - mapping.start + mapping.offset))
			.unwrap_or_default()
	};
	let mut locks = BTreeSet::new();
	for site in sites {
		let mut at = names(site.ip);
		// The call returns into the code that made it, which a return address names one byte in.
		if at.iter().any(|name| name == "syscall")
			&& let Some(ret) = site.ret
		{
			at = names(ret - 1);
		}
		if site.pi || at.iter().any(|name| takes_lock(name)) {
			locks.insert(*site);
		}
	}
	Ok(locks)
}

/// Whether the function `name` waits to take a lock: Rust's standard library's mutex or
/// reader-writer lock, once contended, or glibc's low-level lock, which its mutexes and its own
/// locks wait on, or a mutex's or reader-writer lock's of glibc's that waits itself.
fn takes_lock(name: &str) -> bool {
	// An inherent method's path, as `<std::sys::sync::mutex::futex::Mutex>::lock_contended`.
	let name = name.replace(['<', '>'], "");
	if name.starts_with("std::") {
		return ["Mutex::lock_contended", "RwLock::read_contended", "RwLock::write_contended"]
			.iter()
			.any(|function| name.ends_with(function));
	}
	let name = name.trim_start_matches('_');
	name.starts_with("lll_lock_wait")
		|| (name.starts_with("pthread_mutex_") && name.contains("lock") && !name.contains("unlock"))
		|| (name.starts_with("pthread_rwlock_") && (name.contains("rdlock") || name.contains("wrlock")))
}

#[cfg(target_os = "linux")]
pub use linux::Rings;

#[cfg(target_os = "linux")]
mod linux {
	use super::{Drained, MOST_RECORD, Parsed, Tracing};
	use crate::perf::redirect;
	use crate::profile::Ring;

	/// The ring buffers of a process's trace, one a CPU, and every event they hold the records of.
	pub struct Rings {
		rings: Vec<Ring>,
		/// The events beside each ring's leader, open until the trace ends.
		_events: Vec<std::os::fd::OwnedFd>,
		tracing: Tracing,
		/// The process whose threads it traces.
		pid: u32,
	}

	impl Rings {
		/// The rings of `fds`, the events the process `pid` handed over, [`Tracing::EVENTS`] a CPU.
		pub fn new(tracing: &Tracing, fds: Vec<std::os::fd::OwnedFd>, pid: u32) -> Result<Self, String> {
			let mut rings = Vec::new();
			let mut events = Vec::new();
			let mut fds = fds.into_iter();
			while let Some(leader) = fds.next() {
				let ring = Ring::map(leader)?;
				for event in fds.by_ref().take(Tracing::EVENTS - 1) {
					redirect(&event, ring.fd())?;
					events.push(event);
				}
				rings.push(ring);
			}
			Ok(Self {
				rings,
				_events: events,
				tracing: tracing.clone(),
				pid,
			})
		}

		/// Every record since the last drain, in time order across the CPUs.
		pub fn drain(&mut self) -> Result<Drained, String> {
			let mut drained = Drained::default();
			for ring in &mut self.rings {
				let filled = ring.records(MOST_RECORD, |kind, misc, record| {
					match self.tracing.parse(kind, misc, record, self.pid)? {
						Parsed::Record(record) => drained.records.push(record),
						Parsed::Other => {}
					}
					Ok(())
				})?;
				drained.filled += u64::from(filled);
			}
			drained.records.sort_by_key(|record| record.time);
			Ok(drained)
		}
	}
}

/// The flags of `perf_event_attr` a trace sets past those a count does.
const SAMPLE_ID_ALL: u64 = 1 << 18;
const USE_CLOCKID: u64 = 1 << 25;
const CONTEXT_SWITCH: u64 = 1 << 26;
const PERF_TYPE_TRACEPOINT: u32 = 2;
/// The software event that counts nothing, which carries a trace's context switches.
const PERF_COUNT_SW_DUMMY: u64 = 9;
const PERF_SAMPLE_TIME: u64 = 1 << 2;
const PERF_SAMPLE_RAW: u64 = 1 << 10;
const PERF_SAMPLE_STACK_USER: u64 = 1 << 13;
const CLOCK_MONOTONIC: i32 = 1;
const EPERM: i32 = 1;
const EACCES: i32 = 13;
const PERF_RECORD_SAMPLE: u32 = 9;
/// A switch on a CPU a whole-CPU event watches.
const PERF_RECORD_SWITCH_CPU_WIDE: u32 = 15;
const PERF_RECORD_MISC_SWITCH_OUT: u16 = 1 << 13;
/// A switch out of a thread still runnable: preempted, rather than asleep.
const PERF_RECORD_MISC_SWITCH_OUT_PREEMPT: u16 = 1 << 14;

#[cfg(test)]
mod tests {
	use super::*;

	fn record(time: u64, tid: u32, event: Event) -> Record {
		Record { time, tid, event }
	}

	const SITE: Site = Site {
		ip: 0x1000,
		ret: Some(0x2000),
		pi: false,
	};

	#[test]
	fn splits_off_cpu_time_into_blocked_preempted_and_futex_waits() {
		let out = Event::Out { preempted: false };
		let preempted = Event::Out { preempted: true };
		let records = [
			// A sleep, then a futex wait, then a preemption inside a futex call, which waits for a
			// CPU rather than the futex.
			record(100, 1, out),
			record(150, 1, Event::In),
			record(200, 1, Event::Enter(Some(SITE))),
			record(210, 1, out),
			record(310, 1, Event::In),
			record(320, 1, preempted),
			record(325, 1, Event::In),
			record(330, 1, Event::Exit),
			// A wake does not wait; the thread then sleeps outside any futex call.
			record(400, 1, Event::Enter(None)),
			record(405, 1, Event::Exit),
			record(410, 1, out),
			record(420, 1, Event::In),
			// Another thread, already off when the window opened, comes back: none of its wait is
			// the window's. It leaves again and is still off when the window closes.
			record(500, 2, Event::In),
			record(600, 2, Event::Enter(Some(SITE))),
			record(650, 2, out),
			record(1000, 1, Event::Stop),
			// The next window: the thread's switch back in, and its return, close nothing.
			record(2000, 2, Event::In),
			record(2010, 2, Event::Exit),
			record(2020, 2, out),
			record(2030, 2, Event::In),
			record(3000, 1, Event::Stop),
		];
		assert_eq!(
			account(&records),
			Waits {
				blocked: 50 + 100 + 10 + 350 + 10,
				preempted: 5,
				futex: BTreeMap::from([(SITE, 100 + 350)]),
			}
		);
		// A window left open, as a drain mid-window would leave one, closes nothing.
		assert_eq!(account(&records[..1]), Waits::default());
	}

	#[test]
	fn takes_a_call_a_share_of_each_wait_with_its_spread() {
		let lock = Site { pi: true, ..SITE };
		let samples = [
			(
				Waits {
					blocked: 400,
					preempted: 0,
					futex: BTreeMap::from([(SITE, 100), (lock, 200)]),
				},
				2,
			),
			(
				Waits {
					blocked: 800,
					preempted: 40,
					futex: BTreeMap::from([(lock, 600)]),
				},
				2,
			),
			(Waits::default(), 4),
		];
		let measures = measures(&samples, &BTreeSet::from([lock]));
		let shown: Vec<(&str, f64, (f64, f64))> = measures
			.iter()
			.map(|m| (m.name.as_str(), m.value, m.spread.unwrap()))
			.collect();
		assert_eq!(
			shown,
			[
				("blocked time", 150.0, (0.0, 400.0)),
				("preempted time", 5.0, (0.0, 20.0)),
				("futex wait time", 112.5, (0.0, 300.0)),
				("lock wait time", 100.0, (0.0, 300.0)),
			]
		);
		assert!(super::measures(&[], &BTreeSet::new()).is_empty());
	}

	#[test]
	fn knows_a_lock_by_the_function_that_waits() {
		for name in [
			"<std::sys::sync::mutex::futex::Mutex>::lock_contended",
			"std::sys::sync::rwlock::futex::RwLock::write_contended",
			"std::sys::pal::unix::locks::futex_rwlock::RwLock::read_contended",
			"__lll_lock_wait",
			"__lll_lock_wait_private",
			"___pthread_mutex_lock",
			"__pthread_mutex_cond_lock",
			"pthread_mutex_clocklock",
			"pthread_rwlock_wrlock",
		] {
			assert!(takes_lock(name), "{name}");
		}
		for name in [
			"std::sys::pal::unix::futex::futex_wait",
			"<std::thread::thread::Thread>::park",
			"<my::Mutex>::lock_contended",
			"std::sys::sync::condvar::futex::Condvar::wait",
			"pthread_mutex_unlock",
			"pthread_rwlock_unlock",
			"pthread_cond_wait",
			"my::Mutex::lock_contended",
			"syscall",
		] {
			assert!(!takes_lock(name), "{name}");
		}
	}

	#[test]
	fn finds_a_field_in_a_tracepoints_format() {
		let format = "name: sys_enter_futex\nID: 499\nformat:\n\
			\tfield:unsigned short common_type;\toffset:0;\tsize:2;\tsigned:0;\n\
			\tfield:u32 * uaddr;\toffset:16;\tsize:8;\tsigned:0;\n\
			\tfield:int op;\toffset:24;\tsize:8;\tsigned:0;\n\
			\tfield:u32 * uaddr2;\toffset:48;\tsize:8;\tsigned:0;\n";
		assert_eq!(offset(format, "op"), Ok(24));
		assert_eq!(offset(format, "uaddr"), Ok(16));
		assert!(offset(format, "option").is_err());
	}

	/// A trace whose tracepoints have these IDs, and lay out their fields as Linux's do.
	fn tracing() -> Tracing {
		Tracing {
			enter: 499,
			exit: 498,
			prctl: 199,
			op: 24,
			option: 16,
		}
	}

	/// A sample of the tracepoint `id`, of thread 7 of process 5 at `time`, with `args` after the
	/// syscall's number, and `stack` copied from the user stack.
	fn sample(id: u16, time: u64, args: &[u64], stack: Option<u64>) -> Vec<u8> {
		let mut raw = vec![0u8; 16];
		raw[0..2].copy_from_slice(&id.to_ne_bytes());
		for arg in args {
			raw.extend(arg.to_ne_bytes());
		}
		// The kernel pads the raw record so it and its size end on a word.
		while (raw.len() + 4) % 8 != 0 {
			raw.push(0);
		}
		let mut record = vec![0u8; 8];
		record.extend(0xabcu64.to_ne_bytes());
		record.extend(5u32.to_ne_bytes());
		record.extend(7u32.to_ne_bytes());
		record.extend(time.to_ne_bytes());
		record.extend((raw.len() as u32).to_ne_bytes());
		record.extend(raw);
		match stack {
			Some(word) => {
				record.extend(8u64.to_ne_bytes());
				record.extend(word.to_ne_bytes());
				record.extend(8u64.to_ne_bytes());
			}
			None => record.extend(0u64.to_ne_bytes()),
		}
		record
	}

	#[test]
	fn reads_switches_and_futex_and_prctl_calls() {
		let tracing = tracing();
		let parse = |kind: u32, misc: u16, record: &[u8]| tracing.parse(kind, misc, record, 5).unwrap();
		let at = |time: u64, tid: u32, event: Event| Parsed::Record(Record { time, tid, event });
		// FUTEX_WAIT_BITSET | FUTEX_PRIVATE_FLAG, a wait; FUTEX_WAKE_PRIVATE, none; FUTEX_LOCK_PI.
		assert_eq!(
			parse(PERF_RECORD_SAMPLE, 0, &sample(499, 10, &[0x7f00, 9 | 128], Some(0x55))),
			at(
				10,
				7,
				Event::Enter(Some(Site {
					ip: 0xabc,
					ret: Some(0x55),
					pi: false
				}))
			)
		);
		assert_eq!(
			parse(PERF_RECORD_SAMPLE, 0, &sample(499, 11, &[0x7f00, 1 | 128], None)),
			at(11, 7, Event::Enter(None))
		);
		assert_eq!(
			parse(PERF_RECORD_SAMPLE, 0, &sample(499, 12, &[0x7f00, 6], None)),
			at(
				12,
				7,
				Event::Enter(Some(Site {
					ip: 0xabc,
					ret: None,
					pi: true
				}))
			)
		);
		assert_eq!(
			parse(PERF_RECORD_SAMPLE, 0, &sample(498, 13, &[0], Some(0x55))),
			at(13, 7, Event::Exit)
		);
		// The harness stopping its events closes a window; another prctl does nothing.
		assert_eq!(
			parse(PERF_RECORD_SAMPLE, 0, &sample(199, 14, &[31, 0, 0, 0, 0], Some(1))),
			at(14, 7, Event::Stop)
		);
		assert_eq!(
			parse(PERF_RECORD_SAMPLE, 0, &sample(199, 15, &[32, 0, 0, 0, 0], Some(1))),
			Parsed::Other
		);
		let switch = |pid: u32, time: u64| {
			let mut record = vec![0u8; 8];
			// The thread on the switch's other side, another process's.
			record.extend(9u32.to_ne_bytes());
			record.extend(10u32.to_ne_bytes());
			record.extend(pid.to_ne_bytes());
			record.extend(8u32.to_ne_bytes());
			record.extend(time.to_ne_bytes());
			record
		};
		assert_eq!(
			parse(
				PERF_RECORD_SWITCH_CPU_WIDE,
				PERF_RECORD_MISC_SWITCH_OUT | PERF_RECORD_MISC_SWITCH_OUT_PREEMPT,
				&switch(5, 20)
			),
			at(20, 8, Event::Out { preempted: true })
		);
		assert_eq!(
			parse(PERF_RECORD_SWITCH_CPU_WIDE, PERF_RECORD_MISC_SWITCH_OUT, &switch(5, 21)),
			at(21, 8, Event::Out { preempted: false })
		);
		assert_eq!(parse(PERF_RECORD_SWITCH_CPU_WIDE, 0, &switch(5, 22)), at(22, 8, Event::In));
		// Another process's.
		assert_eq!(parse(PERF_RECORD_SWITCH_CPU_WIDE, 0, &switch(6, 23)), Parsed::Other);
		assert!(tracing.parse(PERF_RECORD_SWITCH_CPU_WIDE, 0, &[0u8; 12], 5).is_err());
	}
}


