//! What the operating system accounts a process for, from `getrusage`, on Linux and macOS.

/// What the process used over a window, but for its peak memory, which is the process's own.
#[derive(Clone, Copy, Default)]
pub struct Usage {
	pub user_ns: u64,
	pub system_ns: u64,
	/// The most memory the process has held resident, in bytes, at any time it ran.
	pub max_rss: u64,
	pub minor_faults: u64,
	pub major_faults: u64,
	pub voluntary_switches: u64,
	pub involuntary_switches: u64,
}

impl Usage {
	/// What the whole process, every thread of it, has used so far.
	pub fn now() -> Self {
		let mut usage = Rusage::default();
		// SAFETY: `usage` is a `struct rusage`, which the call fills.
		let done = unsafe { getrusage(RUSAGE_SELF, &mut usage) };
		assert_eq!(done, 0, "getrusage takes RUSAGE_SELF");
		let ns = |time: Timeval| time.sec as u64 * 1_000_000_000 + time.usec as u64 * 1_000;
		Self {
			user_ns: ns(usage.utime),
			system_ns: ns(usage.stime),
			max_rss: usage.maxrss as u64 * MAX_RSS_UNIT,
			minor_faults: usage.minflt as u64,
			major_faults: usage.majflt as u64,
			voluntary_switches: usage.nvcsw as u64,
			involuntary_switches: usage.nivcsw as u64,
		}
	}

	/// What was used from `start` to `self`, added to `total`, whose peak memory becomes this one's.
	pub fn add_since(self, start: Self, total: &mut Self) {
		total.user_ns += self.user_ns - start.user_ns;
		total.system_ns += self.system_ns - start.system_ns;
		total.max_rss = self.max_rss;
		total.minor_faults += self.minor_faults - start.minor_faults;
		total.major_faults += self.major_faults - start.major_faults;
		total.voluntary_switches += self.voluntary_switches - start.voluntary_switches;
		total.involuntary_switches += self.involuntary_switches - start.involuntary_switches;
	}

	/// The usage as the harness prints it, after `swole rusage `.
	pub fn fields(&self) -> String {
		format!(
			"{} {} {} {} {} {} {}",
			self.user_ns,
			self.system_ns,
			self.max_rss,
			self.minor_faults,
			self.major_faults,
			self.voluntary_switches,
			self.involuntary_switches
		)
	}
}

const RUSAGE_SELF: i32 = 0;

/// Linux counts peak memory in KiB, macOS in bytes.
#[cfg(target_os = "macos")]
const MAX_RSS_UNIT: u64 = 1;
#[cfg(not(target_os = "macos"))]
const MAX_RSS_UNIT: u64 = 1024;

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct Timeval {
	sec: i64,
	#[cfg(not(target_os = "macos"))]
	usec: i64,
	/// macOS's is a 32-bit field, then padding.
	#[cfg(target_os = "macos")]
	usec: i32,
	#[cfg(target_os = "macos")]
	_pad: i32,
}

/// `struct rusage`, whose fields after the times are each a `long`.
#[repr(C)]
#[derive(Default)]
struct Rusage {
	utime: Timeval,
	stime: Timeval,
	maxrss: i64,
	ixrss: i64,
	idrss: i64,
	isrss: i64,
	minflt: i64,
	majflt: i64,
	nswap: i64,
	inblock: i64,
	oublock: i64,
	msgsnd: i64,
	msgrcv: i64,
	nsignals: i64,
	nvcsw: i64,
	nivcsw: i64,
}

unsafe extern "C" {
	fn getrusage(who: i32, usage: *mut Rusage) -> i32;
}
