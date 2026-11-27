//! The physical core a `bench` run times on, leased from every other swole run on the host.
//!
//! A run leases one physical core with an open file description lock on a file in [`LEASES`],
//! which the kernel drops however the run ends, and times every benchmark on the core's first
//! hardware thread. No other swole run uses any of the core's threads while the lease is held, so
//! two runs never share a core or contend through its SMT sibling. The lease keeps other swole
//! runs off the core, not the host's other processes: only the kernel's isolated CPUs keep those
//! off, which the run says.
//!
//! Where swole may write the core's cpufreq policy, it switches it to the `performance` governor
//! for the run, recording what it changed first in a journal beside the lease that only the run's
//! user may write, and restores it on exit or on SIGINT, SIGTERM, or SIGHUP; the next run of that
//! user to lease the core restores what a run killed outright left behind.

use std::fs::File;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::perf;

/// Where every swole run on the host leases cores: a tmpfs every user may write, which a reboot
/// empties as it resets every governor.
const LEASES: &str = "/dev/shm/swole";

/// Where the kernel describes each CPU.
const SYS: &str = "/sys/devices/system/cpu";

/// The governor a run times under, where swole may set it.
const PERFORMANCE: &str = "performance";

/// The files of a cpufreq policy swole changes, restored in this order: the governor first, since
/// a driver such as `amd-pstate-epp` refuses an energy preference under `performance`.
const TUNED: [&str; 2] = ["scaling_governor", "energy_performance_preference"];

/// The physical core a run holds, and the CPU it times on.
pub struct Reservation {
	cpu: usize,
	/// Every hardware thread of its core, which no other swole run uses meanwhile.
	core: Vec<usize>,
	/// Every core the run may time on, its own among them, each its hardware threads.
	cores: Vec<Vec<usize>>,
	/// The CPUs swole may run on, which a spread stays within though it leases whole cores.
	candidates: Vec<usize>,
	/// Where the leases of `cores` are.
	leases: PathBuf,
	/// Whether the kernel keeps the CPU out of scheduler load balancing (`isolcpus`).
	isolated: bool,
	governor: Governor,
	/// What to write back on drop, before the lease's lock is released.
	restore: Option<Box<Restore>>,
	#[expect(dead_code, reason = "held for its lock, released after the restore")]
	lease: File,
}

/// The CPUs a threaded benchmark runs on, and the leases of the cores it holds beyond the run's own.
pub struct Spread {
	pub cpus: Vec<usize>,
	leases: Vec<File>,
}

/// The cpufreq governor a run times under.
enum Governor {
	/// The CPU has no cpufreq policy, as in many VMs.
	Absent,
	/// The host's own, and why swole left it.
	Kept { governor: String, why: String },
	/// `performance`, which swole set for the run in place of `was`.
	Set { was: String },
}

impl Reservation {
	/// The CPU every benchmark process runs on.
	pub fn cpu(&self) -> usize {
		self.cpu
	}

	/// The governor active while the run times, as the fingerprint names it.
	pub fn governor(&self) -> &str {
		match &self.governor {
			Governor::Absent => "none",
			Governor::Kept { governor, .. } => governor,
			Governor::Set { .. } => PERFORMANCE,
		}
	}

	/// The CPUs a benchmark that runs other threads spreads over instead, since one CPU would
	/// serialize them: its own core's, and every other core no other swole run leases now, each
	/// leased until the spread drops.
	pub fn spread(&self) -> Result<Spread, String> {
		let mut spread = Spread {
			cpus: self.core.clone(),
			leases: Vec::new(),
		};
		for core in self.cores.iter().filter(|core| **core != self.core) {
			let file = open(&self.leases, core)?;
			if lock(&file).map_err(|e| format!("cannot lease core {}: {e}", core[0]))? {
				spread.cpus.extend(core);
				spread.leases.push(file);
			}
		}
		spread.cpus.retain(|cpu| self.candidates.contains(cpu));
		spread.cpus.sort_unstable();
		Ok(spread)
	}

