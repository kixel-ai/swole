//! The physical core a `bench` run times on, leased from every other swole run on the host.
//!
//! A run leases one physical core with an advisory lock on a file in [`LEASES`], which the kernel
//! drops however the run ends, and times every benchmark on the core's first hardware thread. No
//! other swole run uses any of the core's threads while the lease is held, so two runs never share
//! a core or contend through its SMT sibling. The lease keeps other swole runs off the core, not
//! the host's other processes: only the kernel's isolated CPUs keep those off, which the run says.
//!
//! Where swole may write the core's cpufreq policy, it switches it to the `performance` governor
//! for the run, recording what it changed in the lease's file first, and restores it on exit or on
//! SIGINT, SIGTERM, or SIGHUP; the next run to lease the core restores what a run killed outright
//! left behind.

use std::fs::File;
use std::io::{Read, Seek, Write};
use std::path::{Path, PathBuf};

use crate::perf;

/// Where every swole run on the host leases cores: a tmpfs every user may write, which a reboot
/// empties as it resets every governor.
#[cfg(target_os = "linux")]
const LEASES: &str = "/dev/shm/swole";

/// Where the kernel describes each CPU.
#[cfg(target_os = "linux")]
const SYS: &str = "/sys/devices/system/cpu";

/// The governor a run times under, where swole may set it.
const PERFORMANCE: &str = "performance";

/// The files of a cpufreq policy swole changes, restored in this order: the governor first, since
/// a driver such as `amd-pstate-epp` refuses an energy preference under `performance`.
const TUNED: [&str; 2] = ["scaling_governor", "energy_performance_preference"];

/// The physical core a run holds, and the CPU it times on.
pub struct Reservation {
	/// The CPU every benchmark process runs on.
	pub cpu: usize,
	/// Every hardware thread of its core, which no other swole run uses meanwhile.
	pub core: Vec<usize>,
	/// The CPUs a benchmark that runs other threads spreads over instead, since one CPU would
	/// serialize them.
	pub spread: Vec<usize>,
	/// Whether the kernel keeps other processes off the CPU.
	pub isolated: bool,
	pub governor: Governor,
	/// What to write back on drop, before the lease's lock is released.
	restore: Option<Box<Restore>>,
	lease: File,
}

/// The cpufreq governor a run times under.
pub enum Governor {
	/// The CPU has no cpufreq policy, as in many VMs.
	Absent,
	/// The host's own, and why swole left it.
	Kept { governor: String, why: String },
	/// `performance`, which swole set for the run in place of `was`.
	Set { was: String },
}

impl Governor {
	/// The governor active while the run times, as the fingerprint names it.
	pub fn active(&self) -> &str {
		match self {
			Self::Absent => "none",
			Self::Kept { governor, .. } => governor,
			Self::Set { .. } => PERFORMANCE,
		}
	}
}

impl Reservation {
	/// What the run holds, in a line.
	pub fn describe(&self) -> String {
		let others = if self.isolated {
			"which the kernel isolates from other processes"
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
			restore.disarm();
			if let Err(err) = restore.apply(&mut self.lease) {
				eprintln!("swole: {err}");
			}
		}
	}
}

/// Leases a physical core of `pmu`'s CPUs, or without counters of the performance cores swole may
/// run on, waiting for one when other swole runs hold them all, and sets its governor where
/// swole may; off Linux, none.
#[cfg(target_os = "linux")]
pub fn reserve(pmu: Option<&perf::Pmu>) -> Result<Option<Reservation>, String> {
	let candidates = match pmu {
		Some(pmu) => pmu.cpus.clone(),
		None => performance()?,
	};
	reserve_in(Path::new(SYS), Path::new(LEASES), candidates).map(Some)
}

#[cfg(not(target_os = "linux"))]
pub fn reserve(_: Option<&perf::Pmu>) -> Result<Option<Reservation>, String> {
	Ok(None)
}

