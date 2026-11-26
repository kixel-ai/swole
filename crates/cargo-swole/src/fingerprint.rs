//! The environment a count is comparable within.
//!
//! Valgrind presents a CPU derived from the host's, and code that picks a path by CPU
//! feature, such as glibc's string functions or Rust's runtime detection, counts
//! differently on another. So does another Valgrind, and glibc's tunables, which can mask
//! features under the same CPU, and the preload every count runs with, whose code runs inside
//! what is counted. Each count names the fingerprint of all of them, so counts from two
//! environments never share a series.

use std::process::Command;

/// The inputs of a fingerprint, and its label.
pub struct Fingerprint {
	pub arch: &'static str,
	/// `valgrind --version`.
	pub valgrind: String,
	/// `GLIBC_TUNABLES` as the benchmarks inherit it, when set.
	pub tunables: Option<String>,
	/// What the CPU identifies as under Valgrind, one line a leaf.
	pub cpu: String,
	/// The hash of the library every count preloads, its seed and clock compiled in.
	pub preload: String,
}

impl Fingerprint {
	/// Probes this environment: runs this executable's [`cpu`] under the same Valgrind tool
	/// and options the benchmarks run under.
	pub fn probe() -> Result<Self, String> {
		let arch = std::env::consts::ARCH;
		if arch != "x86_64" {
			return Err(format!(
				"{arch} is not supported: only x86_64's CPU, as Valgrind presents it, is fingerprinted"
			));
		}
		let version = output(
			Command::new(crate::valgrind_path()?).arg("--version"),
			"valgrind --version",
		)?;
		let exe = crate::perf::helper()?;
		let mut probe = crate::valgrind()?;
		probe.arg("--callgrind-out-file=/dev/null").arg(exe).arg(crate::PROBE);
		let cpu = output(&mut probe, "the CPU probe under Valgrind")?;
		Ok(Self {
			arch,
			valgrind: version.trim().to_owned(),
			tunables: std::env::var("GLIBC_TUNABLES").ok(),
			cpu,
			preload: crate::preload::hash(),
		})
	}

	/// Every input, a line each, but the CPU's, a line a leaf.
	pub fn describe(&self) -> String {
		format!(
			"arch: {}\nvalgrind: {}\nGLIBC_TUNABLES: {}\npreload: {}\ncpu:\n{}",
			self.arch,
			self.valgrind,
			self.tunables.as_deref().unwrap_or("(unset)"),
			self.preload,
			self.cpu
		)
	}

	/// The fingerprint as a measure names it: the architecture and a hash of every input.
	pub fn label(&self) -> String {
		let mut hash = Fnv::default();
		hash.write(self.arch);
		hash.write(&self.valgrind);
		match &self.tunables {
			Some(tunables) => hash.write(&format!("GLIBC_TUNABLES={tunables}")),
			None => hash.write("GLIBC_TUNABLES unset"),
		}
		hash.write(&self.cpu);
		hash.write(&self.preload);
		format!("{}-{:012x}", self.arch, hash.0 >> 16)
	}
}

/// The environment a native measure, quick's count or bench's time, is comparable within: the
/// CPU's model, how many CPUs swole may run on, and what the host lets swole count, the perf
/// tier, PMU, and counters its probe found, so a series never mixes two kinds of host or two
/// sets of counters; the loaded libc and glibc tunables every benchmark inherits, which pick its
/// string and memory routines; and for quick, the preload, whose code runs inside each count.
#[derive(Clone)]
pub struct Native {
	pub arch: &'static str,
	/// The CPU's model, as the system describes it.
	pub model: String,
	/// How many CPUs swole may run on.
	pub cpus: usize,
	pub host: crate::perf::Host,
	/// The loaded libc identity and hash on Linux, so counts never mix across libc builds.
	pub libc: Option<String>,
	/// `GLIBC_TUNABLES`, as every benchmark inherits it.
	pub tunables: Option<String>,
	/// The preload's hash, for a count that runs with it.
	pub preload: Option<String>,
}

impl Native {
	/// Probes this host for `mode`, quick or bench.
	pub fn probe(mode: crate::Mode) -> Result<Self, String> {
		Ok(Self {
			arch: std::env::consts::ARCH,
			model: model()?,
			cpus: std::thread::available_parallelism()
				.map_err(|e| format!("cannot count the CPUs: {e}"))?
				.get(),
			host: crate::perf::Host::probe(mode)?,
			libc: libc()?,
			tunables: std::env::var("GLIBC_TUNABLES").ok(),
			preload: (mode == crate::Mode::Quick).then(crate::preload::hash),
		})
	}

