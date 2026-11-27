//! Builds preload.rs, the library swole preloads into every counted process, and on macOS
//! interpose.rs, the one it inserts into an allocation pass, which swole embeds: each is a cdylib,
//! which Cargo cannot build as a dependency of a binary.

use std::path::PathBuf;
use std::process::Command;

/// Each library swole embeds, and the cfgs it is built with: the whole library; the one that only
/// counts threads; and each of them also counting heap allocations, for the allocation pass beside
/// a count (`quick`'s and `sim`'s) and beside a time (`bench`'s), the last of which macOS builds
/// from interpose.rs instead.
const LIBRARIES: [(&str, &[&str]); 4] = [
	("preload.so", &[]),
	("threads.so", &["--cfg=threads"]),
	("heap.so", &["--cfg=heap"]),
	("heap-threads.so", &["--cfg=heap", "--cfg=threads"]),
];

fn main() {
	for source in ["preload.rs", "allocs.rs", "interpose.rs"] {
		println!("cargo::rerun-if-changed={source}");
	}
	let out = PathBuf::from(std::env::var_os("OUT_DIR").expect("Cargo sets OUT_DIR"));
	let target = std::env::var("TARGET").expect("Cargo sets TARGET");
	let rustc = std::env::var_os("RUSTC").unwrap_or_else(|| "rustc".into());
	let build = |name: &str, source: &str, args: &[&str]| {
		let status = Command::new(&rustc)
			.args([
				"--crate-type=cdylib",
				"--crate-name=swole_preload",
				"--edition=2024",
				"-Cpanic=abort",
				"-Copt-level=3",
				"-Cstrip=debuginfo",
				"--target",
				&target,
			])
			.args(args)
			.arg("-o")
			.arg(out.join(name))
			.arg(source)
			.status()
			.expect("rustc runs");
		assert!(status.success(), "building {source} as {name} failed ({status})");
	};
	let empty = |name: &str| std::fs::write(out.join(name), []).expect("OUT_DIR is writable");
	match target.as_str() {
		"x86_64-unknown-linux-gnu" => {
			for (name, cfgs) in LIBRARIES {
				// Its bytes enter the fingerprint, so nothing of where it was built may: no debug
				// info, and the toolchain's own lld rather than the host's C compiler, which may add
				// a search path of the build's, as Nix's does.
				let args = [&["-Clinker=rust-lld", "-Clinker-flavor=ld.lld"], cfgs].concat();
				build(name, "preload.rs", &args);
			}
		}
		// macOS has `bench` alone, whose allocation pass inserts the interposer, which counts
		// threads too.
		target if target.ends_with("-apple-darwin") => {
			for (name, _) in &LIBRARIES[..3] {
				empty(name);
			}
			build(LIBRARIES[3].0, "interpose.rs", &[]);
		}
		// Only x86_64 Linux is fingerprinted, which fails the run first; an empty library keeps
		// the runner building elsewhere, as its unit tests do.
		_ => {
			for (name, _) in LIBRARIES {
				empty(name);
			}
		}
	}
}
