//! Heap allocation counts from swole's allocation interposer, which swole preloads into a pass of
//! its own: the meter has it count over each call, and reads what it counted.

use std::ffi::{CStr, c_void};

/// The interposer's controls, found in the process by name.
#[derive(Clone, Copy)]
pub struct Hooks {
	start: extern "C" fn(),
	stop: extern "C" fn(),
	read: unsafe extern "C" fn(*mut u64),
}

/// What the interposer counted over a window: allocations, their bytes, and the most bytes the
/// window's own allocations held at once.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Counted {
	pub count: u64,
	pub bytes: u64,
	pub peak: u64,
}

#[cfg(target_os = "linux")]
const RTLD_DEFAULT: *mut c_void = std::ptr::null_mut();
#[cfg(not(target_os = "linux"))]
const RTLD_DEFAULT: *mut c_void = -2isize as *mut c_void;

unsafe extern "C" {
	fn dlsym(handle: *mut c_void, symbol: *const std::ffi::c_char) -> *mut c_void;
}

/// The function `name` some loaded library defines, if any.
fn find(name: &CStr) -> Option<*mut c_void> {
	// SAFETY: the name is a C string; RTLD_DEFAULT searches every loaded library.
	let found = unsafe { dlsym(RTLD_DEFAULT, name.as_ptr()) };
	(!found.is_null()).then_some(found)
}

impl Hooks {
	/// The interposer's controls, or why the process has none.
	pub fn find() -> Result<Self, String> {
		let missing = || {
			"SWOLE_HEAP counts allocations only under swole's allocation interposer, which this process never loaded: \
			 a static executable, or one whose environment drops LD_PRELOAD, bypasses it"
				.to_owned()
		};
		let start = find(c"swole_heap_start").ok_or_else(missing)?;
		let stop = find(c"swole_heap_stop").ok_or_else(missing)?;
		let read = find(c"swole_heap_read").ok_or_else(missing)?;
		// SAFETY: the interposer defines each as a function of this signature.
		unsafe {
			Ok(Self {
				start: std::mem::transmute::<*mut c_void, extern "C" fn()>(start),
				stop: std::mem::transmute::<*mut c_void, extern "C" fn()>(stop),
				read: std::mem::transmute::<*mut c_void, unsafe extern "C" fn(*mut u64)>(read),
			})
		}
	}

	/// Zeroes the counts and starts counting.
	#[inline(always)]
	pub fn start(&self) {
		(self.start)();
	}

	/// Stops counting.
	#[inline(always)]
	pub fn stop(&self) {
		(self.stop)();
	}

	/// What the last window counted.
	pub fn read(&self) -> Counted {
		let mut out = [0u64; 3];
		// SAFETY: `out` holds the three counts the interposer writes.
		unsafe { (self.read)(out.as_mut_ptr()) };
		let [count, bytes, peak] = out;
		Counted { count, bytes, peak }
	}

	/// Whether an allocation through the program's global allocator reaches the interposer: one
	/// that serves memory of its own, as a `#[global_allocator]` that maps pages itself or a static
	/// allocator replacing `malloc` in the executable does, bypasses it, and its counts would read
	/// as none.
	pub fn reaches(&self) -> bool {
		let layout = std::alloc::Layout::new::<[u64; 4]>();
		self.start();
		// SAFETY: a layout of nonzero size, freed once with the same layout.
		unsafe {
			let block = std::hint::black_box(std::alloc::alloc(layout));
			if !block.is_null() {
				std::alloc::dealloc(block, layout);
			}
		}
		self.stop();
		self.read().count > 0
	}
}