	/// Returns a copy of this host description using the loaded libc reported by the benchmarks.
	pub fn with_libc(&self, libc: Option<String>) -> Self {
		let mut updated = self.clone();
		updated.libc = libc;
		updated
	}

	/// Every input, a line each, as the label hashes them.
	pub fn describe(&self) -> String {
		let mut out = format!(
			"arch: {}
CPUs: {}
GLIBC_TUNABLES: {}
",
			self.arch,
			self.cpus,
			self.tunables.as_deref().unwrap_or("(unset)")
		);
		if let Some(libc) = &self.libc {
			out.push_str(&format!("libc: {libc}\n"));
		}
		out.push_str(&self.host.describe());
		if let Some(preload) = &self.preload {
			out.push_str(&format!(
				"preload: {preload}
"
			));
		}
		out.push_str(&format!("model:\n{}", self.model));
		out
	}

	/// The fingerprint as a measure names it: the architecture and a hash of every input.
	pub fn label(&self) -> String {
		let mut hash = Fnv::default();
		hash.write(&self.describe());
		format!("{}-{:012x}", self.arch, hash.0 >> 16)
	}
}

/// The mapped libc on Linux, read directly from its in-memory ELF `NT_GNU_BUILD_ID` note (plus
/// `gnu_get_libc_version()` when on glibc), so counts never collide across different libc builds
/// or race with on-disk package updates.
#[cfg(target_os = "linux")]
fn libc() -> Result<Option<String>, String> {
	use std::ffi::{CStr, c_char, c_int, c_void};
	use std::fmt::Write as _;

	#[cfg(target_pointer_width = "64")]
	#[repr(C)]
	struct Phdr {
		p_type: u32,
		p_flags: u32,
		p_offset: u64,
		p_vaddr: u64,
		p_paddr: u64,
		p_filesz: u64,
		p_memsz: u64,
		p_align: u64,
	}
	#[cfg(target_pointer_width = "32")]
	#[repr(C)]
	struct Phdr {
		p_type: u32,
		p_offset: u32,
		p_vaddr: u32,
		p_paddr: u32,
		p_filesz: u32,
		p_memsz: u32,
		p_flags: u32,
		p_align: u32,
	}
	#[repr(C)]
	struct DlPhdrInfo {
		dlpi_addr: usize,
		dlpi_name: *const c_char,
		dlpi_phdr: *const Phdr,
		dlpi_phnum: u16,
	}
	unsafe extern "C" {
		fn getpid() -> i32;
		fn dl_iterate_phdr(
			callback: unsafe extern "C" fn(*mut DlPhdrInfo, usize, *mut c_void) -> c_int,
			data: *mut c_void,
		) -> c_int;
		#[cfg(target_env = "gnu")]
		fn gnu_get_libc_version() -> *const c_char;
	}

	const PT_LOAD: u32 = 1;
	const PT_NOTE: u32 = 4;
	const NT_GNU_BUILD_ID: u32 = 3;

	fn align_up(value: usize, align: usize) -> Option<usize> {
		let mask = align.checked_sub(1)?;
		value.checked_add(mask).map(|v| v & !mask)
	}

	struct State {
		target: usize,
		own: usize,
		result: Option<Result<String, &'static str>>,
	}

	unsafe extern "C" fn visit(info: *mut DlPhdrInfo, _size: usize, data: *mut c_void) -> c_int {
		// SAFETY: `dl_iterate_phdr` passes valid `info` and our `State` pointer.
		let info = unsafe { &*info };
		let state = unsafe { &mut *(data as *mut State) };
		if info.dlpi_phdr.is_null() || info.dlpi_phnum == 0 {
			return 0;
		}
		// SAFETY: `dlpi_phdr` points to `dlpi_phnum` ELF program headers.
		let phdrs = unsafe { std::slice::from_raw_parts(info.dlpi_phdr, usize::from(info.dlpi_phnum)) };
		let contains = |addr: usize| {
			phdrs.iter().any(|p| {
				if p.p_type != PT_LOAD || p.p_memsz == 0 {
					return false;
				}
				let start = info.dlpi_addr.wrapping_add(p.p_vaddr as usize);
				let end = start.saturating_add(p.p_memsz as usize);
				addr >= start && addr < end
			})
		};
		if !contains(state.target) {
			return 0;
		}
		if contains(state.own) {
			state.result = Some(Ok("static".to_owned()));
			return 1;
		}
		for p in phdrs {
			if p.p_type != PT_NOTE || p.p_memsz < 12 {
				continue;
			}
			let note_vaddr = p.p_vaddr as usize;
			let note_len = p.p_memsz as usize;
			let Some(note_end_vaddr) = note_vaddr.checked_add(note_len) else {
				continue;
			};
			let mapped = phdrs.iter().any(|load| {
				load.p_type == PT_LOAD
					&& note_vaddr >= load.p_vaddr as usize
					&& note_end_vaddr <= (load.p_vaddr as usize).saturating_add(load.p_memsz as usize)
			});
			if !mapped {
				continue;
			}
			let note_ptr = info.dlpi_addr.wrapping_add(note_vaddr) as *const u8;
			if note_ptr.is_null() {
				continue;
			}
			// SAFETY: the `PT_NOTE` segment lies within a mapped `PT_LOAD` segment of this loaded image.
			let bytes = unsafe { std::slice::from_raw_parts(note_ptr, note_len) };
			let align = if p.p_align == 8 { 8usize } else { 4usize };
			let mut offset = 0usize;
			while let Some(header) = bytes.get(offset..offset + 12) {
				let namesz = u32::from_ne_bytes(header[0..4].try_into().expect("4 bytes")) as usize;
				let descsz = u32::from_ne_bytes(header[4..8].try_into().expect("4 bytes")) as usize;
				let ntype = u32::from_ne_bytes(header[8..12].try_into().expect("4 bytes"));
				let Some(name_start) = offset.checked_add(12) else {
					break;
				};
				let Some(name_end) = name_start.checked_add(namesz) else {
					break;
				};
				let Some(desc_start) = align_up(name_end, align) else {
					break;
				};
				let Some(desc_end) = desc_start.checked_add(descsz) else {
					break;
				};
				if desc_end > bytes.len() {
					break;
				}
				if ntype == NT_GNU_BUILD_ID && &bytes[name_start..name_end] == b"GNU\0" && descsz > 0 {
					let mut hex = String::with_capacity(descsz * 2);
					for &byte in &bytes[desc_start..desc_end] {
						let _ = write!(hex, "{byte:02x}");
					}
					state.result = Some(Ok(format!("build-id {hex}")));
					return 1;
				}
				let Some(next) = align_up(desc_end, align) else {
					break;
				};
				if next <= offset {
					break;
				}
				offset = next;
			}
		}
		state.result = Some(Err("mapped libc image has no GNU build ID"));
		1
	}

	let mut state = State {
		target: getpid as *const () as usize,
		own: libc as *const () as usize,
		result: None,
	};
	// SAFETY: `visit` reads only valid program headers and writes into `state`.
	unsafe {
		dl_iterate_phdr(visit, (&mut state as *mut State).cast());
	}
	let id = match state.result {
		Some(Ok(id)) => id,
		Some(Err(why)) => return Err(format!("cannot identify mapped libc: {why}")),
		None => return Err("cannot identify mapped libc: getpid not found in any loaded image".to_owned()),
	};
	if id == "static" {
		return Ok(Some(id));
	}
	#[cfg(target_env = "gnu")]
	{
		// SAFETY: `gnu_get_libc_version` returns a static NUL-terminated string.
		let ver = unsafe { CStr::from_ptr(gnu_get_libc_version()) }.to_string_lossy();
		if !ver.is_empty() && !ver.chars().any(char::is_whitespace) {
			return Ok(Some(format!("glibc {ver} ({id})")));
		}
	}
	Ok(Some(id))
}