	/// What the run holds, in a line.
	pub fn describe(&self) -> String {
		let others = if self.isolated {
			"which the kernel keeps out of scheduler load balancing"
		} else {
			"which other processes may still share"
		};
		let governor = match &self.governor {
			Governor::Absent => "no cpufreq governor".to_owned(),
			Governor::Kept { governor, why } => format!("governor {governor}, {why}"),
			Governor::Set { was } => format!("governor {PERFORMANCE} until swole exits, in place of {was}"),
		};
		format!(
			"timing on CPU {}, its core (CPUs {}) leased from other swole runs, {others}; {governor}",
			self.cpu,
			perf::list(&self.core)
		)
	}
}

impl Drop for Reservation {
	fn drop(&mut self) {
		if let Some(restore) = self.restore.take() {
			signals::disarm();
			if let Err(err) = restore.apply() {
				eprintln!("swole: {err}");
			}
		}
	}
}

/// Leases a physical core of `pmu`'s CPUs, or without counters of the performance cores swole may
/// run on, waiting for one when other swole runs hold them all, and sets its governor where
/// swole may.
pub fn reserve(pmu: Option<&perf::Pmu>) -> Result<Option<Reservation>, String> {
	let candidates = match pmu {
		Some(pmu) => pmu.cpus.clone(),
		None => performance()?,
	};
	reserve_in(Path::new(SYS), Path::new(LEASES), &candidates).map(Some)
}

/// The CPUs swole may run on, only the performance cores' on a hybrid Intel host that lets it run
/// on one.
fn performance() -> Result<Vec<usize>, String> {
	let allowed = perf::allowed()?;
	let path = Path::new(perf::PMUS).join(perf::PERFORMANCE).join("cpus");
	let Ok(list) = std::fs::read_to_string(&path) else {
		return Ok(allowed);
	};
	let cores: Vec<usize> = perf::cpus(&list)
		.map_err(|err| format!("{}: {err}", path.display()))?
		.into_iter()
		.filter(|cpu| allowed.contains(cpu))
		.collect();
	Ok(if cores.is_empty() { allowed } else { cores })
}

/// [`reserve`] with the kernel's CPUs described under `sys` and leases in `leases`.
fn reserve_in(sys: &Path, leases: &Path, candidates: &[usize]) -> Result<Reservation, String> {
	let cores = cores(sys, candidates)?;
	let (core, lease) = lease(leases, &cores)?;
	let journal = leases.join(format!("core{}.restore", core[0]));
	recover(sys, &core, &journal)?;
	let cpu = *core
		.iter()
		.find(|cpu| candidates.contains(cpu))
		.expect("a core holds a candidate");
	let isolated = match std::fs::read_to_string(sys.join("isolated")) {
		Ok(list) => perf::cpus(&list)?.contains(&cpu),
		Err(_) => false,
	};
	let (governor, restore) = tune(sys, &core, cpu, &journal)?;
	Ok(Reservation {
		cpu,
		core,
		cores,
		candidates: candidates.to_owned(),
		leases: leases.to_owned(),
		isolated,
		governor,
		restore,
		lease,
	})
}

/// The physical cores of `candidates`, each its hardware threads, the last core first, so a run
/// leaves the first, where the kernel does most of its own work, for last.
fn cores(sys: &Path, candidates: &[usize]) -> Result<Vec<Vec<usize>>, String> {
	let mut cores: Vec<Vec<usize>> = Vec::new();
	for &cpu in candidates {
		let path = sys.join(format!("cpu{cpu}/topology/thread_siblings_list"));
		let list = std::fs::read_to_string(&path).map_err(|e| format!("cannot read {}: {e}", path.display()))?;
		let mut threads = perf::cpus(&list).map_err(|err| format!("{}: {err}", path.display()))?;
		threads.sort_unstable();
		if !threads.contains(&cpu) {
			return Err(format!("{} leaves out CPU {cpu} itself", path.display()));
		}
		if !cores.contains(&threads) {
			cores.push(threads);
		}
	}
	if cores.is_empty() {
		return Err("swole may run on no CPU to time on".to_owned());
	}
	cores.sort_by_key(|threads| std::cmp::Reverse(threads[0]));
	Ok(cores)
}

