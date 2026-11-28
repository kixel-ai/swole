//! Where a benchmark's native time goes: a pass of its own samples each side's timed calls with
//! precise cycles (`cycles:pp`, which AMD's IBS or Intel's PEBS takes), resolves each sample's
//! instruction pointer against the executable or library mapped there, inlined frames included,
//! and splits the cycles a call took among the functions that ran them.
//!
//! swole opens a sampling event for each CPU the pass runs on in the benchmark's process, just
//! before it `exec`s, as it opens counters, and hands each to itself over a socket, so it reads
//! their ring buffers while the target holds none. The harness starts and stops every event its
//! process owns around exactly what its clock brackets, so setup, steps, and inputs built between
//! calls stay out of a profile as they stay out of a time.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fmt::Write as _;

use crate::attribution::{Row, shifted};
use crate::callgrind::Function;

/// One side's profile of a benchmark.
#[derive(Clone, Debug, PartialEq)]
pub struct Profile {
	/// Each function's sampled self cycles per call, its own and what it inlined from Rust's
	/// standard library.
	pub functions: BTreeMap<Function, f64>,
	/// How many samples it holds.
	pub samples: u64,
	/// The files sampled that hold neither a symbol table nor debug info, whose samples name no function.
	pub bare: BTreeSet<String>,
}

/// What a profile's ring buffers held: each user-space instruction pointer's cycles, the
/// kernel's, and what the kernel says it missed.
#[derive(Debug, Default)]
pub struct Samples {
	user: HashMap<u64, u64>,
	kernel: u64,
	count: u64,
	/// Samples the kernel dropped for want of room in a ring buffer.
	pub lost: u64,
	/// How often the kernel stopped sampling for a while, since interrupts came too fast.
	pub throttled: u64,
	/// How often a drain found a ring buffer full, which may have dropped samples unsaid.
	pub filled: u64,
}

impl Samples {
	/// Adds `other`'s.
	pub fn add(&mut self, other: Self) {
		for (ip, cycles) in other.user {
			*self.user.entry(ip).or_default() += cycles;
		}
		self.kernel += other.kernel;
		self.count += other.count;
		self.lost += other.lost;
		self.throttled += other.throttled;
		self.filled += other.filled;
	}

	/// How many samples it holds.
	pub fn count(&self) -> u64 {
		self.count
	}

	/// The functions behind its samples, over `calls` calls, given the process's `/proc/<pid>/maps`.
	pub fn resolve(&self, maps: &str, calls: u64) -> Result<Profile, String> {
		let mappings = mappings(maps)?;
		let mut objects: HashMap<&str, Option<Object>> = HashMap::new();
		let mut bare = BTreeSet::new();
		let mut cycles: BTreeMap<Function, u64> = BTreeMap::new();
		if self.kernel > 0 {
			*cycles.entry(function("[kernel]", "kernel")).or_default() += self.kernel;
		}
		for (&ip, &weight) in &self.user {
			let found = match mapped(&mappings, ip) {
				Some(mapping) => {
					let shown = shown(&mapping.path);
					let object = objects.entry(&mapping.path).or_insert_with(|| {
						mapping
							.path
							.starts_with('/')
							.then(|| Object::load(&mapping.path))
							.flatten()
					});
					// A mapping of no file, such as `[vdso]`, names its samples itself.
					if object.as_ref().is_some_and(|object| object.bare) {
						bare.insert(mapping.path.clone());
					}
					let name = match object {
						Some(object) => object
							.function(ip - mapping.start + mapping.offset)
							.unwrap_or_else(|| "[unknown]".to_owned()),
						None if !mapping.path.starts_with('/') => mapping.path.clone(),
						None => "[unknown]".to_owned(),
					};
					Function { name, object: shown }
				}
				None => function("[unmapped]", ""),
			};
			*cycles.entry(found).or_default() += weight;
		}
		Ok(Profile {
			functions: cycles
				.into_iter()
				.map(|(function, cycles)| (function, cycles as f64 / calls as f64))
				.collect(),
			samples: self.count,
			bare,
		})
	}
}

fn function(name: &str, object: &str) -> Function {
	Function {
		name: name.to_owned(),
		object: object.to_owned(),
	}
}

/// An executable mapping of a process, as `/proc/<pid>/maps` lists it.
#[derive(Debug, PartialEq)]
pub(crate) struct Mapping {
	pub start: u64,
	pub end: u64,
	/// Where in its file it starts.
	pub offset: u64,
	/// Its file, or what the kernel calls an anonymous one, such as `[vdso]`.
	pub path: String,
}

