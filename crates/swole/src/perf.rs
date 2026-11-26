//! Linux perf counters swole opened on this process before it ran the benchmark target, which
//! the harness reads in user space around each call. swole opens them, with whatever privileges
//! it holds, such as `CAP_PERFMON`, so a benchmark target needs none of its own.
//!
//! A hardware counter is read with `rdpmc` through the page the kernel keeps for it, so the
//! system calls that start and stop counting fall outside what a count holds. Such counters
//! count the calling thread alone: the kernel maps no page for a counter its threads inherit.

use std::cell::Cell;
use std::io::Read;
#[cfg(target_arch = "x86_64")]
use std::os::fd::AsRawFd;
use std::os::fd::{FromRawFd, OwnedFd};
use std::sync::atomic::{Ordering, compiler_fence};

/// The counters swole opened, in the order it opened them, which is the order of their
/// descriptors: each takes the lowest free one. The hardware ones come first.
pub struct Counters {
	fds: Vec<OwnedFd>,
	/// Each hardware counter, read in user space.
	slots: Vec<Slot>,
	/// Why a call's reads cannot be trusted, once one's cannot.
	failed: Cell<Option<&'static str>>,
}

/// A hardware counter, read in user space, and what its reads found.
struct Slot {
	page: Page,
	/// Its page as the current call's start read it, and as it read after the end.
	snap: Cell<Snap>,
	last: Cell<Snap>,
	/// What its hardware counter read at the current call's start, and at its end, as `rdpmc`
	/// returns it: its low and high halves.
	start: Cell<(u32, u32)>,
	end: Cell<(u32, u32)>,
	/// What it counted over the calls so far.
	counted: Cell<u64>,
}

impl Counters {
	/// The `hardware` then `software` counters swole left open on this process, each counting
	/// only the thread that runs the benchmark. Fails unless there are exactly that many, or a
	/// hardware one cannot be read in user space.
	pub fn inherited(hardware: usize, software: usize) -> Result<Self, String> {
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
		let count = hardware + software;
		if found.len() != count {
			return Err(format!(
				"swole opened {count} perf counters on this process, but it holds {}",
				found.len()
			));
		}
		// SAFETY: each is an open perf event descriptor nothing else in the process owns:
		// swole opened it before the process started, for the harness alone.
		let fds: Vec<OwnedFd> = found
			.into_iter()
			.map(|fd| unsafe { OwnedFd::from_raw_fd(fd) })
			.collect();
		let slots = fds[..hardware]
			.iter()
			.map(|fd| {
				Ok(Slot {
					page: Page::map(fd)?,
					snap: Cell::default(),
					last: Cell::default(),
					start: Cell::default(),
					end: Cell::default(),
					counted: Cell::default(),
				})
			})
			.collect::<Result<Vec<_>, String>>()?;
		Ok(Self {
			fds,
			slots,
			failed: Cell::new(None),
		})
	}

	/// Starts every counter, then reads each hardware one, so the `prctl` that starts them all
	/// (the thread that opened them owns them) comes before the count. The pages are read
	/// first and the counters back to back, so what each counter counts of the reads' own is the
	/// same few instructions for every counter and every call.
	#[inline(never)]
	pub fn start(&self) {
		prctl(PR_TASK_PERF_EVENTS_ENABLE);
		let Some(first) = self.slots.first() else {
			return;
		};
		loop {
			for slot in &self.slots {
				slot.snap.set(slot.page.snap());
			}
			if self.slots.iter().any(|slot| slot.snap.get().index == 0) {
				// Not on the PMU while it should count: `read` fails the run, as it ran less than
				// it was enabled.
				return self.fail(OFF);
			}
			compiler_fence(Ordering::SeqCst);
			for slot in &self.slots {
				slot.start.set(rdpmc(slot.snap.get().index - 1));
			}
			compiler_fence(Ordering::SeqCst);
			// The kernel rewrites every page at once, as it moves the thread's counters on or off a
			// CPU, so the first page read, unchanged since, means no page changed after its read;
			// `stop` checks each.
			if first.page.lock() == first.snap.get().lock {
				return;
			}
		}
	}

