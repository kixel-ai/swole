//! Heap allocation counts, shared by each library that interposes the allocator: what the
//! allocations made while the harness's meter has them count add up to, and the most bytes they
//! held at once. Each library's own glue forwards each call to the allocator and reports it here.

use core::cell::UnsafeCell;
use core::ffi::{c_int, c_void};
use core::sync::atomic::{AtomicBool, AtomicU64, Ordering::*};

use super::refuse;

struct Shared<T>(UnsafeCell<T>);

// SAFETY: TABLE, the one user, is reached only under LOCK.
unsafe impl<T> Sync for Shared<T> {}

static COUNTING: AtomicBool = AtomicBool::new(false);
static COUNT: AtomicU64 = AtomicU64::new(0);
static BYTES: AtomicU64 = AtomicU64::new(0);
static LIVE: AtomicU64 = AtomicU64::new(0);
static PEAK: AtomicU64 = AtomicU64::new(0);

/// Each block allocated while counting and not yet freed, by address, with its size, so a
/// free subtracts from the live bytes only what counting added: an open-addressed table in
/// pages of its own, so it never allocates through the allocator it watches.
struct Table {
	slots: *mut Slot,
	/// A power of two, or zero before the first insert.
	capacity: usize,
	len: usize,
	/// Slots of removed blocks, which a probe passes over.
	removed: usize,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct Slot {
	/// The block's address, or [`EMPTY`] or [`REMOVED`].
	at: usize,
	size: u64,
}

const EMPTY: usize = 0;
const REMOVED: usize = 1;

static TABLE: Shared<Table> = Shared(UnsafeCell::new(Table {
	slots: core::ptr::null_mut(),
	capacity: 0,
	len: 0,
	removed: 0,
}));
static LOCK: AtomicBool = AtomicBool::new(false);

unsafe extern "C" {
	fn mmap(addr: *mut c_void, len: usize, prot: c_int, flags: c_int, fd: c_int, offset: i64) -> *mut c_void;
	fn munmap(addr: *mut c_void, len: usize) -> c_int;
}

const PROT_READ: c_int = 1;
const PROT_WRITE: c_int = 2;
const MAP_PRIVATE: c_int = 2;
#[cfg(target_os = "linux")]
const MAP_ANONYMOUS: c_int = 0x20;
#[cfg(target_os = "macos")]
const MAP_ANONYMOUS: c_int = 0x1000;
const MAP_FAILED: *mut c_void = !0 as *mut c_void;

/// Runs `f` on the table, under its lock.
fn locked<R>(f: impl FnOnce(&mut Table) -> R) -> R {
	while LOCK.compare_exchange_weak(false, true, Acquire, Relaxed).is_err() {
		core::hint::spin_loop();
	}
	// SAFETY: the lock gives this thread the table alone.
	let out = f(unsafe { &mut *TABLE.0.get() });
	LOCK.store(false, Release);
	out
}

impl Table {
	fn index(&self, at: usize) -> usize {
		(at >> 4).wrapping_mul(0x9e37_79b9_7f4a_7c15) >> (usize::BITS - self.capacity.trailing_zeros())
	}

	/// The slot `i`, which must be below the capacity: no slice, whose bounds check would link
	/// core's formatting and unwinding into the library.
	fn slot(&mut self, i: usize) -> &mut Slot {
		// SAFETY: `slots` maps `capacity` slots, which only this table reaches, and callers
		// mask `i` by the capacity.
		unsafe { &mut *self.slots.add(i & (self.capacity - 1)) }
	}

	fn insert(&mut self, at: usize, size: u64) {
		if (self.len + self.removed + 1) * 2 > self.capacity {
			self.grow();
		}
		let mut i = self.index(at);
		// A block the table holds already the allocator handed out again, which it never does
		// for a live one, unless it was freed past this library: it is the new one.
		while !matches!(self.slot(i).at, EMPTY | REMOVED) && self.slot(i).at != at {
			i += 1;
		}
		let slot = self.slot(i);
		let was = slot.at;
		*slot = Slot { at, size };
		if was == REMOVED {
			self.removed -= 1;
		}
		if was != at {
			self.len += 1;
		}
	}