/// The mapping of `mappings`, sorted by address, that holds `ip`.
pub(crate) fn mapped(mappings: &[Mapping], ip: u64) -> Option<&Mapping> {
	let i = mappings.partition_point(|mapping| mapping.end <= ip);
	mappings.get(i).filter(|mapping| mapping.start <= ip)
}

/// The executable mappings `/proc/<pid>/maps` lists, by address.
pub(crate) fn mappings(maps: &str) -> Result<Vec<Mapping>, String> {
	let mut mappings = Vec::new();
	for line in maps.lines() {
		let malformed = || format!("/proc/<pid>/maps listed {line:?}");
		let mut fields = line.splitn(6, ' ');
		let (Some(range), Some(perms), Some(offset), Some(_dev), Some(_inode)) = (
			fields.next(),
			fields.next(),
			fields.next(),
			fields.next(),
			fields.next(),
		) else {
			return Err(malformed());
		};
		if !perms.contains('x') {
			continue;
		}
		let hex = |text: &str| u64::from_str_radix(text, 16).map_err(|_| malformed());
		let (start, end) = range.split_once('-').ok_or_else(malformed)?;
		let path = fields.next().unwrap_or_default().trim_start();
		let path = if path.is_empty() { "[anonymous]" } else { path };
		mappings.push(Mapping {
			start: hex(start)?,
			end: hex(end)?,
			offset: hex(offset)?,
			path: path.to_owned(),
		});
	}
	mappings.sort_by_key(|mapping| mapping.start);
	Ok(mappings)
}

/// How a table shows the object at `path`: its file's name, less the hash Cargo gives each build
/// of a target, so a function of the base's build and of the head's are one.
fn shown(path: &str) -> String {
	let name = path.rsplit('/').next().unwrap_or(path);
	match name.rsplit_once('-') {
		Some((stem, hash)) if hash.len() == 16 && hash.bytes().all(|b| b.is_ascii_hexdigit()) => stem.to_owned(),
		_ => name.to_owned(),
	}
}

/// An ELF file, its symbols and debug info, and where each of its loaded segments lies in it.
pub(crate) struct Object {
	loader: addr2line::Loader,
	/// Each loaded segment's offset and size in the file, and its address.
	segments: Vec<(u64, u64, u64)>,
	/// Each function symbol's address, size, and name, by address: the symbol table's, or without
	/// one, the dynamic symbols'.
	symbols: Vec<(u64, u64, String)>,
	/// Whether it holds neither a symbol table nor debug info.
	bare: bool,
}

impl Object {
	/// The ELF file at `path`, or none when it cannot be read as one, whose samples then name no function.
	pub(crate) fn load(path: &str) -> Option<Self> {
		use object::{Object as _, ObjectSegment as _, ObjectSymbol as _};
		let data = std::fs::read(path).ok()?;
		let file = object::File::parse(&*data).ok()?;
		let segments = file
			.segments()
			.map(|segment| {
				let (offset, size) = segment.file_range();
				(offset, size, segment.address())
			})
			.collect();
		let bare = file.symbol_table().is_none() && !file.has_debug_symbols();
		let table = if file.symbol_table().is_some() {
			file.symbols()
		} else {
			file.dynamic_symbols()
		};
		let mut symbols: Vec<(u64, u64, String)> = table
			.filter(|symbol| symbol.kind() == object::SymbolKind::Text && symbol.size() > 0)
			.filter_map(|symbol| Some((symbol.address(), symbol.size(), symbol.name().ok()?.to_owned())))
			.collect();
		symbols.sort();
		Some(Self {
			loader: addr2line::Loader::new(path).ok()?,
			segments,
			symbols,
			bare,
		})
	}

	/// The address `offset` in the file loads at.
	fn address(&self, offset: u64) -> Option<u64> {
		let &(start, _, address) = self
			.segments
			.iter()
			.find(|&&(start, size, _)| (start..start + size).contains(&offset))?;
		Some(offset - start + address)
	}

	/// Every function at `offset` in the file, demangled: each frame inlined there, innermost
	/// first, then the symbol whose bytes hold it, which a function with neither debug info nor a
	/// symbol of its own, such as one of a stripped library's internal ones, lacks.
	pub(crate) fn names(&self, offset: u64) -> Vec<String> {
		let Some(probe) = self.address(offset) else {
			return Vec::new();
		};
		let mut names = Vec::new();
		if let Ok(mut frames) = self.loader.find_frames(probe) {
			while let Ok(Some(frame)) = frames.next() {
				if let Some(name) = frame.function.as_ref().and_then(|name| name.raw_name().ok()) {
					names.push(demangle(&name));
				}
			}
		}
		let i = self.symbols.partition_point(|&(address, _, _)| address <= probe);
		if let Some((address, size, name)) = i.checked_sub(1).map(|i| &self.symbols[i])
			&& probe < address + size
		{
			names.push(demangle(name));
		}
		names
	}