/// Leases the first of `cores` no other swole run holds, trying them all again until one is free
/// when other runs hold every one: the core, and its lease's file, locked.
fn lease(leases: &Path, cores: &[Vec<usize>]) -> Result<(Vec<usize>, File), String> {
	shared(leases)?;
	let mut pause = Duration::from_millis(10);
	loop {
		for core in cores {
			let file = open(leases, core)?;
			if lock(&file).map_err(|e| format!("cannot lease core {}: {e}", core[0]))? {
				return Ok((core.clone(), file));
			}
		}
		// No lock waits on any one of several, and a run may free any core, even one another
		// run's threaded benchmark holds only for a while.
		if pause == Duration::from_millis(10) {
			eprintln!("swole: other swole runs lease every core swole may time on, so waiting for one");
		}
		std::thread::sleep(pause);
		pause = (pause * 2).min(Duration::from_secs(1));
	}
}

/// Takes the write lock of all of `file` for its open file description, which every descriptor of
/// it shares and the kernel releases with the last, unless another holds it: whether it was free.
fn lock(file: &File) -> std::io::Result<bool> {
	/// `struct flock`, which an open file description's lock takes with `l_pid` 0.
	#[repr(C)]
	struct Flock {
		l_type: i16,
		l_whence: i16,
		l_start: i64,
		l_len: i64,
		l_pid: i32,
	}
	const F_OFD_SETLK: i32 = 37;
	const F_WRLCK: i16 = 1;
	const EAGAIN: i32 = 11;
	const EACCES: i32 = 13;
	const EINTR: i32 = 4;
	unsafe extern "C" {
		fn fcntl(fd: i32, command: i32, ...) -> i32;
	}
	loop {
		let mut flock = Flock {
			l_type: F_WRLCK,
			l_whence: 0,
			l_start: 0,
			l_len: 0,
			l_pid: 0,
		};
		// SAFETY: a descriptor of `file`, and a `struct flock` the call reads and may write.
		let result = unsafe {
			fcntl(
				std::os::fd::AsRawFd::as_raw_fd(file),
				F_OFD_SETLK,
				&mut flock as *mut Flock,
			)
		};
		if result == 0 {
			return Ok(true);
		}
		let err = std::io::Error::last_os_error();
		match err.raw_os_error() {
			Some(EAGAIN | EACCES) => return Ok(false),
			Some(EINTR) => {}
			_ => return Err(err),
		}
	}
}

/// Creates `dir` for every user's leases, as `/tmp` is shared, when no run has yet.
fn shared(dir: &Path) -> Result<(), String> {
	use std::os::unix::fs::PermissionsExt;
	match std::fs::create_dir(dir) {
		Ok(()) => std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o1777))
			.map_err(|e| format!("cannot share {}: {e}", dir.display())),
		Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
			let meta = std::fs::symlink_metadata(dir).map_err(|e| format!("cannot read {}: {e}", dir.display()))?;
			if !meta.is_dir() {
				return Err(format!("{} is no directory to lease cores in", dir.display()));
			}
			Ok(())
		}
		Err(e) => Err(format!("cannot create {}: {e}", dir.display())),
	}
}

/// Opens the lease's file of `core`, named by its first thread, which every user may lock.
fn open(dir: &Path, core: &[usize]) -> Result<File, String> {
	use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
	let path = dir.join(format!("core{}", core[0]));
	let failed = |e: std::io::Error| format!("cannot open {}: {e}", path.display());
	loop {
		match File::options().read(true).write(true).open(&path) {
			Ok(file) => return Ok(file),
			Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
			Err(e) => return Err(failed(e)),
		}
		// Opened without creating above, since a sticky directory refuses to create over another
		// user's file; created only where none is, so a race with another run retries the open.
		match File::options()
			.read(true)
			.write(true)
			.create_new(true)
			.mode(0o666)
			.open(&path)
		{
			Ok(file) => {
				file.set_permissions(std::fs::Permissions::from_mode(0o666))
					.map_err(failed)?;
				return Ok(file);
			}
			Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
			Err(e) => return Err(failed(e)),
		}
	}
}

