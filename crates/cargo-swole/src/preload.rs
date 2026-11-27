//! The library every counted process preloads, built from `preload.rs` beside this crate: it
//! hands the process a fixed-seed stream for randomness and a clock that advances only as the
//! process reads it and waits, so a count repeats; and the same library built to count the
//! threads a process starts alone, for `bench`'s counting passes, which keep the host's clock and
//! randomness, as its timing does.

use std::path::{Path, PathBuf};

use crate::fingerprint::Fnv;

/// The library, which build.rs built for this target.
const LIBRARY: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/preload.so"));

/// The library built to count threads alone.
const THREADS: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/threads.so"));

/// What the library logs to Valgrind once it loads: preload.rs's `MARKER`, which a test keeps the
/// same.
const MARKER: &str = "swole preload: deterministic randomness and clock";

/// The library's hash, which the fingerprint holds, since its code runs inside what is counted.
pub fn hash() -> String {
	digest(LIBRARY)
}

fn digest(library: &[u8]) -> String {
	let mut hash = Fnv::default();
	hash.bytes(library);
	format!("{:012x}", hash.0 >> 16)
}

/// Writes the library where every run on this host finds it at the same path, under the user's
/// cache, and returns that path. Named by its hash, it is rewritten only when missing or altered.
pub fn install() -> Result<PathBuf, String> {
	write("preload", LIBRARY)
}

/// Writes the library that counts threads alone, as [`install`] writes the whole one.
pub fn threads() -> Result<PathBuf, String> {
	write("threads", THREADS)
}

fn write(name: &str, library: &[u8]) -> Result<PathBuf, String> {
	if library.is_empty() {
		return Err("swole's preload is built for x86_64 Linux alone, where it counts".to_owned());
	}
	let path = crate::cache()?.join(format!("{name}-{}.so", digest(library)));
	// The loader splits LD_PRELOAD at each.
	if path.to_string_lossy().contains([':', ' ']) {
		return Err(format!(
			"the preload's path {} holds a ':' or a space, which LD_PRELOAD cannot: point XDG_CACHE_HOME elsewhere",
			path.display()
		));
	}
	if std::fs::read(&path).is_ok_and(|bytes| bytes == library) {
		return Ok(path);
	}
	// Written aside, then renamed, so a run beside this one never loads a partial library.
	let partial = path.with_extension(format!("so.{}", std::process::id()));
	std::fs::write(&partial, library).map_err(|e| format!("cannot write {}: {e}", partial.display()))?;
	std::fs::rename(&partial, &path).map_err(|e| format!("cannot write {}: {e}", path.display()))?;
	Ok(path)
}

/// Fails unless the process `pid` of a job, whose Valgrind log is in `dir`, loaded the library.
pub fn check(dir: &Path, pid: &str, label: &str) -> Result<(), String> {
	let log = dir.join(format!("valgrind.{pid}.log"));
	let text = std::fs::read_to_string(&log).map_err(|e| format!("cannot read {}: {e}", log.display()))?;
	if text.contains(MARKER) {
		return Ok(());
	}
	Err(format!(
		"{label} measured a benchmark in a process that never loaded swole's preload, so its count \
		 follows the host's randomness and clock: a static executable, one whose environment drops \
		 LD_PRELOAD, or a glibc before 2.34 bypasses it; its output is in {}",
		dir.display()
	))
}

#[cfg(all(test, target_os = "linux", target_arch = "x86_64", target_env = "gnu"))]
mod tests {
	use super::*;

	#[test]
	fn the_library_logs_the_marker() {
		assert!(
			LIBRARY.windows(MARKER.len()).any(|window| window == MARKER.as_bytes()),
			"preload.rs's MARKER is not {MARKER:?}"
		);
	}
}