	/// The function at `offset` in the file: the innermost frame inlined there that is not Rust's
	/// standard library's, whose generic helpers belong to the code that called them, or the
	/// outermost when every frame is; else the symbol there.
	fn function(&self, offset: u64) -> Option<String> {
		let probe = self.address(offset)?;
		if let Ok(mut frames) = self.loader.find_frames(probe) {
			let mut outermost = None;
			while let Ok(Some(frame)) = frames.next() {
				let Some(name) = frame.function.as_ref().and_then(|name| name.raw_name().ok()) else {
					continue;
				};
				let standard = frame
					.location
					.as_ref()
					.and_then(|location| location.file)
					.is_some_and(standard);
				if !standard {
					return Some(demangle(&name));
				}
				outermost = Some(name.into_owned());
			}
			if let Some(name) = outermost {
				return Some(demangle(&name));
			}
		}
		self.loader.find_symbol(probe).map(demangle)
	}
}

/// Whether the source file `path` is Rust's standard library's, or a crate it vendors: as
/// rustc remaps the sources of a standard library it ships, or as a toolchain built from source,
/// such as Nix's, finds them under its own `rustlib`.
fn standard(path: &str) -> bool {
	path.starts_with("/rustc/") || path.starts_with("/rust/deps/") || path.contains("/lib/rustlib/src/rust/")
}

/// A Rust symbol's path without its hash, or any other name without the version a shared
/// library's dynamic symbol carries, such as `pthread_create@@GLIBC_2.34`.
pub(crate) fn demangle(name: &str) -> String {
	let name = name.split('@').next().unwrap_or(name);
	rustc_demangle::try_demangle(name).map_or_else(|_| name.to_owned(), |name| format!("{name:#}"))
}

/// For each of `names`, in their order, whose time changed, the functions whose sampled self
/// cycles moved its sampled cycles most, a Markdown table as sim's attribution prints one; empty
/// when none has any.
pub fn table(profiles: &BTreeMap<String, [Profile; 2]>, names: &[String], sampling: &crate::perf::Sampling) -> String {
	let mut out = String::new();
	for (name, [base, head]) in names.iter().filter_map(|name| Some((name, profiles.get(name)?))) {
		let was: f64 = base.functions.values().sum();
		let shown = name.replace('|', "\\|");
		for Row {
			function,
			before,
			after,
		} in shifted(&base.functions, &head.functions)
		{
			if out.is_empty() {
				write!(
					out,
					"\nFunctions whose CPU cycles moved with each time change, each one's own cycles a call on every \
					 thread, sampled with {} ({}), up to five for each benchmark:\n\n\
					 | benchmark | measure | function | base | head | change (of benchmark's cycles) |\n\
					 |---|---|---|---:|---:|---:|\n",
					crate::perf::Sampling::NAME,
					sampling.by
				)
				.expect("a String takes every write");
			}
			let change = after - before;
			let sampled = crate::metrics::SAMPLED;
			writeln!(
				out,
				"| {shown} | cycles | {function} | {sampled}{before:.0} | {sampled}{after:.0} | \
				 {sampled}{change:+.0} ({:+.2}%) |",
				change / was * 100.0
			)
			.expect("a String takes every write");
		}
	}
	out
}

/// Each side's profile as `--report` writes it: every function's cycles a call, most first.
pub fn report(profile: &Profile) -> serde_json::Value {
	let mut functions: Vec<(&Function, f64)> = profile.functions.iter().map(|(f, &c)| (f, c)).collect();
	functions.sort_by(|a, b| b.1.total_cmp(&a.1).then(a.0.cmp(b.0)));
	serde_json::json!({
		"event": crate::perf::Sampling::NAME,
		"samples": profile.samples,
		"unit": "cycles",
		"functions": functions
			.into_iter()
			.map(|(function, cycles)| serde_json::json!({
				"function": function.name,
				"object": function.object,
				"value": crate::metrics::number(cycles),
			}))
			.collect::<Vec<_>>(),
	})
}