/// What swole changed in cpufreq policies, each file and what it held, and the journal recording
/// them, which a signal handler restores through.
struct Restore {
	entries: Vec<(std::ffi::CString, Vec<u8>)>,
	journal: std::ffi::CString,
}

impl Restore {
	/// The record a journal holds: a line a file, its path and what it held.
	fn record(&self) -> String {
		self.entries
			.iter()
			.map(|(path, value)| {
				format!(
					"{}\t{}\n",
					path.to_str().expect("a sysfs path is UTF-8"),
					std::str::from_utf8(value).expect("a governor is UTF-8")
				)
			})
			.collect()
	}

	/// Writes each file back, then removes the journal, which stays for the next run on failure.
	fn apply(&self) -> Result<(), String> {
		for (path, value) in &self.entries {
			let path = Path::new(path.to_str().expect("a sysfs path is UTF-8"));
			std::fs::write(path, value).map_err(|e| {
				format!(
					"cannot restore {} to {}: {e}; the next swole run to lease its core retries",
					path.display(),
					String::from_utf8_lossy(value)
				)
			})?;
		}
		let journal = Path::new(self.journal.to_str().expect("a journal's path is UTF-8"));
		std::fs::remove_file(journal).map_err(|e| format!("cannot remove {}: {e}", journal.display()))
	}
}

/// Restores what `journal` says a run killed outright left on `core`, before this run reads the
/// policy as the host's.
fn recover(sys: &Path, core: &[usize], journal: &Path) -> Result<(), String> {
	use std::os::unix::fs::MetadataExt;
	unsafe extern "C" {
		fn geteuid() -> u32;
	}
	let read = |e: std::io::Error| format!("cannot read {}: {e}", journal.display());
	// SAFETY: takes no arguments.
	let euid = unsafe { geteuid() };
	// Only a journal the run's own user wrote is replayed, so no other user steers what it writes,
	// and the run can remove what it replays. The owner is checked before opening, since another
	// user's journal may not be readable.
	let owner = match std::fs::symlink_metadata(journal) {
		Ok(meta) if meta.is_file() => meta.uid(),
		Ok(_) => return Err(format!("{} is no journal swole wrote", journal.display())),
		Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
		Err(e) => return Err(read(e)),
	};
	if owner != euid {
		eprintln!(
			"swole: another user's swole run was killed with core {}'s governor changed, which its journal {} \
			 records and that user's next run restores",
			core[0],
			journal.display()
		);
		return Ok(());
	}
	// Whoever owns the lease directory may swap the journal since, so the opened file must still be
	// the run's own.
	let mut file = File::open(journal).map_err(read)?;
	if file.metadata().map_err(read)?.uid() != euid {
		return Err(format!("{} changed owner while swole read it", journal.display()));
	}
	let mut record = String::new();
	std::io::Read::read_to_string(&mut file, &mut record).map_err(read)?;
	// A policy shared with another core is one tuning never changes, as another run may time there.
	let mut policies = Vec::new();
	for policy in policies_of(sys, core)? {
		let related = words(&std::fs::read_to_string(policy.join("related_cpus")).map_err(read)?)?;
		if related.iter().all(|cpu| core.contains(cpu)) {
			policies.push(policy);
		}
	}
	let mut entries = Vec::new();
	for line in record.lines() {
		let (path, value) = line
			.split_once('\t')
			.ok_or_else(|| format!("{} records {line:?}, no file and value", journal.display()))?;
		let path = Path::new(path);
		let tuned = path
			.parent()
			.is_some_and(|policy| policies.iter().any(|own| own == policy))
			&& path
				.file_name()
				.and_then(|name| name.to_str())
				.is_some_and(|name| TUNED.contains(&name));
		let word = !value.is_empty() && value.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_');
		if !tuned || !word {
			return Err(format!(
				"{} records {line:?}, which swole never changes",
				journal.display()
			));
		}
		entries.push((cstring(path), value.as_bytes().to_vec()));
	}
	let restore = Restore {
		entries,
		journal: cstring(journal),
	};
	// One swole may write a policy another may not; the run then times under what it finds.
	match restore.apply() {
		Ok(()) => eprintln!("swole: restored the cpufreq policy a killed swole run left changed"),
		Err(err) => eprintln!("swole: a killed swole run left a cpufreq policy changed, and {err}"),
	}
	Ok(())
}

