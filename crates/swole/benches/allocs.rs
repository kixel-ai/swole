//! The benchmarks swole's tests count the allocations of, under a global allocator that counts
//! them too: each body counts with it, and its output, dropped once the meter stops, writes what it
//! counted to the directory `SWOLE_TEST_ALLOCS` names, so a test compares the two.

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::UnsafeCell;
use std::hint::black_box;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering::Relaxed};

use swole::bench;

/// System's allocator, counting what it allocates while [`COUNTING`], and the most bytes those
/// allocations held at once: the interposer's measures, from inside the program.
struct Counting;

static COUNTING: AtomicBool = AtomicBool::new(false);
static COUNT: AtomicU64 = AtomicU64::new(0);
static BYTES: AtomicU64 = AtomicU64::new(0);
static LIVE: AtomicU64 = AtomicU64::new(0);
static PEAK: AtomicU64 = AtomicU64::new(0);

/// The blocks allocated while counting, so a free subtracts only theirs: few, since only a
/// single-threaded body records, so a list without allocating is enough.
struct Blocks(UnsafeCell<[(usize, u64); 4096]>);

// SAFETY: only a single-threaded body's allocations touch it; a threaded one's never compares.
unsafe impl Sync for Blocks {}

static BLOCKS: Blocks = Blocks(UnsafeCell::new([(0, 0); 4096]));

fn allocated(ptr: *mut u8, size: usize) {
	if ptr.is_null() || !COUNTING.load(Relaxed) {
		return;
	}
	COUNT.fetch_add(1, Relaxed);
	BYTES.fetch_add(size as u64, Relaxed);
	let live = LIVE.fetch_add(size as u64, Relaxed) + size as u64;
	PEAK.fetch_max(live, Relaxed);
	// SAFETY: see `Blocks`.
	let blocks = unsafe { &mut *BLOCKS.0.get() };
	let free = blocks
		.iter_mut()
		.find(|(at, _)| *at == 0)
		.expect("room for every block");
	*free = (ptr as usize, size as u64);
}

fn freed(ptr: *mut u8) {
	if !COUNTING.load(Relaxed) {
		return;
	}
	// SAFETY: see `Blocks`.
	let blocks = unsafe { &mut *BLOCKS.0.get() };
	if let Some(block) = blocks.iter_mut().find(|(at, _)| *at == ptr as usize) {
		LIVE.fetch_sub(block.1, Relaxed);
		*block = (0, 0);
	}
}

// SAFETY: System's allocator, beside counts that never allocate.
unsafe impl GlobalAlloc for Counting {
	unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
		// SAFETY: the caller's layout.
		let ptr = unsafe { System.alloc(layout) };
		allocated(ptr, layout.size());
		ptr
	}

	unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
		// SAFETY: the caller's layout.
		let ptr = unsafe { System.alloc_zeroed(layout) };
		allocated(ptr, layout.size());
		ptr
	}

	unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
		freed(ptr);
		// SAFETY: the caller's block and layout.
		unsafe { System.dealloc(ptr, layout) }
	}

	unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, size: usize) -> *mut u8 {
		// SAFETY: the caller's block, layout, and size.
		let moved = unsafe { System.realloc(ptr, layout, size) };
		if !moved.is_null() {
			freed(ptr);
			allocated(moved, size);
		}
		moved
	}
}

#[global_allocator]
static ALLOCATOR: Counting = Counting;

/// What a body counted, which writes itself out once dropped, after the meter stops.
struct Counted {
	name: &'static str,
	counts: [u64; 3],
}

impl Drop for Counted {
	fn drop(&mut self) {
		if let Some(dir) = std::env::var_os("SWOLE_TEST_ALLOCS") {
			let [count, bytes, peak] = self.counts;
			let path = std::path::Path::new(&dir).join(self.name);
			std::fs::write(path, format!("{count} {bytes} {peak}")).unwrap();
		}
	}
}

/// Runs `body` counting its allocations, as the interposer counts the call around it.
fn counted<T>(name: &'static str, body: impl FnOnce() -> T) -> (T, Counted) {
	for count in [&COUNT, &BYTES, &LIVE, &PEAK] {
		count.store(0, Relaxed);
	}
	// SAFETY: see `Blocks`.
	unsafe { (*BLOCKS.0.get()).fill((0, 0)) };
	COUNTING.store(true, Relaxed);
	let out = body();
	COUNTING.store(false, Relaxed);
	let counts = [COUNT.load(Relaxed), BYTES.load(Relaxed), PEAK.load(Relaxed)];
	(out, Counted { name, counts })
}

/// A vector grown a push at a time, reallocating as it doubles, and freed.
#[bench]
fn vec() -> (usize, Counted) {
	counted("vec", || {
		let mut v = Vec::new();
		for i in 0..1000u64 {
			v.push(black_box(i));
		}
		v.len()
	})
}

/// Boxes, half of them kept past the call, so its peak is not what it ends holding.
#[bench]
fn boxes() -> (Vec<Box<[u8; 100]>>, Counted) {
	counted("boxes", || {
		let mut kept = Vec::with_capacity(8);
		for i in 0..16u8 {
			let block = Box::new([i; 100]);
			if i % 2 == 0 {
				kept.push(block);
			} else {
				drop(black_box(block));
			}
		}
		kept
	})
}

/// A string grown by formatting, shrunk to fit, and returned.
#[bench]
fn string() -> (String, Counted) {
	counted("string", || {
		let mut s = String::new();
		for i in 0..200 {
			s.push_str(&format!("{i},"));
		}
		s.shrink_to_fit();
		s
	})
}

/// Blocks aligned past what malloc gives, zeroed, and reallocated: `posix_memalign` and `calloc`
/// beside `malloc`. An over-aligned block grows by a new block and a copy, so both are live for a
/// moment, which the interposer sees and a `GlobalAlloc::realloc` does not: it shrinks back before
/// the call's peak, which both then agree on.
#[bench]
fn aligned() -> (usize, Counted) {
	counted("aligned", || {
		let layout = Layout::from_size_align(256, 64).unwrap();
		// SAFETY: a layout of nonzero size, resized and freed with the layout it then has.
		let first = unsafe {
			let block = black_box(std::alloc::alloc_zeroed(layout));
			let grown = black_box(std::alloc::realloc(block, layout, 4096));
			let first = *grown as usize;
			std::alloc::dealloc(grown, Layout::from_size_align(4096, 64).unwrap());
			first
		};
		#[repr(align(4096))]
		struct Page([u8; 4096]);
		let page = black_box(Box::new(Page([1; 4096])));
		let zeroed = black_box(vec![0u8; 1 << 16]);
		black_box(&page.0[0]);
		first + zeroed.len()
	})
}

/// Allocations on another thread, which make the count a sample.
#[bench]
fn spawned() -> usize {
	std::thread::spawn(|| black_box(vec![1u8; 1000]).len()).join().unwrap()
}

/// A thread its setup started that never allocates once the setup returns, beside which the call
/// still counts as a sample: what the interposer counts does not tell whose allocation it was.
fn idle() -> std::thread::JoinHandle<()> {
	// A barrier waits without allocating, so the thread has made whatever starting it allocates
	// before the call.
	let started = std::sync::Arc::new(std::sync::Barrier::new(2));
	let thread = std::thread::spawn({
		let started = started.clone();
		move || {
			started.wait();
			loop {
				std::thread::park();
			}
		}
	});
	started.wait();
	thread
}

#[bench(setup = idle)]
fn beside(_idle: &std::thread::JoinHandle<()>) -> Vec<u8> {
	vec![1; 1000]
}

fn main() {
	swole::main();
}