#[cfg(target_os = "linux")]
pub use linux::{Ring, arm, hand};

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn reads_the_executable_mappings_by_address() {
		let maps = "7f00-7f80 r-xp 00001000 fd:01 42 /usr/lib/lib c.so\n\
		            5000-6000 r--p 00000000 fd:01 7 /bin/x\n\
		            6000-7000 r-xp 00001000 fd:01 7 /bin/x\n\
		            ff00-ff10 r-xp 00000000 00:00 0                          [vdso]\n\
		            a000-b000 rwxp 00000000 00:00 0\n";
		assert_eq!(
			mappings(maps).unwrap(),
			[
				Mapping {
					start: 0x6000,
					end: 0x7000,
					offset: 0x1000,
					path: "/bin/x".to_owned()
				},
				Mapping {
					start: 0x7f00,
					end: 0x7f80,
					offset: 0x1000,
					path: "/usr/lib/lib c.so".to_owned()
				},
				Mapping {
					start: 0xa000,
					end: 0xb000,
					offset: 0,
					path: "[anonymous]".to_owned()
				},
				Mapping {
					start: 0xff00,
					end: 0xff10,
					offset: 0,
					path: "[vdso]".to_owned()
				},
			]
		);
		assert!(mappings("6000-x r-xp 0 fd:01 7 /bin/x\n").is_err());
		assert!(mappings("6000-7000 r-xp\n").is_err());
	}

	#[test]
	fn names_an_object_by_its_file_less_cargos_hash() {
		assert_eq!(shown("/t/release/deps/spin-0123456789abcdef"), "spin");
		assert_eq!(shown("/usr/lib/libc.so.6"), "libc.so.6");
		assert_eq!(shown("/t/my-tool-0123"), "my-tool-0123");
		assert_eq!(shown("[vdso]"), "[vdso]");
		assert!(standard(
			"/rustc/90b35a6239c3d8bdabc530a6a0816f7ff89a0aaf/library/core/src/hint.rs"
		));
		assert!(standard(
			"/nix/store/x-rust-1.98.1/lib/rustlib/src/rust/library/core/src/hint.rs"
		));
		assert!(!standard("/home/me/repo/benches/spin.rs"));
	}

	#[inline(never)]
	fn marker(n: u64) -> u64 {
		std::hint::black_box(n).wrapping_mul(3)
	}

	#[test]
	fn resolves_its_own_functions_and_the_kernel() {
		assert_eq!(marker(1), 3);
		let ip = marker as *const () as u64;
		let maps = std::fs::read_to_string("/proc/self/maps").unwrap();
		let mut samples = Samples::default();
		samples.user.insert(ip + 1, 300);
		samples.user.insert(16, 20);
		samples.kernel = 100;
		samples.count = 3;
		let profile = samples.resolve(&maps, 10).unwrap();
		let exe = std::env::current_exe().unwrap();
		let object = shown(exe.to_str().unwrap());
		assert_eq!(
			profile,
			Profile {
				functions: BTreeMap::from([
					(function("cargo_swole::profile::tests::marker", &object), 30.0),
					(function("[kernel]", "kernel"), 10.0),
					(function("[unmapped]", ""), 2.0),
				]),
				samples: 3,
				bare: BTreeSet::new(),
			}
		);
	}

	fn profile(functions: &[(&str, f64)]) -> Profile {
		Profile {
			functions: functions
				.iter()
				.map(|(name, cycles)| {
					let (name, object) = name.split_once('@').unwrap_or((name, "bin"));
					(function(name, object), *cycles)
				})
				.collect(),
			samples: 1000,
			bare: BTreeSet::new(),
		}
	}

	fn sampling() -> crate::perf::Sampling {
		crate::perf::Sampling {
			by: "AMD IBS op",
			attr: Default::default(),
		}
	}

	fn table(base: &[(&str, f64)], head: &[(&str, f64)]) -> String {
		let profiles = BTreeMap::from([("bench".to_owned(), [profile(base), profile(head)])]);
		super::table(&profiles, &["bench".to_owned()], &sampling())
	}

	#[test]
	fn prints_sampled_cycles_in_sims_table_shape() {
		let table = table(
			&[("decode", 1000.0), ("alloc", 500.0), ("main", 100.0), ("gone", 40.0)],
			&[("decode", 1400.0), ("alloc", 560.0), ("main", 100.0), ("new", 30.0)],
		);
		assert!(table.starts_with(
			"\nFunctions whose CPU cycles moved with each time change, each one's own cycles a call on every thread, \
			 sampled with cycles:pp (AMD IBS op)"
		));
		let rows: Vec<&str> = table.lines().filter(|row| row.starts_with("| bench |")).collect();
		// +400 and +60 and +30 and -40 make +450: decode alone is the majority, past the least three.
		assert_eq!(rows.len(), 3, "{table}");
		assert_eq!(
			rows[0],
			"| bench | cycles | `decode` | ~1000 | ~1400 | ~+400 (+24.39%) |"
		);
		assert!(rows[1].contains("`alloc` | ~500 | ~560 | ~+60"), "{table}");
		assert!(rows[2].contains("`new` (not in base) | ~0 | ~30 | ~+30"), "{table}");
		assert!(!table.contains("gone"), "{table}");
		assert_eq!(super::table(&BTreeMap::new(), &[], &sampling()), "");
	}

	#[test]
	fn reports_every_function_most_first() {
		let report = report(&profile(&[("a", 1.5), ("b", 30.0)]));
		assert_eq!(
			report,
			serde_json::json!({
				"event": "cycles:pp",
				"samples": 1000,
				"unit": "cycles",
				"functions": [
					{ "function": "b", "object": "bin", "value": 30 },
					{ "function": "a", "object": "bin", "value": 1.5 },
				],
			})
		);
	}
}