	/// Reads each hardware counter back to back, in the order `start` read them, then adds what
	/// each counted since, and stops every counter.
	#[inline(never)]
	pub fn stop(&self) {
		if self.failed.get().is_none()
			&& let Some(first) = self.slots.first()
		{
			let lock = first.page.lock();
			compiler_fence(Ordering::SeqCst);
			for slot in &self.slots {
				slot.end.set(rdpmc(slot.snap.get().index - 1));
			}
			compiler_fence(Ordering::SeqCst);
			self.add(lock);
		}
		prctl(PR_TASK_PERF_EVENTS_DISABLE);
	}

	/// Adds what each hardware counter counted over the call just read, given the first page's
	/// lock as the end read it, just before the hardware counters.
	fn add(&self, lock: u32) {
		let Some(first) = self.slots.first() else {
			return;
		};
		// The kernel rewrites every page at once, as in `start`, so the first page unchanged
		// since means each page says where its counter was as the end read it. Else a context
		// switch landed on those reads, and the start's page stands in, where it still says the
		// same.
		let held = loop {
			for slot in &self.slots {
				slot.last.set(slot.page.snap());
			}
			compiler_fence(Ordering::SeqCst);
			if first.page.lock() == first.last.get().lock {
				break first.last.get().lock == lock;
			}
		};
		for slot in &self.slots {
			let (start, now) = (slot.snap.get(), slot.last.get());
			// The end read the hardware counter the start's page named, which must still hold the
			// counter, as it does unless another user of the PMU moved it during the call.
			if now.index != start.index || !held && now.offset != start.offset {
				return self.fail(MOVED);
			}
			let ended = now.value(slot.end.get());
			slot.counted.set(
				slot.counted
					.get()
					.wrapping_add(ended.wrapping_sub(start.value(slot.start.get()))),
			);
		}
	}

	/// Notes why the calls' reads cannot be trusted, the first time.
	#[cold]
	fn fail(&self, why: &'static str) {
		if self.failed.get().is_none() {
			self.failed.set(Some(why));
		}
	}

	/// Each counter's value, refusing one that did not count the whole time it was enabled: a
	/// hardware one's over the calls, a software one's from each start to its stop.
	pub fn read(&self) -> Result<Vec<u64>, String> {
		if let Some(why) = self.failed.get() {
			return Err(why.to_owned());
		}
		let mut values = Vec::with_capacity(self.fds.len());
		for (i, fd) in self.fds.iter().enumerate() {
			let total = read(fd)?;
			let Some(slot) = self.slots.get(i) else {
				values.push(total);
				continue;
			};
			let counted = slot.counted.get();
			// Started and stopped around the calls, the counter holds every read of its page.
			if counted > total {
				return Err(format!(
					"a perf counter read {counted} in user space over the calls, more than its {total} over the whole \
					 time it counted"
				));
			}
			values.push(counted);
		}
		Ok(values)
	}
}

/// A counter's page as one read found it.
#[derive(Clone, Copy, Default)]
struct Snap {
	/// The page's sequence count, which the kernel bumps around each rewrite.
	lock: u32,
	/// Which hardware counter holds the counter, plus one, or 0 while none does.
	index: u32,
	/// What the counter counted before the hardware counter's current value.
	offset: i64,
	/// How many bits wide the hardware counter is.
	width: u32,
}

impl Snap {
	/// The counter's value, given what its hardware counter read, as `rdpmc` returns it.
	fn value(self, (low, high): (u32, u32)) -> u64 {
		let raw = (u64::from(high) << 32 | u64::from(low)) as i64;
		// The hardware counter's value, as wide as it is, signed.
		self.offset.wrapping_add(raw << (64 - self.width) >> (64 - self.width)) as u64
	}
}

/// Why a call's reads fail when a counter was not on the PMU while it should have counted.
const OFF: &str = "a perf counter was not on the PMU while it counted: another user of this host's counters crowded it \
                   out, or the thread left the CPUs of the PMU it counts";

/// Why a call's reads fail when the kernel moved a counter to another hardware counter during it.
const MOVED: &str = "the kernel moved a perf counter to another hardware counter during a call, as another user of \
                     this host's counters can make it, so what the call counted cannot be told";

/// The page the kernel keeps for a counter: its offset and which hardware counter holds it.
struct Page(*const u8);

/// What a counter's page holds, by byte offset: `struct perf_event_mmap_page`.
const LOCK: usize = 8;
const INDEX: usize = 12;
const OFFSET: usize = 16;
#[cfg(target_arch = "x86_64")]
const CAPABILITIES: usize = 40;
const PMC_WIDTH: usize = 48;

