//! The library swole inserts into a macOS benchmark process for `bench`'s allocation pass, with
//! `DYLD_INSERT_LIBRARIES`: it counts heap allocations as `allocs.rs` does, and the threads the
//! process starts, which the harness reads around each call. It interposes libSystem's allocator
//! and `pthread_create` through dyld's `__interpose` section, which redirects every other image's
//! calls, while this library's own calls reach the originals.
//!
//! Built by build.rs as a `no_std` cdylib for macOS, where it is the one library swole inserts:
//! macOS's counts keep the host's clock and randomness, as `bench`'s do everywhere.

#![no_std]

use core::ffi::{c_int, c_void};
use core::sync::atomic::{AtomicU64, Ordering::Relaxed};

mod allocs;

use allocs::{allocated, freed, kept};

// libSystem's, which a `no_std` library links by name.
#[link(name = "System")]
unsafe extern "C" {
	fn abort() -> !;
	fn write(fd: c_int, buf: *const c_void, len: usize) -> isize;
	fn malloc(size: usize) -> *mut c_void;
	fn calloc(n: usize, size: usize) -> *mut c_void;
	fn realloc(ptr: *mut c_void, size: usize) -> *mut c_void;
	fn reallocf(ptr: *mut c_void, size: usize) -> *mut c_void;
	fn free(ptr: *mut c_void);
	fn posix_memalign(out: *mut *mut c_void, align: usize, size: usize) -> c_int;
	fn aligned_alloc(align: usize, size: usize) -> *mut c_void;
	fn valloc(size: usize) -> *mut c_void;
	fn pthread_create(
		thread: *mut *mut c_void,
		attr: *const c_void,
		start: extern "C" fn(*mut c_void) -> *mut c_void,
		arg: *mut c_void,
	) -> c_int;
}

#[panic_handler]
fn panic(_: &core::panic::PanicInfo) -> ! {
	// SAFETY: abort takes nothing and never returns.
	unsafe { abort() }
}

/// Writes `line`, NUL-terminated, to stderr, then aborts the process, so the pass fails rather
/// than report counts it could not keep.
fn refuse(line: &[u8]) -> ! {
	// SAFETY: `line` holds its bytes, less the NUL, and stderr takes any.
	unsafe {
		write(2, line.as_ptr().cast(), line.len().saturating_sub(1));
		abort()
	}
}

unsafe extern "C" fn counted_malloc(size: usize) -> *mut c_void {
	// SAFETY: the caller's argument, as it passed it.
	let ptr = unsafe { malloc(size) };
	allocated(ptr, size);
	ptr
}

unsafe extern "C" fn counted_calloc(n: usize, size: usize) -> *mut c_void {
	// SAFETY: the caller's arguments, as it passed them.
	let ptr = unsafe { calloc(n, size) };
	// A product that overflows fails the call, so a block's size never does.
	allocated(ptr, n.wrapping_mul(size));
	ptr
}

/// A `realloc` counts as freeing the old block and allocating the new, as Rust's
/// `GlobalAlloc::realloc` replaces one layout with another; macOS resizes a block to nothing by
/// freeing it for a new, minimal one.
unsafe extern "C" fn counted_realloc(ptr: *mut c_void, size: usize) -> *mut c_void {
	// Uncounted first, since once the allocator frees it another thread may take its address.
	let old = freed(ptr);
	// SAFETY: the caller's arguments, as it passed them.
	let moved = unsafe { realloc(ptr, size) };
	// A failed resize leaves the block as it was.
	if moved.is_null() {
		kept(ptr, old);
	}
	allocated(moved, size);
	moved
}

/// `realloc`, but freeing the block when the resize fails.
unsafe extern "C" fn counted_reallocf(ptr: *mut c_void, size: usize) -> *mut c_void {
	let _ = freed(ptr);
	// SAFETY: the caller's arguments, as it passed them.
	let moved = unsafe { reallocf(ptr, size) };
	allocated(moved, size);
	moved
}

unsafe extern "C" fn counted_free(ptr: *mut c_void) {
	let _ = freed(ptr);
	// SAFETY: the caller's argument, as it passed it.
	unsafe { free(ptr) }
}

unsafe extern "C" fn counted_posix_memalign(out: *mut *mut c_void, align: usize, size: usize) -> c_int {
	// SAFETY: the caller's arguments, as it passed them.
	let failed = unsafe { posix_memalign(out, align, size) };
	if failed == 0 {
		// SAFETY: the call succeeded, so it wrote the block's address to `out`.
		allocated(unsafe { *out }, size);
	}
	failed
}

unsafe extern "C" fn counted_aligned_alloc(align: usize, size: usize) -> *mut c_void {
	// SAFETY: the caller's arguments, as it passed them.
	let ptr = unsafe { aligned_alloc(align, size) };
	allocated(ptr, size);
	ptr
}

unsafe extern "C" fn counted_valloc(size: usize) -> *mut c_void {
	// SAFETY: the caller's argument, as it passed it.
	let ptr = unsafe { valloc(size) };
	allocated(ptr, size);
	ptr
}

// Threads: how many the process has started, which the harness reads around each case's call,
// so a thread the call starts and joins, gone by the call's end, still marks the case as one
// other threads ran beside.

static STARTED: AtomicU64 = AtomicU64::new(0);

/// How many threads `pthread_create` has started in this process.
#[unsafe(no_mangle)]
pub extern "C" fn swole_threads_started() -> u64 {
	STARTED.load(Relaxed)
}

unsafe extern "C" fn counted_pthread_create(
	thread: *mut *mut c_void,
	attr: *const c_void,
	start: extern "C" fn(*mut c_void) -> *mut c_void,
	arg: *mut c_void,
) -> c_int {
	// SAFETY: the caller's arguments, passed on.
	let made = unsafe { pthread_create(thread, attr, start, arg) };
	if made == 0 {
		STARTED.fetch_add(1, Relaxed);
	}
	made
}

/// One of dyld's interpositions: calls any other image makes to `original` reach `replacement`.
#[repr(C)]
struct Interpose {
	replacement: *const c_void,
	original: *const c_void,
}

struct Interposes([Interpose; 9]);

// SAFETY: only dyld reads it, before the process runs.
unsafe impl Sync for Interposes {}

#[used]
#[unsafe(link_section = "__DATA,__interpose")]
static INTERPOSE: Interposes = Interposes([
	Interpose {
		replacement: counted_malloc as *const c_void,
		original: malloc as *const c_void,
	},
	Interpose {
		replacement: counted_calloc as *const c_void,
		original: calloc as *const c_void,
	},
	Interpose {
		replacement: counted_realloc as *const c_void,
		original: realloc as *const c_void,
	},
	Interpose {
		replacement: counted_reallocf as *const c_void,
		original: reallocf as *const c_void,
	},
	Interpose {
		replacement: counted_free as *const c_void,
		original: free as *const c_void,
	},
	Interpose {
		replacement: counted_posix_memalign as *const c_void,
		original: posix_memalign as *const c_void,
	},
	Interpose {
		replacement: counted_aligned_alloc as *const c_void,
		original: aligned_alloc as *const c_void,
	},
	Interpose {
		replacement: counted_valloc as *const c_void,
		original: valloc as *const c_void,
	},
	Interpose {
		replacement: counted_pthread_create as *const c_void,
		original: pthread_create as *const c_void,
	},
]);