#[cfg(target_os = "linux")]
mod linux {
	use std::ffi::c_void;
	use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
	use std::sync::atomic::{AtomicU64, Ordering};

	use super::Samples;
	use crate::perf::Sampling;

	/// Pages of each CPU's ring buffer past its header page: 256 KiB, some 8,000 samples, a third of a
	/// second of one CPU's at 6 GHz, within the half MiB a CPU the kernel lets any user lock.
	const PAGES: usize = 64;

	/// The most descriptors one message hands over, well under the kernel's 253.
	const CHUNK: usize = 64;

	/// Has `command` open a profile's sampling event on each of `cpus` just before it `exec`s, where
	/// the target holds none of them, and hand each to swole: what receives them once it has run.
	pub fn arm(command: &mut std::process::Command, sampling: &Sampling, cpus: &[usize]) -> Result<Armed, String> {
		let sampling = sampling.clone();
		hand(command, cpus, 1, move |cpu, fds| {
			fds[0] = sampling.open(cpu)?;
			Ok(())
		})
	}

	/// Has `command` open `per` events on each of `cpus` with `open`, which fills one CPU's
	/// descriptors, just before it `exec`s, where the target holds none of them, and hand each to
	/// swole, a CPU's in order: what receives them once it has run. `open` runs between fork and
	/// exec, so it makes system calls alone, and opens each event closed on exec.
	pub fn hand(
		command: &mut std::process::Command,
		cpus: &[usize],
		per: usize,
		open: impl Fn(i32, &mut [i32]) -> std::io::Result<()> + Send + Sync + 'static,
	) -> Result<Armed, String> {
		use std::os::unix::process::CommandExt;
		assert!((1..=CHUNK).contains(&per), "a message hands over a CPU's events at once");
		let mut pair = [0i32; 2];
		// SAFETY: `pair` holds the two descriptors the call writes.
		if unsafe { socketpair(AF_UNIX, SOCK_SEQPACKET | SOCK_CLOEXEC, 0, pair.as_mut_ptr()) } != 0 {
			return Err(format!(
				"cannot open a socket to hand perf events over: {}",
				std::io::Error::last_os_error()
			));
		}
		// SAFETY: the kernel returned two new descriptors, which nothing else owns.
		let (ours, theirs) = unsafe { (OwnedFd::from_raw_fd(pair[0]), OwnedFd::from_raw_fd(pair[1])) };
		let sender = theirs.as_raw_fd();
		let cpus: Vec<i32> = cpus.iter().map(|&cpu| cpu as i32).collect();
		let count = cpus.len() * per;
		// Built before the fork, which the child only writes into.
		let mut control = vec![0u64; space(CHUNK) / 8];
		let mut fds = vec![0i32; CHUNK];
		// SAFETY: between fork and exec it makes only system calls, into buffers built before.
		unsafe {
			command.pre_exec(move || {
				for chunk in cpus.chunks(CHUNK / per) {
					for (fds, &cpu) in fds.chunks_mut(per).zip(chunk) {
						open(cpu, fds)?;
					}
					send(sender, &fds[..chunk.len() * per], &mut control)?;
				}
				Ok(())
			});
		}
		Ok(Armed {
			socket: ours,
			sender: theirs,
			count,
		})
	}