/// Sets `performance` on every cpufreq policy of `core`, recording in `journal` what each held
/// first, where every one is the core's alone and swole may write it: the governor the run times
/// under on `cpu`, and what to restore.
fn tune(sys: &Path, core: &[usize], cpu: usize, journal: &Path) -> Result<(Governor, Option<Box<Restore>>), String> {
	let read = |path: &Path| {
		std::fs::read_to_string(path)
			.map(|text| text.trim().to_owned())
			.map_err(|e| format!("cannot read {}: {e}", path.display()))
	};
	let policies = policies_of(sys, core)?;
	if policies.is_empty() {
		return Ok((Governor::Absent, None));
	}
	// The timing CPU's own policy names the governor kept, since its siblings' may differ.
	let own = sys.join(format!("cpu{cpu}/cpufreq"));
	if !own.exists() {
		return Err(format!(
			"CPU {cpu} has no cpufreq policy though its core's other CPUs do"
		));
	}
	let governor = read(&own.join(TUNED[0]))?;
	let kept = |why: String| -> Result<(Governor, Option<Box<Restore>>), String> {
		Ok((
			Governor::Kept {
				governor: governor.clone(),
				why,
			},
			None,
		))
	};
	let mut entries = Vec::new();
	for policy in &policies {
		let related = words(&read(&policy.join("related_cpus"))?)?;
		if let Some(other) = related.iter().find(|cpu| !core.contains(cpu)) {
			return kept(format!(
				"since its cpufreq policy also sets CPU {other}, which another run may time on"
			));
		}
		let available = read(&policy.join("scaling_available_governors"))?;
		if !available.split_whitespace().any(|name| name == PERFORMANCE) {
			return kept(format!("since its cpufreq driver has no {PERFORMANCE} governor"));
		}
		for name in TUNED {
			let path = policy.join(name);
			if name != TUNED[0] && !path.exists() {
				continue;
			}
			let value = read(&path)?;
			if let Err(e) = File::options().write(true).open(&path) {
				return match e.kind() {
					std::io::ErrorKind::PermissionDenied | std::io::ErrorKind::ReadOnlyFilesystem => kept(format!(
						"since swole may not write {}: grant it as the README's Cores says",
						path.display()
					)),
					_ => Err(format!("cannot open {}: {e}", path.display())),
				};
			}
			entries.push((cstring(&path), value.into_bytes()));
		}
	}
	let governors: Vec<String> = policies
		.iter()
		.map(|policy| read(&policy.join(TUNED[0])))
		.collect::<Result<_, _>>()?;
	if governors.iter().all(|governor| governor == PERFORMANCE) {
		return kept("as the host set it".to_owned());
	}
	let restore = Box::new(Restore {
		entries,
		journal: cstring(journal),
	});
	// Recorded before anything changes, so a run killed at any point leaves what to restore, in a
	// file of the run's user alone, which other users of the shared directory cannot replace.
	let created = {
		use std::os::unix::fs::OpenOptionsExt;
		File::options().write(true).create_new(true).mode(0o644).open(journal)
	};
	let mut file = match created {
		Ok(file) => file,
		Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
			return kept(format!(
				"since another user's killed swole run left its governor changed, as {} records",
				journal.display()
			));
		}
		Err(e) => return Err(format!("cannot create {}: {e}", journal.display())),
	};
	file.write_all(restore.record().as_bytes())
		.map_err(|e| format!("cannot write {}: {e}", journal.display()))?;
	signals::arm(&restore);
	for policy in &policies {
		let path = policy.join(TUNED[0]);
		if let Err(e) = std::fs::write(&path, PERFORMANCE) {
			signals::disarm();
			let undone = restore.apply().err().map_or(String::new(), |err| format!("; {err}"));
			return Err(format!("cannot write {}: {e}{undone}", path.display()));
		}
	}
	Ok((Governor::Set { was: governor }, Some(restore)))
}