	fn remove(&mut self, at: usize) -> Option<u64> {
		if self.capacity == 0 {
			return None;
		}
		let mut i = self.index(at);
		loop {
			match self.slot(i).at {
				EMPTY => return None,
				found if found == at => break,
				_ => i += 1,
			}
		}
		let slot = self.slot(i);
		slot.at = REMOVED;
		let size = slot.size;
		self.len -= 1;
		self.removed += 1;
		Some(size)
	}

	/// Rehashes into twice as many slots as the live blocks need, dropping removed ones.
	fn grow(&mut self) {
		let capacity = ((self.len + 1) * 4).next_power_of_two().max(4096);
		let bytes = capacity * core::mem::size_of::<Slot>();
		// SAFETY: a private anonymous mapping, which starts zeroed, so every slot EMPTY.
		let slots = unsafe {
			mmap(
				core::ptr::null_mut(),
				bytes,
				PROT_READ | PROT_WRITE,
				MAP_PRIVATE | MAP_ANONYMOUS,
				-1,
				0,
			)
		};
		if slots == MAP_FAILED {
			refuse(b"swole preload: cannot map the table of live allocations\n\0");
		}
		let mut old = core::mem::replace(
			self,
			Table {
				slots: slots.cast(),
				capacity,
				len: 0,
				removed: 0,
			},
		);
		for i in 0..old.capacity {
			let slot = *old.slot(i);
			if slot.at > REMOVED {
				self.insert(slot.at, slot.size);
			}
		}
		old.unmap();
	}

	fn unmap(&mut self) {
		if self.capacity > 0 {
			// SAFETY: the table's own mapping, which nothing reaches once replaced.
			unsafe { munmap(self.slots.cast(), self.capacity * core::mem::size_of::<Slot>()) };
		}
	}
}

/// Counts a block of `size` bytes allocated at `ptr`, when counting and it was allocated.
pub fn allocated(ptr: *mut c_void, size: usize) {
	if ptr.is_null() || !COUNTING.load(Relaxed) {
		return;
	}
	COUNT.fetch_add(1, Relaxed);
	BYTES.fetch_add(size as u64, Relaxed);
	locked(|table| table.insert(ptr as usize, size as u64));
	let live = LIVE.fetch_add(size as u64, Relaxed) + size as u64;
	PEAK.fetch_max(live, Relaxed);
}

/// Uncounts the block at `ptr`, when counting allocated it, returning its size, before the
/// allocator frees it: once freed, another thread may be handed the same address.
pub fn freed(ptr: *mut c_void) -> Option<u64> {
	if ptr.is_null() || !COUNTING.load(Relaxed) {
		return None;
	}
	let size = locked(|table| table.remove(ptr as usize))?;
	LIVE.fetch_sub(size, Relaxed);
	Some(size)
}

/// Counts again the block at `ptr` of `size` bytes, which [`freed`] uncounted, when the allocator
/// did not free it after all, as a failed `realloc` does not.
pub fn kept(ptr: *mut c_void, size: Option<u64>) {
	if let Some(size) = size {
		locked(|table| table.insert(ptr as usize, size));
		LIVE.fetch_add(size, Relaxed);
	}
}

/// Zeroes the counts and starts counting, which the harness's meter calls as its window opens.
#[unsafe(no_mangle)]
pub extern "C" fn swole_heap_start() {
	COUNTING.store(false, Relaxed);
	locked(|table| {
		table.unmap();
		*table = Table {
			slots: core::ptr::null_mut(),
			capacity: 0,
			len: 0,
			removed: 0,
		};
	});
	for count in [&COUNT, &BYTES, &LIVE, &PEAK] {
		count.store(0, Relaxed);
	}
	COUNTING.store(true, SeqCst);
}

/// Stops counting, which the meter calls as its window closes.
#[unsafe(no_mangle)]
pub extern "C" fn swole_heap_stop() {
	COUNTING.store(false, SeqCst);
}

/// Writes the allocations counted, their bytes, and the most bytes live at once to `out`.
///
/// # Safety
///
/// `out` holds three writable `u64`s.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn swole_heap_read(out: *mut u64) {
	for (i, count) in [&COUNT, &BYTES, &PEAK].into_iter().enumerate() {
		// SAFETY: the caller hands three.
		unsafe { out.add(i).write(count.load(SeqCst)) };
	}
}