	/// Sends `fds` over `socket` with a byte of data, as a message with none carries no descriptors.
	fn send(socket: i32, fds: &[i32], control: &mut [u64]) -> std::io::Result<()> {
		let mut byte = 0u8;
		let mut iov = IoVec {
			base: (&raw mut byte).cast(),
			len: 1,
		};
		let len = size_of_val(fds);
		let buffer = control.as_mut_ptr().cast::<u8>();
		// SAFETY: the buffer holds a header and `CHUNK` descriptors, at least `fds`.
		unsafe {
			buffer.cast::<CmsgHdr>().write(CmsgHdr {
				len: HEADER + len,
				level: SOL_SOCKET,
				kind: SCM_RIGHTS,
			});
			std::ptr::copy_nonoverlapping(fds.as_ptr().cast::<u8>(), buffer.add(HEADER), len);
		}
		let message = MsgHdr {
			name: std::ptr::null_mut(),
			namelen: 0,
			iov: &raw mut iov,
			iovlen: 1,
			control: buffer.cast(),
			controllen: space(fds.len()),
			flags: 0,
		};
		// SAFETY: the message points at buffers that outlive the call.
		if unsafe { sendmsg(socket, &message, 0) } < 0 {
			return Err(std::io::Error::last_os_error());
		}
		Ok(())
	}

	/// The sampling events of a process yet to hand them over.
	pub struct Armed {
		socket: OwnedFd,
		/// The child's end, closed once it has run, so a short handover ends rather than waits.
		sender: OwnedFd,
		count: usize,
	}

	impl Armed {
		/// The ring buffer of each sampling event the process `pid`, now started, handed over,
		/// whose samples alone each keeps.
		pub fn rings(self, pid: u32) -> Result<Rings, String> {
			let rings = self.receive()?.into_iter().map(Ring::map).collect::<Result<_, _>>()?;
			Ok(Rings { rings, pid })
		}

		/// Every event the process, now started, handed over, in the order it opened them.
		pub fn receive(self) -> Result<Vec<OwnedFd>, String> {
			let Self { socket, sender, count } = self;
			drop(sender);
			let mut fds = Vec::with_capacity(count);
			let mut control = vec![0u64; space(CHUNK) / 8];
			while fds.len() < count {
				let mut byte = 0u8;
				let mut iov = IoVec {
					base: (&raw mut byte).cast(),
					len: 1,
				};
				let mut message = MsgHdr {
					name: std::ptr::null_mut(),
					namelen: 0,
					iov: &raw mut iov,
					iovlen: 1,
					control: control.as_mut_ptr().cast(),
					controllen: control.len() * 8,
					flags: 0,
				};
				// SAFETY: the message points at buffers that outlive the call. Each descriptor
				// received is closed on exec, so no later job inherits it.
				let received = unsafe { recvmsg(socket.as_raw_fd(), &mut message, MSG_CMSG_CLOEXEC) };
				if received < 0 {
					return Err(format!(
						"cannot receive the perf events: {}",
						std::io::Error::last_os_error()
					));
				}
				if received == 0 {
					return Err(format!(
						"the benchmark handed over {} of its {count} perf events",
						fds.len()
					));
				}
				if message.flags & MSG_CTRUNC != 0 {
					return Err("the benchmark handed over more perf events than asked".to_owned());
				}
				let buffer = control.as_ptr().cast::<u8>();
				// SAFETY: the kernel wrote one header of this kind and its descriptors, as sent.
				let header = unsafe { buffer.cast::<CmsgHdr>().read() };
				if message.controllen < HEADER || header.level != SOL_SOCKET || header.kind != SCM_RIGHTS {
					return Err("the benchmark handed over no perf events".to_owned());
				}
				for i in 0..(header.len - HEADER) / 4 {
					// SAFETY: as above; each is a new descriptor, which nothing else owns.
					let fd = unsafe { buffer.add(HEADER + 4 * i).cast::<i32>().read_unaligned() };
					fds.push(unsafe { OwnedFd::from_raw_fd(fd) });
				}
			}
			Ok(fds)
		}
	}

	/// The ring buffers of a process's sampling events, one each.
	pub struct Rings {
		rings: Vec<Ring>,
		/// The process whose samples count, of all that ran on the CPUs.
		pid: u32,
	}

	impl Rings {
		/// Every sample of the process since the last drain.
		pub fn drain(&mut self) -> Result<Samples, String> {
			let mut samples = Samples::default();
			for ring in &mut self.rings {
				ring.drain(self.pid, &mut samples)?;
			}
			Ok(samples)
		}
	}