/// The cpufreq policies of `core`'s CPUs, each its directory once, none for a CPU without one.
fn policies_of(sys: &Path, core: &[usize]) -> Result<Vec<PathBuf>, String> {
	let mut policies: Vec<PathBuf> = Vec::new();
	for cpu in core {
		let dir = sys.join(format!("cpu{cpu}/cpufreq"));
		if !dir.exists() {
			continue;
		}
		let policy = dir
			.canonicalize()
			.map_err(|e| format!("cannot read {}: {e}", dir.display()))?;
		if !policies.contains(&policy) {
			policies.push(policy);
		}
	}
	Ok(policies)
}

/// The CPUs a cpufreq list such as `related_cpus` names, which separates them by spaces.
fn words(list: &str) -> Result<Vec<usize>, String> {
	list.split_whitespace()
		.map(|cpu| cpu.parse().map_err(|_| format!("{list:?} is no cpufreq CPU list")))
		.collect()
}

fn cstring(path: &Path) -> std::ffi::CString {
	std::ffi::CString::new(path.as_os_str().as_encoded_bytes()).expect("a sysfs path holds no NUL")
}

/// Restores a run's governor when a signal ends it, which unwinds nothing.
mod signals {
	use std::sync::atomic::{AtomicPtr, Ordering};

	use super::Restore;

	/// What the handler restores, or null once nothing is left to.
	static ARMED: AtomicPtr<Restore> = AtomicPtr::new(std::ptr::null_mut());

	const SIGHUP: i32 = 1;
	const SIGINT: i32 = 2;
	const SIGTERM: i32 = 15;
	const SIG_DFL: usize = 0;
	const O_WRONLY: i32 = 1;
	const O_CLOEXEC: i32 = 0o2_000_000;

	unsafe extern "C" {
		fn signal(signum: i32, handler: usize) -> usize;
		fn raise(signum: i32) -> i32;
		fn open(path: *const std::ffi::c_char, flags: i32, ...) -> i32;
		fn write(fd: i32, buf: *const u8, count: usize) -> isize;
		fn close(fd: i32) -> i32;
		fn unlink(path: *const std::ffi::c_char) -> i32;
	}

	/// Has the handler restore `restore`, which outlives the handler's use by [`disarm`] first.
	pub fn arm(restore: &Restore) {
		let previous = ARMED.swap((restore as *const Restore).cast_mut(), Ordering::SeqCst);
		assert!(previous.is_null(), "a run reserves one core");
		static INSTALL: std::sync::Once = std::sync::Once::new();
		INSTALL.call_once(|| {
			for signum in [SIGHUP, SIGINT, SIGTERM] {
				// SAFETY: the handler makes only async-signal-safe calls.
				unsafe { signal(signum, handle as extern "C" fn(i32) as usize) };
			}
		});
	}

	/// Takes the restore back, once the handler can no longer run it.
	pub fn disarm() {
		ARMED.store(std::ptr::null_mut(), Ordering::SeqCst);
	}