#[cfg(not(target_os = "linux"))]
fn libc() -> Result<Option<String>, String> {
	Ok(None)
}

/// Validates a benchmark process's `swole libc <record>` identity read from its mapped ELF image:
/// `static`, `build-id <hex>`, or `glibc <version> (build-id <hex>)`.
pub fn validate_libc_record(record: &str) -> Result<String, String> {
	let is_hex = |s: &str| !s.is_empty() && s.bytes().all(|b| b.is_ascii_hexdigit());
	if record == "static" {
		return Ok(record.to_owned());
	}
	if let Some(hex) = record.strip_prefix("build-id ")
		&& is_hex(hex)
	{
		return Ok(record.to_owned());
	}
	if let Some(rest) = record.strip_prefix("glibc ")
		&& let Some((version, tail)) = rest.split_once(" (build-id ")
		&& let Some(hex) = tail.strip_suffix(')')
		&& !version.is_empty()
		&& !version.chars().any(char::is_whitespace)
		&& is_hex(hex)
	{
		return Ok(record.to_owned());
	}
	Err(format!("a harness target printed malformed libc identity {record:?}"))
}

/// Validates that every benchmark process reported a verified mapped libc identity (when required,
/// i.e. on Linux) and that all reported identities match a single libc build.
pub fn require_single_libc<'a>(
	seen: impl IntoIterator<Item = (&'a str, Option<&'a str>)>,
	required: bool,
) -> Result<Option<String>, String> {
	let mut unique = std::collections::BTreeSet::new();
	for (label, libc) in seen {
		match libc {
			Some(libc) => {
				unique.insert(libc);
			}
			None if required => {
				return Err(format!(
					"{label} did not report its mapped libc identity (its swole harness may predate libc verification), \
					 so swole cannot verify its fingerprint"
				));
			}
			None => {}
		}
	}
	match unique.len() {
		0 => Ok(None),
		1 => Ok(unique.into_iter().next().map(str::to_owned)),
		_ => {
			let list: Vec<&str> = unique.into_iter().collect();
			Err(format!(
				"benchmarks loaded different libc builds ({}), so they cannot share one fingerprint",
				list.join(", ")
			))
		}
	}
}