/// The CPUs swole may run on, only the performance cores' on a hybrid Intel host that lets it run
/// on one.
#[cfg(target_os = "linux")]
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
fn reserve_in(sys: &Path, leases: &Path, candidates: Vec<usize>) -> Result<Reservation, String> {
	let cores = cores(sys, &candidates)?;
	let (core, mut lease) = lease(leases, &cores)?;
	recover(sys, &mut lease)?;
	let cpu = *core
		.iter()
		.find(|cpu| candidates.contains(cpu))
		.expect("a core holds a candidate");
	let isolated = match std::fs::read_to_string(sys.join("isolated")) {
		Ok(list) => perf::cpus(&list)?.contains(&cpu),
		Err(_) => false,
	};
	let (governor, restore) = tune(sys, &core, &mut lease)?;
	Ok(Reservation {
		cpu,
		core,
		spread: candidates,
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

/// Leases the first of `cores` no other swole run holds, or waits for the first when they hold
/// every one: the core, and its lease's file, locked.
fn lease(leases: &Path, cores: &[Vec<usize>]) -> Result<(Vec<usize>, File), String> {
	shared(leases)?;
	for core in cores {
		let file = open(leases, core)?;
		match file.try_lock() {
			Ok(()) => return Ok((core.clone(), file)),
			Err(std::fs::TryLockError::WouldBlock) => {}
			Err(std::fs::TryLockError::Error(e)) => return Err(format!("cannot lease core {}: {e}", core[0])),
		}
	}
	let core = &cores[0];
	eprintln!(
		"swole: other swole runs lease every core swole may time on, so waiting for core {}",
		core[0]
	);
	let file = open(leases, core)?;
	file.lock().map_err(|e| format!("cannot lease core {}: {e}", core[0]))?;
	Ok((core.clone(), file))
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

/// Opens the lease's file of `core`, named by its first thread, which every user may write, since
/// a run of any user records there what to restore.
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

/// What swole changed in cpufreq policies, each file and what it held, and the lease's file
/// recording them, which a signal handler restores through.
struct Restore {
	entries: Vec<(std::ffi::CString, Vec<u8>)>,
	lease: i32,
}

impl Restore {
	/// The record a lease's file holds: a line a file, its path and what it held.
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

	/// Writes each file back, then empties the record, which stays for the next run on failure.
	fn apply(&self, lease: &mut File) -> Result<(), String> {
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
		lease
			.set_len(0)
			.map_err(|e| format!("cannot clear a core's lease: {e}"))
	}

	/// Hands it to the signal handler until [`Restore::disarm`].
	#[cfg(target_os = "linux")]
	fn arm(&self) {
		signals::arm(self);
	}

	#[cfg(not(target_os = "linux"))]
	fn arm(&self) {}

	/// Takes it back from the signal handler, which then restores nothing.
	#[cfg(target_os = "linux")]
	fn disarm(&self) {
		signals::disarm();
	}

	#[cfg(not(target_os = "linux"))]
	fn disarm(&self) {}
}

/// Restores what the record in `lease` says a run killed outright left, before this run reads the
/// policy as the host's.
fn recover(sys: &Path, lease: &mut File) -> Result<(), String> {
	let mut record = String::new();
	lease
		.read_to_string(&mut record)
		.map_err(|e| format!("cannot read a core's lease: {e}"))?;
	if record.is_empty() {
		return Ok(());
	}
	let mut entries = Vec::new();
	for line in record.lines() {
		let (path, value) = line
			.split_once('\t')
			.ok_or_else(|| format!("a core's lease records {line:?}, no file and value"))?;
		let path = Path::new(path);
		// Any user may write a lease, so it restores only a tuned file of a CPU's policy, to a word.
		let tuned = path.starts_with(sys)
			&& path
				.components()
				.all(|part| matches!(part, std::path::Component::RootDir | std::path::Component::Normal(_)))
			&& path
				.file_name()
				.and_then(|name| name.to_str())
				.is_some_and(|name| TUNED.contains(&name));
		let word = !value.is_empty() && value.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_');
		if !tuned || !word {
			return Err(format!("a core's lease records {line:?}, which swole never changes"));
		}
		entries.push((cstring(path), value.as_bytes().to_vec()));
	}
	// One swole may write a policy another may not; the run then times under what it finds.
	match (Restore { entries, lease: -1 }).apply(lease) {
		Ok(()) => eprintln!("swole: restored the cpufreq policy a killed swole run left changed"),
		Err(err) => eprintln!("swole: a killed swole run left a cpufreq policy changed, and {err}"),
	}
	Ok(())
}

/// Sets `performance` on every cpufreq policy of `core`, recording in `lease` what each held
/// first, where every one is the core's alone and swole may write it: the governor the run times
/// under, and what to restore.
fn tune(sys: &Path, core: &[usize], lease: &mut File) -> Result<(Governor, Option<Box<Restore>>), String> {
	let read = |path: &Path| {
		std::fs::read_to_string(path)
			.map(|text| text.trim().to_owned())
			.map_err(|e| format!("cannot read {}: {e}", path.display()))
	};
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
	let Some(first) = policies.first() else {
		return Ok((Governor::Absent, None));
	};
	let governor = read(&first.join(TUNED[0]))?;
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
		let related = perf::cpus(&read(&policy.join("related_cpus"))?)?;
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
						"since swole may not write {}: grant it as the README's Bench says",
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
		lease: std::os::fd::AsRawFd::as_raw_fd(lease),
	});
	// Recorded before anything changes, so a run killed at any point leaves what to restore.
	lease
		.set_len(0)
		.and_then(|()| lease.rewind())
		.and_then(|()| lease.write_all(restore.record().as_bytes()))
		.map_err(|e| format!("cannot record a core's governor in its lease: {e}"))?;
	restore.arm();
	for policy in &policies {
		let path = policy.join(TUNED[0]);
		if let Err(e) = std::fs::write(&path, PERFORMANCE) {
			restore.disarm();
			let undone = restore
				.apply(lease)
				.err()
				.map_or(String::new(), |err| format!("; {err}"));
			return Err(format!("cannot write {}: {e}{undone}", path.display()));
		}
	}
	Ok((Governor::Set { was: governor }, Some(restore)))
}

fn cstring(path: &Path) -> std::ffi::CString {
	std::ffi::CString::new(path.as_os_str().as_encoded_bytes()).expect("a sysfs path holds no NUL")
}

/// Restores a run's governor when a signal ends it, which unwinds nothing.
#[cfg(target_os = "linux")]
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
		fn ftruncate(fd: i32, length: i64) -> i32;
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

	/// Writes back each file, empties the lease's record if all took, then dies of `signum` as
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
				// SAFETY: the lease's descriptor, open while the restore is alive.
				unsafe { ftruncate(restore.lease, 0) };
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
		let first = reserve_in(&sys, &leases, all.clone()).unwrap();
		let second = reserve_in(&sys, &leases, all.clone()).unwrap();
		assert_eq!((first.cpu, &first.core), (1, &vec![1, 3]));
		assert_eq!((second.cpu, &second.core), (0, &vec![0, 2]));
		assert_eq!(second.spread, all);
		drop(first);
		let third = reserve_in(&sys, &leases, all).unwrap();
		assert_eq!(third.cpu, 1);
		assert!(matches!(&third.governor, Governor::Kept { governor, .. } if governor == PERFORMANCE));
	}

	#[test]
	fn sets_performance_and_restores_it_on_drop() {
		let _arms = ARMS.lock().unwrap();
		let dir = scratch("governor");
		let sys = sys(&dir, 2, 2, "powersave");
		let leases = dir.join("leases");
		let reservation = reserve_in(&sys, &leases, vec![0, 1, 2, 3]).unwrap();
		assert!(matches!(&reservation.governor, Governor::Set { was } if was == "powersave"));
		assert_eq!(
			(governor(&sys, 1), governor(&sys, 3)),
			(PERFORMANCE.into(), PERFORMANCE.into())
		);
		assert_eq!(governor(&sys, 0), "powersave");
		let record = std::fs::read_to_string(leases.join("core1")).unwrap();
		assert_eq!(record.lines().count(), 4, "{record}");
		drop(reservation);
		assert_eq!(
			(governor(&sys, 1), governor(&sys, 3)),
			("powersave".into(), "powersave".into())
		);
		assert_eq!(std::fs::read_to_string(leases.join("core1")).unwrap(), "");
	}

	#[test]
	fn the_next_run_restores_what_a_killed_one_left() {
		let _arms = ARMS.lock().unwrap();
		let dir = scratch("recover");
		let sys = sys(&dir, 1, 2, "powersave");
		let leases = dir.join("leases");
		let killed = reserve_in(&sys, &leases, vec![0, 1]).unwrap();
		assert_eq!(governor(&sys, 0), PERFORMANCE);
		// Killed outright: the lock goes with its file, and nothing restores.
		let mut killed = killed;
		killed.restore.take().unwrap().disarm();
		drop(killed);
		let next = reserve_in(&sys, &leases, vec![0, 1]).unwrap();
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
		let sys = sys(&dir, 1, 1, "powersave");
		let leases = dir.join("leases");
		shared(&leases).unwrap();
		for record in [
			"/etc/passwd\tperformance\n".to_owned(),
			format!("{}/../../scaling_governor\tperformance\n", sys.display()),
			format!("{}\tperformance\n", sys.join("cpu0/cpufreq/scaling_max_freq").display()),
			format!(
				"{}\tper formance\n",
				sys.join("cpu0/cpufreq/scaling_governor").display()
			),
		] {
			std::fs::write(leases.join("core0"), &record).unwrap();
			let err = reserve_in(&sys, &leases, vec![0]).err().unwrap();
			assert!(err.contains("which swole never changes"), "{err}");
		}
	}

	#[test]
	fn keeps_a_policy_another_core_shares() {
		let dir = scratch("shared");
		let sys = sys(&dir, 2, 1, "powersave");
		std::fs::write(sys.join("cpufreq/policy1/related_cpus"), "0-1\n").unwrap();
		let reservation = reserve_in(&sys, &dir.join("leases"), vec![0, 1]).unwrap();
		assert_eq!(reservation.cpu, 1);
		assert!(
			matches!(&reservation.governor, Governor::Kept { governor, why } if governor == "powersave" && why.contains("CPU 0"))
		);
		assert_eq!(governor(&sys, 1), "powersave");
	}
}
