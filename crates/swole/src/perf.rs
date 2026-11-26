//! Linux perf counters swole opened on this process before it ran the benchmark target, which
//! the harness starts and stops around each case and reads. swole opens them, with whatever
//! privileges it holds, such as `CAP_PERFMON`, so a benchmark target needs none of its own.

use std::io::Read;
use std::os::fd::{FromRawFd, OwnedFd};

/// The counters swole opened, in the order it opened them, which is the order of their
/// descriptors: each takes the lowest free one.
pub struct Counters {
	fds: Vec<OwnedFd>,
}

impl Counters {
	/// The `count` counters swole left open on this process, each counting every thread the
	/// process starts. Fails unless there are exactly that many.
	pub fn inherited(count: usize) -> Result<Self, String> {
		let entries = std::fs::read_dir("/proc/self/fd").map_err(|e| format!("cannot list /proc/self/fd: {e}"))?;
		let mut found = Vec::new();
		for entry in entries {
			let entry = entry.map_err(|e| format!("cannot list /proc/self/fd: {e}"))?;
			// The listing's own descriptor is gone by the time it is read.
			let Ok(link) = std::fs::read_link(entry.path()) else {
				continue;
			};
			if link.as_os_str() != "anon_inode:[perf_event]" {
				continue;
			}
			let fd: i32 = entry
				.file_name()
				.to_str()
				.and_then(|name| name.parse().ok())
				.ok_or("/proc/self/fd lists a descriptor by no number")?;
			found.push(fd);
		}
		found.sort_unstable();
		if found.len() != count {
			return Err(format!(
				"swole opened {count} perf counters on this process, but it holds {}",
				found.len()
			));
		}
		Ok(Self {
			// SAFETY: each is an open perf event descriptor nothing else in the process owns:
			// swole opened it before the process started, for the harness alone.
			fds: found
				.into_iter()
				.map(|fd| unsafe { OwnedFd::from_raw_fd(fd) })
				.collect(),
		})
	}

	/// Starts every counter at once: the thread that opened them, which became this process's
	/// main thread, owns them, so one `prctl` starts them all, on every thread.
	#[inline(always)]
	pub fn start(&self) {
		prctl(PR_TASK_PERF_EVENTS_ENABLE);
	}

	/// Stops every counter at once.
	#[inline(always)]
	pub fn stop(&self) {
		prctl(PR_TASK_PERF_EVENTS_DISABLE);
	}

	/// Each counter's value, refusing one that did not count the whole time it was enabled.
	pub fn read(&self) -> Result<Vec<u64>, String> {
		self.fds.iter().map(read).collect()
	}
}

/// Fails unless the process loaded the first library `LD_PRELOAD` names, where swole puts
/// its own, which a static executable never does, so a count never follows the host's
/// randomness and clock unannounced.
pub fn preloaded() -> Result<(), String> {
	let Some(preloads) = std::env::var_os("LD_PRELOAD") else {
		return Ok(());
	};
	let preloads = preloads.to_string_lossy().into_owned();
	let Some(library) = preloads.split([':', ' ']).find(|library| !library.is_empty()) else {
		return Ok(());
	};
	let name =
		std::ffi::CString::new(library).map_err(|_| format!("LD_PRELOAD names {library:?}, which holds a NUL"))?;
	// Finds a library already loaded, by a bare name as the loader searched for it, or by a path
	// or a link to it as the same file, and loads none.
	// SAFETY: `name` is a C string; `RTLD_NOLOAD` only looks the library up.
	let handle = unsafe { dlopen(name.as_ptr(), RTLD_NOLOAD | RTLD_LAZY) };
	if handle.is_null() {
		return Err(format!(
			"LD_PRELOAD names {library}, which this process never loaded: the loader found no such library, or a static \
			 executable or a glibc before 2.34 ignores it"
		));
	}
	// SAFETY: the handle `dlopen` returned, whose reference it took, released once.
	unsafe { dlclose(handle) };
	Ok(())
}

/// How many threads swole's preload saw the process start, or `None` without it: a thread a
/// case starts and joins within its call is gone before the case's end can list it.
pub fn started() -> Option<u64> {
	// SAFETY: the name is a C string; RTLD_DEFAULT searches every loaded library.
	let symbol = unsafe { dlsym(RTLD_DEFAULT, c"swole_threads_started".as_ptr()) };
	if symbol.is_null() {
		return None;
	}
	// SAFETY: the preload defines the symbol as a function of this signature.
	let started: extern "C" fn() -> u64 = unsafe { std::mem::transmute(symbol) };
	Some(started())
}

/// How many threads the process has now.
pub fn threads() -> Result<usize, String> {
	let tasks = std::fs::read_dir("/proc/self/task").map_err(|e| format!("cannot list /proc/self/task: {e}"))?;
	Ok(tasks.count())
}

const RTLD_LAZY: i32 = 1;
const RTLD_NOLOAD: i32 = 4;
const RTLD_DEFAULT: *mut std::ffi::c_void = std::ptr::null_mut();
const PR_TASK_PERF_EVENTS_DISABLE: i32 = 31;
const PR_TASK_PERF_EVENTS_ENABLE: i32 = 32;

unsafe extern "C" {
	#[link_name = "prctl"]
	fn sys_prctl(option: i32, ...) -> i32;
	fn dlopen(name: *const std::ffi::c_char, flags: i32) -> *mut std::ffi::c_void;
	fn dlclose(handle: *mut std::ffi::c_void) -> i32;
	fn dlsym(handle: *mut std::ffi::c_void, symbol: *const std::ffi::c_char) -> *mut std::ffi::c_void;
}

/// Starts or stops every perf event the calling thread opened, or swole opened before `exec`.
#[inline(always)]
fn prctl(option: i32) {
	// SAFETY: these options take no argument.
	let done = unsafe { sys_prctl(option) };
	assert_eq!(done, 0, "a thread starts and stops the perf events it owns");
}

/// A counter's value, refusing one that did not count the whole time it was enabled.
fn read(fd: &OwnedFd) -> Result<u64, String> {
	let mut bytes = [0u8; 24];
	let mut file = std::fs::File::from(fd.try_clone().map_err(|e| format!("cannot read a perf counter: {e}"))?);
	file.read_exact(&mut bytes)
		.map_err(|e| format!("cannot read a perf counter: {e}"))?;
	let [count, enabled, running] =
		std::array::from_fn(|i| u64::from_ne_bytes(bytes[i * 8..i * 8 + 8].try_into().expect("8 bytes")));
	// A counter the kernel shared among more events than the PMU holds counted part of the
	// time, and one on a CPU of another PMU counted none of it: neither is scaled up.
	if enabled != running {
		return Err(format!(
			"a perf counter ran {running} ns of the {enabled} ns it was enabled, so it missed events: another user of \
			 this host's counters multiplexed it, or a thread left the CPUs of the PMU it counts"
		));
	}
	Ok(count)
}