	/// A sampling event's ring buffer, mapped: a header page, then the data the kernel writes.
	pub struct Ring {
		fd: OwnedFd,
		base: *mut u8,
		len: usize,
		/// Where the data starts past the header, and its size.
		offset: usize,
		size: usize,
	}

	/// Where `perf_event_mmap_page` holds how far the kernel wrote and swole read, and where the
	/// data lies.
	const DATA_HEAD: usize = 1024;
	const DATA_TAIL: usize = 1032;
	const DATA_OFFSET: usize = 1040;
	const DATA_SIZE: usize = 1048;

	/// The largest record a profile's event writes: a sample's header, instruction pointer,
	/// process and thread, and period.
	const MOST_RECORD: usize = 32;

	/// The records a profile reads.
	const PERF_RECORD_LOST: u32 = 2;
	const PERF_RECORD_THROTTLE: u32 = 5;
	const PERF_RECORD_SAMPLE: u32 = 9;
	const PERF_RECORD_LOST_SAMPLES: u32 = 13;
	/// Which ring a sample was taken in, of its header's `misc`.
	const PERF_RECORD_MISC_CPUMODE_MASK: u16 = 7;
	const PERF_RECORD_MISC_USER: u16 = 2;

	impl Ring {
		/// Maps `fd`'s ring buffer, which the kernel counts against what the user may lock.
		pub fn map(fd: OwnedFd) -> Result<Self, String> {
			// SAFETY: asks the page size alone.
			let page = unsafe { sysconf(SC_PAGESIZE) } as usize;
			let len = (1 + PAGES) * page;
			// SAFETY: maps the event's buffer, shared, read and written as the kernel's protocol
			// says; unmapped on drop.
			let base = unsafe {
				mmap(
					std::ptr::null_mut(),
					len,
					PROT_READ | PROT_WRITE,
					MAP_SHARED,
					fd.as_raw_fd(),
					0,
				)
			};
			if base == MAP_FAILED {
				return Err(format!(
					"cannot map a {} KiB ring buffer for samples: {}, as past what the kernel lets a user lock \
					 (kernel.perf_event_mlock_kb)",
					len / 1024,
					std::io::Error::last_os_error()
				));
			}
			let base = base.cast::<u8>();
			// SAFETY: the header page holds these fields at their offsets.
			let (offset, size) = unsafe {
				(
					base.add(DATA_OFFSET).cast::<u64>().read_volatile() as usize,
					base.add(DATA_SIZE).cast::<u64>().read_volatile() as usize,
				)
			};
			// A kernel before 4.1 says neither, and puts the data right past the header page.
			let (offset, size) = if size == 0 {
				(page, PAGES * page)
			} else {
				(offset, size)
			};
			Ok(Self {
				fd,
				base,
				len,
				offset,
				size,
			})
		}

		/// The event whose ring buffer it is.
		pub fn fd(&self) -> &OwnedFd {
			&self.fd
		}

		/// The kernel's `u64` at `at` in the header page.
		fn word(&self, at: usize) -> &AtomicU64 {
			// SAFETY: the header page holds an aligned `u64` there, which the kernel and swole
			// share, for as long as the mapping.
			unsafe { AtomicU64::from_ptr(self.base.add(at).cast()) }
		}

		/// Adds every record of the process `pid` since the last drain to `samples`, and frees its room.
		fn drain(&mut self, pid: u32, samples: &mut Samples) -> Result<(), String> {
			let filled = self.records(MOST_RECORD, |kind, misc, record| {
				let field = |at: usize| {
					record
						.get(at..at + 8)
						.map(|bytes| u64::from_ne_bytes(bytes.try_into().expect("8 bytes")))
						.ok_or(format!(
							"a sampling ring buffer holds a record of kind {kind} too short to read"
						))
				};
				match kind {
					PERF_RECORD_SAMPLE => {
						// The instruction pointer, the process and thread, then the period: the
						// cycles the sample stands for.
						let (ip, period) = (field(8)?, field(24)?);
						let task = u32::from_ne_bytes(record[16..20].try_into().expect("4 bytes"));
						// Another process's, which ran on one of the CPUs meanwhile.
						if task != pid {
							return Ok(());
						}
						if misc & PERF_RECORD_MISC_CPUMODE_MASK == PERF_RECORD_MISC_USER {
							*samples.user.entry(ip).or_default() += period;
						} else {
							samples.kernel += period;
						}
						samples.count += 1;
					}
					PERF_RECORD_LOST => samples.lost += field(16)?,
					PERF_RECORD_LOST_SAMPLES => samples.lost += field(8)?,
					PERF_RECORD_THROTTLE => samples.throttled += 1,
					_ => {}
				}
				Ok(())
			})?;
			if filled {
				samples.filled += 1;
			}
			Ok(())
		}

