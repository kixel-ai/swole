//! A benchmark whose global allocator never calls malloc, as one that maps memory of its own or a
//! static allocator does, so swole's allocation interposer sees none of its allocations and must
//! say it has no counts rather than report none.

use std::alloc::{GlobalAlloc, Layout};
use std::cell::UnsafeCell;
use std::sync::atomic::{AtomicUsize, Ordering::Relaxed};

/// A bump allocator over a static arena, which frees only its latest block, as a benchmark timed
/// over many calls frees what each call returned.
struct Bump;

const ARENA: usize = 64 << 20;

#[repr(align(4096))]
struct Arena(UnsafeCell<[u8; ARENA]>);

// SAFETY: each allocation takes a range of its own, by an atomic bump.
unsafe impl Sync for Arena {}

static MEMORY: Arena = Arena(UnsafeCell::new([0; ARENA]));
static USED: AtomicUsize = AtomicUsize::new(0);

// SAFETY: each block is a range of the arena no other live block holds, aligned as asked.
unsafe impl GlobalAlloc for Bump {
	unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
		let base = MEMORY.0.get() as usize;
		let mut at = USED.load(Relaxed);
		loop {
			let start = (base + at).next_multiple_of(layout.align()) - base;
			let end = start + layout.size();
			if end > ARENA {
				return std::ptr::null_mut();
			}
			match USED.compare_exchange_weak(at, end, Relaxed, Relaxed) {
				Ok(_) => return (base + start) as *mut u8,
				Err(now) => at = now,
			}
		}
	}

	unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
		let start = ptr as usize - MEMORY.0.get() as usize;
		let _ = USED.compare_exchange(start + layout.size(), start, Relaxed, Relaxed);
	}
}

#[global_allocator]
static ALLOCATOR: Bump = Bump;

#[swole::bench]
fn bumped() -> Vec<u64> {
	(0..1000).collect()
}

fn main() {
	swole::main();
}
