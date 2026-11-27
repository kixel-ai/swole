//! What the harness learns of a macOS process's threads: how many it has, from the kernel, and
//! how many swole's inserted interposer saw it start.

use std::ffi::{c_int, c_void};

/// `proc_pidinfo`'s flavor for a task's `proc_taskinfo`.
const PROC_PIDTASKINFO: c_int = 4;

/// `struct proc_taskinfo`'s size, and where its `pti_threadnum` sits.
const TASKINFO: usize = 96;
const THREADNUM: usize = 84;

const RTLD_DEFAULT: *mut c_void = -2isize as *mut c_void;

unsafe extern "C" {
	fn getpid() -> c_int;
	fn proc_pidinfo(pid: c_int, flavor: c_int, arg: u64, buffer: *mut c_void, size: c_int) -> c_int;
	fn dlsym(handle: *mut c_void, symbol: *const std::ffi::c_char) -> *mut c_void;
}

/// How many threads the process has now.
pub fn threads() -> Result<usize, String> {
	let mut info = [0u8; TASKINFO];
	// SAFETY: `info` holds the `proc_taskinfo` the call writes, as its size says.
	let wrote = unsafe {
		proc_pidinfo(
			getpid(),
			PROC_PIDTASKINFO,
			0,
			info.as_mut_ptr().cast(),
			TASKINFO as c_int,
		)
	};
	if wrote != TASKINFO as c_int {
		return Err(format!(
			"cannot read this process's task info: {}",
			std::io::Error::last_os_error()
		));
	}
	let threads = i32::from_ne_bytes(info[THREADNUM..THREADNUM + 4].try_into().expect("four bytes"));
	Ok(threads.max(1) as usize)
}

/// How many threads swole's interposer saw the process start, or `None` without it.
pub fn started() -> Option<u64> {
	static STARTED: std::sync::OnceLock<Option<extern "C" fn() -> u64>> = std::sync::OnceLock::new();
	let started = *STARTED.get_or_init(|| {
		// SAFETY: the name is a C string; RTLD_DEFAULT searches every loaded image.
		let symbol = unsafe { dlsym(RTLD_DEFAULT, c"swole_threads_started".as_ptr()) };
		// SAFETY: the interposer defines the symbol as a function of this signature.
		(!symbol.is_null()).then(|| unsafe { std::mem::transmute::<*mut c_void, extern "C" fn() -> u64>(symbol) })
	});
	started.map(|f| f())
}