		/// Hands `each` every record since the last drain, its kind, its header's `misc`, and its
		/// bytes, the header's among them, then frees their room; and says whether the ring had too
		/// little left for a record of `most` bytes, the largest its events write, so the kernel may
		/// have dropped what came next unsaid.
		pub fn records(
			&mut self,
			most: usize,
			mut each: impl FnMut(u32, u16, &[u8]) -> Result<(), String>,
		) -> Result<bool, String> {
			let head = self.word(DATA_HEAD).load(Ordering::Acquire);
			let mut tail = self.word(DATA_TAIL).load(Ordering::Relaxed);
			// Without room for another record, the kernel dropped what came next, and says so only
			// in a record it writes once there is room again, which may never come. It keeps a
			// byte of the ring free, so a record fits only in less than the rest.
			let filled = head - tail >= (self.size - most) as u64;
			while tail < head {
				let header = self.copy(tail, 8);
				let kind = u32::from_ne_bytes(header[0..4].try_into().expect("4 bytes"));
				let misc = u16::from_ne_bytes(header[4..6].try_into().expect("2 bytes"));
				let size = u16::from_ne_bytes(header[6..8].try_into().expect("2 bytes")) as u64;
				if size < 8 || tail + size > head {
					return Err(format!(
						"a perf ring buffer holds a record of {size} bytes, which no kernel writes"
					));
				}
				each(kind, misc, &self.copy(tail, size as usize))?;
				tail += size;
			}
			self.word(DATA_TAIL).store(tail, Ordering::Release);
			Ok(filled)
		}

		/// `len` bytes of data from `at`, which may wrap around the data's end.
		fn copy(&self, at: u64, len: usize) -> Vec<u8> {
			(0..len)
				.map(|i| {
					let index = self.offset + (at as usize + i) % self.size;
					// SAFETY: the index lies within the mapped data.
					unsafe { self.base.add(index).read_volatile() }
				})
				.collect()
		}
	}

	impl Drop for Ring {
		fn drop(&mut self) {
			// SAFETY: the mapping made in `map`, of this length.
			unsafe { munmap(self.base.cast(), self.len) };
		}
	}

	/// The bytes a control message of `n` descriptors takes, its header and padding included.
	const fn space(n: usize) -> usize {
		HEADER + (n * 4).div_ceil(8) * 8
	}

	#[repr(C)]
	struct IoVec {
		base: *mut c_void,
		len: usize,
	}

	#[repr(C)]
	struct MsgHdr {
		name: *mut c_void,
		namelen: u32,
		iov: *mut IoVec,
		iovlen: usize,
		control: *mut c_void,
		controllen: usize,
		flags: i32,
	}

	#[repr(C)]
	struct CmsgHdr {
		len: usize,
		level: i32,
		kind: i32,
	}

	/// A control message's header, aligned.
	const HEADER: usize = size_of::<CmsgHdr>();
	const AF_UNIX: i32 = 1;
	const SOCK_SEQPACKET: i32 = 5;
	const SOCK_CLOEXEC: i32 = 0o2_000_000;
	const SOL_SOCKET: i32 = 1;
	const SCM_RIGHTS: i32 = 1;
	const MSG_CTRUNC: i32 = 8;
	const MSG_CMSG_CLOEXEC: i32 = 0x4000_0000;
	const SC_PAGESIZE: i32 = 30;
	const PROT_READ: i32 = 1;
	const PROT_WRITE: i32 = 2;
	const MAP_SHARED: i32 = 1;
	const MAP_FAILED: *mut c_void = !0 as *mut c_void;

	unsafe extern "C" {
		fn socketpair(domain: i32, kind: i32, protocol: i32, fds: *mut i32) -> i32;
		fn sendmsg(socket: i32, message: *const MsgHdr, flags: i32) -> isize;
		fn recvmsg(socket: i32, message: *mut MsgHdr, flags: i32) -> isize;
		fn sysconf(name: i32) -> i64;
		fn mmap(addr: *mut c_void, len: usize, prot: i32, flags: i32, fd: i32, offset: i64) -> *mut c_void;
		fn munmap(addr: *mut c_void, len: usize) -> i32;
	}
}