/// The CPU's model as Linux describes each logical CPU, by the fields that name it.
#[cfg(target_os = "linux")]
fn model() -> Result<String, String> {
	const MODEL: [&str; 9] = [
		"vendor_id",
		"cpu family",
		"model",
		"model name",
		"stepping",
		"CPU implementer",
		"CPU variant",
		"CPU part",
		"CPU revision",
	];
	let info = std::fs::read_to_string("/proc/cpuinfo").map_err(|e| format!("cannot read /proc/cpuinfo: {e}"))?;
	// Each line once: a host's CPUs mostly repeat each other's, and `cpus` counts them.
	let mut model = String::new();
	for line in info
		.lines()
		.filter(|line| line.split_once(':').is_some_and(|(key, _)| MODEL.contains(&key.trim())))
	{
		if !model.lines().any(|seen| seen == line) {
			model.push_str(line);
			model.push('\n');
		}
	}
	if model.is_empty() {
		return Err("/proc/cpuinfo names no CPU model".to_owned());
	}
	Ok(model)
}

/// The CPU's model as macOS names it.
#[cfg(target_os = "macos")]
fn model() -> Result<String, String> {
	let out = Command::new("sysctl")
		.args(["-n", "machdep.cpu.brand_string"])
		.output()
		.map_err(|e| format!("cannot run sysctl: {e}"))?;
	if !out.status.success() {
		return Err(format!("sysctl failed ({})", out.status));
	}
	String::from_utf8(out.stdout).map_err(|_| "sysctl named the CPU in something not UTF-8".to_owned())
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn model() -> Result<String, String> {
	Err("swole names CPUs on Linux and macOS alone".to_owned())
}

/// Runs `command`, returning its stdout, or failing with its stderr.
fn output(command: &mut Command, what: &str) -> Result<String, String> {
	let out = command.output().map_err(|e| format!("cannot run {what}: {e}"))?;
	if !out.status.success() {
		return Err(format!(
			"{what} failed ({}): {}",
			out.status,
			String::from_utf8_lossy(&out.stderr)
		));
	}
	String::from_utf8(out.stdout).map_err(|_| format!("{what} printed something not UTF-8"))
}

/// The CPU's identification, every CPUID leaf and its first 16 subleaves, one line each,
/// with the fields that name the core it ran on masked, since those change from one run to
/// the next on a host that passes them through.
#[cfg(target_arch = "x86_64")]
pub fn cpu() -> String {
	use std::arch::x86_64::__cpuid_count;
	use std::fmt::Write;

	let mut out = String::new();
	for base in [0, 0x8000_0000u32] {
		let max = __cpuid_count(base, 0).eax;
		// A leaf range past its base is not one: on a CPU without extended leaves, the
		// extended base answers as the highest basic leaf.
		let max = if max < base { base } else { max.min(base + 0xff) };
		for leaf in base..=max {
			for sub in 0..16 {
				let mut r = __cpuid_count(leaf, sub);
				match leaf {
					// The initial APIC ID.
					1 => r.ebx &= 0x00ff_ffff,
					// The x2APIC ID.
					0xb | 0x1f => r.edx = 0,
					// A hybrid CPU's core type.
					0x1a => r.eax = 0,
					// AMD's extended APIC ID and its core and node IDs.
					0x8000_001e => (r.eax, r.ebx, r.ecx) = (0, r.ebx & !0xff, r.ecx & !0xff),
					_ => {}
				}
				writeln!(
					out,
					"{leaf:08x}.{sub:x} {:08x} {:08x} {:08x} {:08x}",
					r.eax, r.ebx, r.ecx, r.edx
				)
				.expect("a String takes every write");
			}
		}
	}
	out
}

#[cfg(not(target_arch = "x86_64"))]
pub fn cpu() -> String {
	unreachable!("probe refuses every other architecture first")
}

/// 64-bit FNV-1a, a stable hash with no dependency; a label needs no resistance to collisions
/// someone engineers.
pub struct Fnv(pub u64);

impl Default for Fnv {
	fn default() -> Self {
		Self(0xcbf2_9ce4_8422_2325)
	}
}

impl Fnv {
	/// Hashes `field`, then a separator no field holds, so no two lists of fields collide by
	/// where one ends.
	pub fn write(&mut self, field: &str) {
		self.bytes(field.as_bytes());
	}

	/// Hashes `field` as [`write`](Self::write) does, for bytes that are not text.
	pub fn bytes(&mut self, field: &[u8]) {
		for &byte in field.iter().chain(&[0]) {
			self.0 = (self.0 ^ u64::from(byte)).wrapping_mul(0x0100_0000_01b3);
		}
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	fn fingerprint() -> Fingerprint {
		Fingerprint {
			arch: "x86_64",
			valgrind: "valgrind-3.27.1".to_owned(),
			tunables: None,
			cpu: "00000000.0 0000000d 756e6547 6c65746e 49656e69\n".to_owned(),
			preload: "0123456789ab".to_owned(),
		}
	}

	#[test]
	fn the_label_names_the_architecture_and_is_stable() {
		let label = fingerprint().label();
		assert_eq!(label, fingerprint().label());
		assert!(
			label.starts_with("x86_64-") && label.len() == "x86_64-".len() + 12,
			"{label}"
		);
	}

	#[test]
	fn every_input_moves_the_label() {
		let base = fingerprint().label();
		let mut tuned = fingerprint();
		tuned.tunables = Some(String::new());
		let mut other = fingerprint();
		other.valgrind = "valgrind-3.28.0".to_owned();
		let mut cpu = fingerprint();
		cpu.cpu.push('\n');
		let mut preload = fingerprint();
		preload.preload = "ba9876543210".to_owned();
		for changed in [tuned, other, cpu, preload] {
			assert_ne!(changed.label(), base);
		}
	}

	#[cfg(target_os = "linux")]
	#[test]
	fn native_fingerprint_includes_loaded_libc() {
		let found = libc().unwrap().expect("expected mapped libc on Linux");
		assert!(found.contains("build-id ") || found == "static", "{found}");
		let mut a = Native::probe(crate::Mode::Bench).unwrap();
		let mut b = Native::probe(crate::Mode::Bench).unwrap();
		a.libc = Some("glibc 2.39 (build-id 000000000001)".to_owned());
		b.libc = Some("glibc 2.40 (build-id 000000000002)".to_owned());
		assert_ne!(a.label(), b.label());
	}

	#[test]
	fn validates_mapped_libc_and_rejects_missing_or_mixed_builds() {
		let a = validate_libc_record("glibc 2.43 (build-id aabbccdd0011)").unwrap();
		let b = validate_libc_record("glibc 2.44 (build-id 112233445566)").unwrap();
		assert_eq!(validate_libc_record("build-id deadbeef").unwrap(), "build-id deadbeef");
		assert_eq!(validate_libc_record("static").unwrap(), "static");
		assert!(validate_libc_record("malformed").is_err());
		assert!(validate_libc_record("glibc 2.44 (build-id nothex!)").is_err());
		assert!(validate_libc_record("build-id ").is_err());

		assert_eq!(
			require_single_libc([("x", Some(a.as_str())), ("y", Some(a.as_str()))], true).unwrap(),
			Some(a.clone())
		);
		assert_eq!(
			require_single_libc(std::iter::empty::<(&str, Option<&str>)>(), true).unwrap(),
			None
		);
		// Missing libc identity is rejected when verification is required (Linux).
		let missing = require_single_libc([("old", None), ("new", Some(a.as_str()))], true).unwrap_err();
		assert!(missing.contains("did not report its mapped libc identity"), "{missing}");
		// Missing libc identity is ignored off Linux.
		assert_eq!(require_single_libc([("mac", None)], false).unwrap(), None);
		// Mixed libc builds are rejected.
		let mixed = require_single_libc([("x", Some(a.as_str())), ("y", Some(b.as_str()))], true).unwrap_err();
		assert!(mixed.contains("different libc builds"), "{mixed}");

		let base = Native::probe(crate::Mode::Bench).unwrap();
		assert_ne!(base.with_libc(Some(a)).label(), base.with_libc(Some(b)).label());
	}
}