/// The capability bit saying `rdpmc` reads the counter.
#[cfg(target_arch = "x86_64")]
const CAP_USER_RDPMC: u64 = 1 << 2;

/// One page, which is what the kernel maps for a counter without a ring buffer.
#[cfg(target_arch = "x86_64")]
const PAGE: usize = 4096;

impl Page {
	/// Maps `fd`'s page, refusing a counter that cannot be read in user space.
	#[cfg(target_arch = "x86_64")]
	fn map(fd: &OwnedFd) -> Result<Self, String> {
		// SAFETY: maps one page of a perf event descriptor, read-only and shared, as the kernel
		// exposes it; the mapping lives as long as the process.
		let page = unsafe { mmap(std::ptr::null_mut(), PAGE, PROT_READ, MAP_SHARED, fd.as_raw_fd(), 0) };
		if page == MAP_FAILED {
			return Err(format!(
				"cannot map a perf counter's page: {}",
				std::io::Error::last_os_error()
			));
		}
		let page = Self(page.cast());
		// SAFETY: the page holds the field at its offset.
		let capabilities = unsafe { std::ptr::read_volatile(page.0.add(CAPABILITIES).cast::<u64>()) };
		if capabilities & CAP_USER_RDPMC == 0 {
			return Err(
				"this host does not let a process read its perf counters in user space (cap_user_rdpmc is off), which \
				 swole counts with"
					.to_owned(),
			);
		}
		Ok(page)
	}

	#[cfg(not(target_arch = "x86_64"))]
	fn map(_: &OwnedFd) -> Result<Self, String> {
		Err("swole reads perf counters in user space with x86_64's rdpmc alone".to_owned())
	}

	/// The page's sequence count now.
	#[inline(always)]
	fn lock(&self) -> u32 {
		// SAFETY: the page is mapped for the process's life, and holds the field at its offset.
		unsafe { std::ptr::read_volatile(self.0.add(LOCK).cast::<u32>()) }
	}

	/// What the page holds now, which may be mid-rewrite: its reader checks its lock.
	#[inline(always)]
	fn snap(&self) -> Snap {
		let lock = self.lock();
		compiler_fence(Ordering::SeqCst);
		// SAFETY: as in `lock`.
		unsafe {
			Snap {
				lock,
				index: std::ptr::read_volatile(self.0.add(INDEX).cast::<u32>()),
				offset: std::ptr::read_volatile(self.0.add(OFFSET).cast::<i64>()),
				width: u32::from(std::ptr::read_volatile(self.0.add(PMC_WIDTH).cast::<u16>())),
			}
		}
	}
}

/// What hardware counter `counter` holds now, as its low and high halves.
#[cfg(target_arch = "x86_64")]
#[inline(always)]
fn rdpmc(counter: u32) -> (u32, u32) {
	let (low, high): (u32, u32);
	// SAFETY: the kernel allows `rdpmc` while the process maps a counter's page, and `counter`
	// is one its page names. Fenced first, since `rdpmc` alone may read before earlier
	// instructions retire.
	unsafe {
		std::arch::asm!(
			"lfence",
			"rdpmc",
			in("ecx") counter,
			out("eax") low,
			out("edx") high,
			options(nostack, preserves_flags),
		);
	}
	(low, high)
}

#[cfg(not(target_arch = "x86_64"))]
fn rdpmc(_: u32) -> (u32, u32) {
	unreachable!("no page maps off x86_64")
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
#[cfg(target_arch = "x86_64")]
const PROT_READ: i32 = 1;
#[cfg(target_arch = "x86_64")]
const MAP_SHARED: i32 = 1;
#[cfg(target_arch = "x86_64")]
const MAP_FAILED: *mut std::ffi::c_void = !0 as *mut std::ffi::c_void;

unsafe extern "C" {
	#[link_name = "prctl"]
	fn sys_prctl(option: i32, ...) -> i32;
	fn dlopen(name: *const std::ffi::c_char, flags: i32) -> *mut std::ffi::c_void;
	fn dlclose(handle: *mut std::ffi::c_void) -> i32;
	fn dlsym(handle: *mut std::ffi::c_void, symbol: *const std::ffi::c_char) -> *mut std::ffi::c_void;
	#[cfg(target_arch = "x86_64")]
	fn mmap(
		addr: *mut std::ffi::c_void,
		len: usize,
		prot: i32,
		flags: i32,
		fd: i32,
		offset: i64,
	) -> *mut std::ffi::c_void;
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