	/// Writes back each file, removes the journal if all took, then dies of `signum` as
	/// it would have.
	extern "C" fn handle(signum: i32) {
		let restore = ARMED.swap(std::ptr::null_mut(), Ordering::SeqCst);
		// SAFETY: armed only while the restore it points to is alive; a drop disarms it first.
		if let Some(restore) = unsafe { restore.as_ref() } {
			let mut restored = true;
			for (path, value) in &restore.entries {
				// SAFETY: a NUL-terminated path, and a buffer of the length given.
				unsafe {
					let fd = open(path.as_ptr(), O_WRONLY | O_CLOEXEC);
					restored &= fd >= 0 && write(fd, value.as_ptr(), value.len()) == value.len() as isize;
					if fd >= 0 {
						close(fd);
					}
				}
			}
			if restored {
				// SAFETY: a NUL-terminated path.
				unsafe { unlink(restore.journal.as_ptr()) };
			}
		}
		// SAFETY: restores the default action, which the raised signal then takes on return.
		unsafe {
			signal(signum, SIG_DFL);
			raise(signum);
		}
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	fn scratch(name: &str) -> PathBuf {
		let dir = std::env::temp_dir()
			.join(format!("swole-cores-{}", std::process::id()))
			.join(name);
		let _ = std::fs::remove_dir_all(&dir);
		std::fs::create_dir_all(&dir).unwrap();
		dir
	}

	/// A sysfs of `threads` CPUs a core, `cores` cores, each CPU its own policy under `governor`.
	fn sys(dir: &Path, cores: usize, threads: usize, governor: &str) -> PathBuf {
		let sys = dir.join("cpu");
		let total = cores * threads;
		for cpu in 0..total {
			let siblings: Vec<String> = (0..threads).map(|t| (cpu % cores + t * cores).to_string()).collect();
			let topology = sys.join(format!("cpu{cpu}/topology"));
			std::fs::create_dir_all(&topology).unwrap();
			std::fs::write(topology.join("thread_siblings_list"), siblings.join(",") + "\n").unwrap();
			let policy = sys.join(format!("cpufreq/policy{cpu}"));
			std::fs::create_dir_all(&policy).unwrap();
			std::fs::write(policy.join("related_cpus"), format!("{cpu}\n")).unwrap();
			std::fs::write(policy.join("scaling_available_governors"), "performance powersave\n").unwrap();
			std::fs::write(policy.join("scaling_governor"), format!("{governor}\n")).unwrap();
			std::fs::write(policy.join("energy_performance_preference"), "balance_performance\n").unwrap();
			std::os::unix::fs::symlink(&policy, sys.join(format!("cpu{cpu}/cpufreq"))).unwrap();
		}
		sys
	}

	/// Held by a test that sets a governor, since the signal handler restores one run's alone.
	static ARMS: std::sync::Mutex<()> = std::sync::Mutex::new(());

	fn governor(sys: &Path, cpu: usize) -> String {
		std::fs::read_to_string(sys.join(format!("cpufreq/policy{cpu}/scaling_governor")))
			.unwrap()
			.trim()
			.to_owned()
	}

	#[test]
	fn groups_threads_into_cores_the_last_first() {
		let dir = scratch("topology");
		let sys = sys(&dir, 4, 2, "powersave");
		assert_eq!(
			cores(&sys, &(0..8).collect::<Vec<_>>()).unwrap(),
			[vec![3, 7], vec![2, 6], vec![1, 5], vec![0, 4]]
		);
		// A core with only its sibling allowed is still leased whole.
		assert_eq!(cores(&sys, &[5, 0]).unwrap(), [vec![1, 5], vec![0, 4]]);
		assert!(cores(&sys, &[]).is_err());
	}

	#[test]
	fn runs_lease_disjoint_cores_and_release_them() {
		let dir = scratch("lease");
		let sys = sys(&dir, 2, 2, "performance");
		let leases = dir.join("leases");
		let all: Vec<usize> = (0..4).collect();
		let first = reserve_in(&sys, &leases, &all).unwrap();
		let spread = first.spread().unwrap();
		assert_eq!(spread.cpus, all);
		// The spread holds the other core until it drops.
		assert!(!lock(&open(&leases, &[0, 2]).unwrap()).unwrap());
		drop(spread);
		let second = reserve_in(&sys, &leases, &all).unwrap();
		assert_eq!((first.cpu, &first.core), (1, &vec![1, 3]));
		assert_eq!((second.cpu, &second.core), (0, &vec![0, 2]));
		// A threaded benchmark spreads over its own run's core alone while the other is leased.
		assert_eq!(first.spread().unwrap().cpus, [1, 3]);
		assert_eq!(second.spread().unwrap().cpus, [0, 2]);
		drop(first);
		assert_eq!(second.spread().unwrap().cpus, all);
		let third = reserve_in(&sys, &leases, &all).unwrap();
		assert_eq!(third.cpu, 1);
		assert!(matches!(&third.governor, Governor::Kept { governor, .. } if governor == PERFORMANCE));
	}

	#[test]
	fn keeps_to_the_cpus_swole_may_run_on() {
		let dir = scratch("eligible");
		let sys = sys(&dir, 2, 2, "performance");
		// CPU 3's sibling 1 holds another governor, under a policy shared with another core, which
		// swole keeps but CPU 3 never times under.
		std::fs::write(sys.join("cpufreq/policy1/scaling_governor"), "powersave\n").unwrap();
		std::fs::write(sys.join("cpufreq/policy1/related_cpus"), "0 1\n").unwrap();
		let only = reserve_in(&sys, &dir.join("leases"), &[3, 2]).unwrap();
		assert_eq!((only.cpu, &only.core), (3, &vec![1, 3]));
		assert_eq!(only.spread().unwrap().cpus, [2, 3]);
		assert_eq!(only.governor(), PERFORMANCE);
	}

	#[test]
	fn sets_performance_and_restores_it_on_drop() {
		let _arms = ARMS.lock().unwrap();
		let dir = scratch("governor");
		let sys = sys(&dir, 2, 2, "powersave");
		let leases = dir.join("leases");
		let reservation = reserve_in(&sys, &leases, &[0, 1, 2, 3]).unwrap();
		assert!(matches!(&reservation.governor, Governor::Set { was } if was == "powersave"));
		assert_eq!(
			(governor(&sys, 1), governor(&sys, 3)),
			(PERFORMANCE.into(), PERFORMANCE.into())
		);
		assert_eq!(governor(&sys, 0), "powersave");
		let record = std::fs::read_to_string(leases.join("core1.restore")).unwrap();
		assert_eq!(record.lines().count(), 4, "{record}");
		drop(reservation);
		assert_eq!(
			(governor(&sys, 1), governor(&sys, 3)),
			("powersave".into(), "powersave".into())
		);
		assert!(!leases.join("core1.restore").exists());
	}

	#[test]
	fn the_next_run_restores_what_a_killed_one_left() {
		let _arms = ARMS.lock().unwrap();
		let dir = scratch("recover");
		let sys = sys(&dir, 1, 2, "powersave");
		let leases = dir.join("leases");
		let killed = reserve_in(&sys, &leases, &[0, 1]).unwrap();
		assert_eq!(governor(&sys, 0), PERFORMANCE);
		// Killed outright: the lock goes with its file, and nothing restores.
		let mut killed = killed;
		signals::disarm();
		killed.restore.take().unwrap();
		drop(killed);
		let next = reserve_in(&sys, &leases, &[0, 1]).unwrap();
		assert!(matches!(&next.governor, Governor::Set { was } if was == "powersave"));
		drop(next);
		assert_eq!(
			(governor(&sys, 0), governor(&sys, 1)),
			("powersave".into(), "powersave".into())
		);
	}

	#[test]
	fn recovers_only_a_tuned_file_to_a_word() {
		let dir = scratch("hostile");
		let sys = sys(&dir, 2, 1, "powersave");
		let leases = dir.join("leases");
		shared(&leases).unwrap();
		let policy = |cpu: usize| sys.join(format!("cpu{cpu}/cpufreq")).canonicalize().unwrap();
		for record in [
			"/etc/passwd\tperformance\n".to_owned(),
			format!("{}/../policy1/scaling_governor\tperformance\n", policy(0).display()),
			// Another core's policy, which another run may be timing on.
			format!("{}\tperformance\n", policy(0).join("scaling_governor").display()),
			format!("{}\tperformance\n", policy(1).join("scaling_max_freq").display()),
			format!("{}\tper formance\n", policy(1).join("scaling_governor").display()),
		] {
			// The run leases core 1 first.
			std::fs::write(leases.join("core1.restore"), &record).unwrap();
			let err = reserve_in(&sys, &leases, &[0, 1]).err().unwrap();
			assert!(err.contains("which swole never changes"), "{err}");
		}
	}

	#[test]
	fn keeps_a_policy_another_core_shares() {
		let dir = scratch("shared");
		let sys = sys(&dir, 2, 1, "powersave");
		std::fs::write(sys.join("cpufreq/policy1/related_cpus"), "0 1\n").unwrap();
		let reservation = reserve_in(&sys, &dir.join("leases"), &[0, 1]).unwrap();
		assert_eq!(reservation.cpu, 1);
		assert!(
			matches!(&reservation.governor, Governor::Kept { governor, why } if governor == "powersave" && why.contains("CPU 0"))
		);
		assert_eq!(governor(&sys, 1), "powersave");
	}
}
