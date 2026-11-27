//! A base commit's counts against the working tree's, on this host, from one directory.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::cargo::Members;
use crate::fingerprint::Fnv;
use crate::{Counter, Counts, Mode, Options, measure, metrics, missing};

/// Counts `base`, then a snapshot of the working tree, both in one worktree of their own, as
/// `mode`, quick or sim, counts, and prints each change. One directory counts both, so a path a
/// build compiles in, such as a panic's location, is the same on both sides and moves neither count.
pub fn compare(base: &str, options: &Options, mode: Mode) -> Result<(), String> {
	// Probed before anything builds, so a host that cannot count fails first.
	let counter = Counter::new(mode)?;
	let top = PathBuf::from(git(Path::new("."), &["rev-parse", "--show-toplevel"])?);
	// Where the run is within the repository, so both sides run from the same place in the worktree.
	let prefix = git(Path::new("."), &["rev-parse", "--show-prefix"])?;
	let sha = git(
		Path::new("."),
		&["rev-parse", "--verify", &format!("{base}^{{commit}}")],
	)?;
	let real = top
		.canonicalize()
		.map_err(|e| format!("cannot resolve {}: {e}", top.display()))?;
	let tree = worktree(&real)?;
	// Taken first, so the head is the working tree as it was when compare started, however
	// long it waits for the lock or the base takes to count.
	let head = snapshot(&top, &tree.with_extension(format!("index-{}", std::process::id())))?;
	// Another compare of this repository shares the worktree, so it waits until this one has
	// counted both sides rather than check out another commit under it.
	let lock = tree.with_extension("lock");
	let lock = std::fs::File::create(&lock).map_err(|e| format!("cannot create {}: {e}", lock.display()))?;
	lock.lock()
		.map_err(|e| format!("cannot lock compare's worktree: {e}"))?;
	let mut both = options.clone();
	both.cargo = rebase(&options.cargo, &real, &tree)?;
	// Parsed before either side counts, so a selector compare cannot resolve fails first.
	let args = parse(&both.cargo)?;
	let dir = tree.join(&prefix);
	checkout(&top, &tree, &sha)?;
	eprintln!(
		"swole: counting {base} ({}) in {}",
		&sha[..12.min(sha.len())],
		tree.display()
	);
	let picked = pick(&dir, &both, &args, base)?;
	// A wanted benchmark may be new at the head or gone from it, but must be on one side.
	let mut before = count(&dir, &both, &counter, &picked, base)?;
	checkout(&top, &tree, &head)?;
	eprintln!(
		"swole: counting the working tree ({}) in {}",
		&head[..12.min(head.len())],
		tree.display()
	);
	let head_picked = pick(&dir, &both, &args, "the working tree")?;
	neither(base, &picked, &head_picked)?;
	let mut after = count(&dir, &both, &counter, &head_picked, "the working tree")?;
	drop(lock);
	if let Some(wanted) = &options.benchmarks {
		missing(wanted, |name| {
			before.counts.contains_key(name) || after.counts.contains_key(name)
		})?;
	}
	if before.counts.is_empty() && after.counts.is_empty() {
		return Err("no benchmark ran".to_owned());
	}
	// An empty side ran no benchmark process and therefore observed no benchmark libc.
	if before.counts.is_empty() {
		before.fingerprint = after.fingerprint.clone();
		before.host = after.host.clone();
	} else if after.counts.is_empty() {
		after.fingerprint = before.fingerprint.clone();
		after.host = before.host.clone();
	}
	let (rendered, order) = table(&before, &after)?;
	if mode == Mode::Quick {
		eprintln!("swole: fingerprint {}", after.fingerprint);
	}
	print!("{rendered}");
	let mut threaded = Counts::none(&counter);
	threaded.threaded = before.threaded.union(&after.threaded).cloned().collect();
	threaded.spread = before.spread.clone().into_iter().chain(after.spread.clone()).collect();
	print!("{}", crate::threaded(&threaded));
	print!(
		"{}",
		metrics::details(
			Some(&before.shown()),
			&after.shown(),
			&std::collections::BTreeMap::new(),
			&metrics::Pick::new(order, options.all_details, true),
		)
	);
	if let Some(report) = &options.report {
		metrics::report(&after, Some(&before), &counter, Some(&sha)).write(report)?;
		eprintln!("swole: wrote {}", report.display());
	}
	Ok(())
}

/// A base commit checked out alone in compare's worktree for a time, which builds the working
/// tree where it is beside it, held from any other compare or time until dropped.
pub struct Checkout {
	/// The commit as the run names it.
	pub base: String,
	/// The commit's full ID.
	pub sha: String,
	/// Where to build it: the same place within the worktree as the run is within the repository.
	pub dir: PathBuf,
	/// The options to build it with, its Cargo args moved into the worktree and its target
	/// directory there.
	pub options: Options,
	/// Held until dropped, so no other compare or time checks out another commit under this one.
	_lock: std::fs::File,
}

impl Checkout {
	/// Checks `base` out, waiting for any other compare or time of this repository to finish.
	pub fn new(base: &str, options: &Options) -> Result<Self, String> {
		let top = PathBuf::from(git(Path::new("."), &["rev-parse", "--show-toplevel"])?);
		let prefix = git(Path::new("."), &["rev-parse", "--show-prefix"])?;
		let sha = git(
			Path::new("."),
			&["rev-parse", "--verify", &format!("{base}^{{commit}}")],
		)?;
		let real = top
			.canonicalize()
			.map_err(|e| format!("cannot resolve {}: {e}", top.display()))?;
		let tree = worktree(&real)?;
		let lock = tree.with_extension("lock");
		let lock = std::fs::File::create(&lock).map_err(|e| format!("cannot create {}: {e}", lock.display()))?;
		lock.lock()
			.map_err(|e| format!("cannot lock compare's worktree: {e}"))?;
		checkout(&top, &tree, &sha)?;
		let mut moved = options.clone();
		// The base builds into the worktree's own target directory, kept warm with it: one the
		// environment, a config, or the run's own `--target-dir` shares with the working tree
		// would get both sides' benchmark executables at one path, since Cargo names them alike
		// whatever the workspace's path.
		let target = tree.join("target");
		let target = target.to_str().ok_or("compare's worktree path is not UTF-8")?;
		moved.cargo = retarget(&rebase(&options.cargo, &real, &tree)?, target);
		Ok(Self {
			base: base.to_owned(),
			sha,
			dir: tree.join(prefix),
			options: moved,
			_lock: lock,
		})
	}

	/// The commit's ID, abbreviated.
	pub fn short(&self) -> &str {
		&self.sha[..12.min(self.sha.len())]
	}

	/// What the base and the working tree where the run is each build of their Cargo args, the
	/// working tree's those of `options`, as compare selects each side.
	pub fn select(&self, options: &Options) -> Result<Sides, String> {
		let (base, head) = (parse(&self.options.cargo)?, parse(&options.cargo)?);
		let before = pick(&self.dir, &self.options, &base, &self.base)?;
		// The run's own directory is the working tree's whether or not Git tracks it yet.
		let after = pick_in(Path::new("."), options, &head, "the working tree")?;
		neither(&self.base, &before, &after)?;
		Ok(Sides {
			base: before.args,
			head: after.args,
		})
	}
}

/// The Cargo args each side builds, `None` for a side with nothing they select.
pub struct Sides {
	/// The base's, in its worktree.
	pub base: Option<Vec<String>>,
	/// The working tree's.
	pub head: Option<Vec<String>>,
}

/// Fails on a selector neither `before`, the side `base`, nor `after`, the working tree, has.
fn neither(base: &str, before: &Selection, after: &Selection) -> Result<(), String> {
	let neither: Vec<&str> = before.lacks.intersection(&after.lacks).map(String::as_str).collect();
	if neither.is_empty() {
		return Ok(());
	}
	Err(format!(
		"neither {base} nor the working tree has {}",
		neither.join(", ")
	))
}

/// Cargo `args` building into `target`, in place of any `--target-dir` they name.
fn retarget(args: &[String], target: &str) -> Vec<String> {
	let mut out = Vec::with_capacity(args.len() + 1);
	let mut args = args.iter();
	while let Some(arg) = args.next() {
		if arg == "--" {
			out.push(format!("--target-dir={target}"));
			out.push(arg.clone());
			out.extend(args.by_ref().cloned());
			return out;
		}
		if arg == "--target-dir" {
			args.next();
		} else if !arg.starts_with("--target-dir=") {
			out.push(arg.clone());
		}
	}
	out.push(format!("--target-dir={target}"));
	out
}

/// A Cargo argument as compare's selection reads it.
#[derive(Debug, PartialEq)]
enum Arg {
	/// `-p`'s or `--package`'s package name.
	Package(String),
	/// `--bench`'s target name.
	Bench(String),
	/// `--exclude`'s package name.
	Exclude(String),
	/// The features of one `-F` or `--features`, each on its own.
	Features(Vec<String>),
	/// Any other argument, passed through as it is.
	Other(String),
}

/// Reads the Cargo `args` before their `--`, refusing a package or benchmark target the selection
/// cannot resolve by its name alone, such as `name@1.0` or a glob, rather than passing it to a side
/// that may lack it.
fn parse(args: &[String]) -> Result<Vec<Arg>, String> {
	let mut out = Vec::new();
	let mut args = args.iter();
	while let Some(arg) = args.next() {
		if arg == "--" {
			out.push(Arg::Other(arg.clone()));
			out.extend(args.by_ref().cloned().map(Arg::Other));
			break;
		}
		// `-p name`, `-pname`, `-p=name`, `--package name`, or `--package=name`, and the same
		// forms of `-F` and `--bench`.
		let value = |short: &str, long: &str, args: &mut std::slice::Iter<String>| -> Result<Option<String>, String> {
			if arg == long || (!short.is_empty() && arg == short) {
				return args
					.next()
					.filter(|value| !value.starts_with('-'))
					.cloned()
					.map(Some)
					.ok_or(format!("compare needs {arg} to name its value"));
			}
			if let Some(value) = arg.strip_prefix(long).and_then(|rest| rest.strip_prefix('=')) {
				return Ok(Some(value.to_owned()));
			}
			if !short.is_empty()
				&& let Some(value) = arg.strip_prefix(short)
			{
				return Ok(Some(value.strip_prefix('=').unwrap_or(value).to_owned()));
			}
			Ok(None)
		};
		let named = |what: &str, name: String| {
			if !name.is_empty() && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_') {
				Ok(name)
			} else {
				Err(format!(
					"compare selects each side's {what} by name, and cannot resolve {name:?}: name each one"
				))
			}
		};
		out.push(if let Some(name) = value("-p", "--package", &mut args)? {
			Arg::Package(named("packages", name)?)
		} else if let Some(name) = value("", "--bench", &mut args)? {
			Arg::Bench(named("benchmark targets", name)?)
		} else if let Some(name) = value("", "--exclude", &mut args)? {
			Arg::Exclude(named("excluded packages", name)?)
		} else if let Some(list) = value("-F", "--features", &mut args)? {
			Arg::Features(
				list.split(|c: char| c == ',' || c.is_whitespace())
					.filter(|feature| !feature.is_empty())
					.map(str::to_owned)
					.collect(),
			)
		} else {
			Arg::Other(arg.clone())
		});
	}
	Ok(out)
}

/// What one side builds: the Cargo args, or none when it has nothing they select, and what of
/// the selection it lacks.
struct Selection {
	args: Option<Vec<String>>,
	/// Each `-p name`, `--bench name`, and `feature package/feature` the side lacks.
	lacks: BTreeSet<String>,
}

/// The Cargo `args` for a side whose workspace has `members`: each package, benchmark target,
/// and package's feature it lacks is left out, and a selection left with no package or no
/// target selects nothing. It refuses a `package/feature` under resolver 1, which applies it to
/// the current package too, so the side could count it where the other does not.
fn select(args: &[Arg], members: &Members) -> Result<Selection, String> {
	if members.legacy
		&& let Some(feature) = args.iter().find_map(|arg| match arg {
			Arg::Features(list) => list.iter().find(|feature| feature.contains('/')),
			_ => None,
		}) {
		return Err(format!(
			"compare cannot select --features {feature} per side under Cargo's resolver 1: set resolver = \"2\" or later"
		));
	}
	let packages: Vec<&String> = args
		.iter()
		.filter_map(|arg| match arg {
			Arg::Package(name) => Some(name),
			_ => None,
		})
		.collect();
	let kept: BTreeSet<&str> = packages
		.iter()
		.map(|name| name.as_str())
		.filter(|name| members.benches.contains_key(*name))
		.collect();
	let flag = |names: &[&str]| {
		args.iter()
			.take_while(|arg| **arg != Arg::Other("--".to_owned()))
			.any(|arg| matches!(arg, Arg::Other(arg) if names.contains(&arg.as_str())))
	};
	let whole = flag(&["--workspace", "--all"]);
	// Each builds every benchmark target of the packages, so one named target a side lacks
	// still leaves it a selection.
	let every_bench = flag(&["--benches", "--all-targets"]);
	let excluded: BTreeSet<&str> = args
		.iter()
		.filter_map(|arg| match arg {
			Arg::Exclude(name) => Some(name.as_str()),
			_ => None,
		})
		.collect();
	// Cargo looks for `--bench` among the packages it builds: every member but those excluded
	// under `--workspace`, which overrides `-p`, else those `-p` selects, else the default ones.
	let builds = |name: &&String| {
		if whole {
			!excluded.contains(name.as_str())
		} else if !packages.is_empty() {
			kept.contains(name.as_str())
		} else {
			members.default.contains(*name)
		}
	};
	let benches: BTreeSet<&String> = members
		.benches
		.iter()
		.filter(|(name, _)| builds(name))
		.flat_map(|(_, benches)| benches)
		.collect();
	// A `package/feature` names a package Cargo builds, or one of their dependencies.
	let featured: BTreeSet<&str> = members
		.dependencies
		.iter()
		.filter(|(name, _)| builds(name))
		.flat_map(|(name, dependencies)| [name].into_iter().chain(dependencies))
		.map(String::as_str)
		.collect();
	let mut selection = Selection {
		args: None,
		lacks: BTreeSet::new(),
	};
	let mut out = Vec::new();
	let (mut wants_bench, mut has_bench) = (false, false);
	for arg in args {
		match arg {
			Arg::Package(name) if kept.contains(name.as_str()) => out.extend(["-p".to_owned(), name.clone()]),
			Arg::Package(name) => {
				selection.lacks.insert(format!("-p {name}"));
			}
			Arg::Bench(name) => {
				wants_bench = true;
				if benches.contains(name) {
					has_bench = true;
					out.extend(["--bench".to_owned(), name.clone()]);
				} else {
					selection.lacks.insert(format!("--bench {name}"));
				}
			}
			Arg::Features(list) => {
				// `package/feature` or `package?/feature`.
				let (features, gone): (Vec<&str>, Vec<&str>) = list.iter().map(String::as_str).partition(|feature| {
					feature
						.split_once('/')
						.is_none_or(|(package, _)| featured.contains(package.strip_suffix('?').unwrap_or(package)))
				});
				selection
					.lacks
					.extend(gone.into_iter().map(|feature| format!("feature {feature}")));
				if !features.is_empty() {
					out.extend(["--features".to_owned(), features.join(",")]);
				}
			}
			Arg::Exclude(name) => out.extend(["--exclude".to_owned(), name.clone()]),
			Arg::Other(arg) => out.push(arg.clone()),
		}
	}
	// Under `--workspace`, Cargo builds the members `--exclude` leaves, and fails with none left.
	let built = if whole {
		members.benches.keys().any(|name| !excluded.contains(name.as_str()))
	} else {
		packages.is_empty() || !kept.is_empty()
	};
	if built && (!wants_bench || has_bench || every_bench) {
		selection.args = Some(out);
	}
	Ok(selection)
}

/// What the side checked out under `dir`, which the log calls `side`, builds of `args`, the Cargo
/// args of `options`: what its workspace has of them, from its metadata, which Cargo reads in
/// milliseconds.
fn pick(dir: &Path, options: &Options, args: &[Arg], side: &str) -> Result<Selection, String> {
	if !options.programs.is_empty() {
		return Ok(Selection {
			args: Some(options.cargo.clone()),
			lacks: BTreeSet::new(),
		});
	}
	// A directory only one side has, such as a new package's, selects nothing on the other. The
	// directory counts only when the side's revision tracks it, since an earlier side's build may
	// have left its output there.
	if !dir.exists() || git(dir, &["ls-tree", "HEAD", "--", "."])?.is_empty() {
		return none(args, side, dir.display().to_string());
	}
	pick_in(dir, options, args, side)
}

/// What the side in `dir`, which has that directory, builds of `args`, as `pick` says.
fn pick_in(dir: &Path, options: &Options, args: &[Arg], side: &str) -> Result<Selection, String> {
	// A manifest only one side has selects nothing on the other.
	if let Some(manifest) = crate::cargo::manifest_path(&options.cargo).filter(|manifest| !dir.join(manifest).exists())
	{
		return none(args, side, format!("--manifest-path {manifest}"));
	}
	let selection = select(args, &crate::cargo::members(dir, &options.cargo)?)?;
	for lacked in &selection.lacks {
		eprintln!("swole: {side} has no {lacked}, so builds without it");
	}
	Ok(selection)
}

/// The selection of a side, which the log calls `side`, that has no `absent` and so builds nothing.
fn none(args: &[Arg], side: &str, absent: String) -> Result<Selection, String> {
	eprintln!("swole: {side} has no {absent}, so builds nothing");
	// It lacks every selector too, so one the other side also lacks still fails.
	let mut lacks = select(args, &Members::default())?.lacks;
	lacks.insert(absent);
	Ok(Selection { args: None, lacks })
}

/// Counts the side checked out under `dir`, which the log calls `side`, with `counter`, building
/// what `selection` picked.
fn count(
	dir: &Path,
	options: &Options,
	counter: &Counter,
	selection: &Selection,
	side: &str,
) -> Result<Counts, String> {
	let Some(args) = &selection.args else {
		eprintln!("swole: {side} has nothing the cargo args select, so counts none");
		return Ok(Counts::none(counter));
	};
	let mut options = options.clone();
	options.cargo = args.clone();
	measure(dir, &options, counter, false)
}

/// The working tree of the repository at `top` as a commit: `HEAD` when nothing differs from
/// it, else a commit on `HEAD` of every file Git tracks or would add, staged or not. It is built
/// in the scratch index at `index`, so the caller's index and stash stay as they were.
fn snapshot(top: &Path, index: &Path) -> Result<String, String> {
	if top.join(".gitmodules").exists() {
		dirty_submodules(&git(
			top,
			// Without the optional lock, status never refreshes the caller's index.
			&[
				"--no-optional-locks",
				"status",
				"--porcelain=v2",
				"--ignore-submodules=none",
				"-z",
			],
		)?)?;
	}
	// A copy of the caller's index, whose cached file stats spare `add` hashing every file again.
	let own = PathBuf::from(git(
		top,
		&["rev-parse", "--path-format=absolute", "--git-path", "index"],
	)?);
	match std::fs::copy(&own, index) {
		Ok(_) => {}
		Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
			let _ = std::fs::remove_file(index);
		}
		Err(e) => return Err(format!("cannot copy {} to {}: {e}", own.display(), index.display())),
	}
	let staged = |args: &[&str]| -> Result<String, String> {
		let out = Command::new("git")
			.args(args)
			.current_dir(top)
			.env("GIT_INDEX_FILE", index)
			.output()
			.map_err(|e| format!("cannot run git: {e}"))?;
		output(args, out)
	};
	staged(&["add", "--all", "--", ":/"])?;
	let tree = staged(&["write-tree"])?;
	std::fs::remove_file(index).map_err(|e| format!("cannot remove {}: {e}", index.display()))?;
	let head = git(top, &["rev-parse", "--verify", "HEAD^{commit}"])?;
	if tree == git(top, &["rev-parse", "--verify", "HEAD^{tree}"])? {
		return Ok(head);
	}
	// A fixed identity, since the host's Git may have none.
	let out = Command::new("git")
		.args(["commit-tree", &tree, "-p", &head, "-m", "swole: the working tree"])
		.current_dir(top)
		.env("GIT_AUTHOR_NAME", "swole")
		.env("GIT_AUTHOR_EMAIL", "swole@localhost")
		.env("GIT_COMMITTER_NAME", "swole")
		.env("GIT_COMMITTER_EMAIL", "swole@localhost")
		.output()
		.map_err(|e| format!("cannot run git: {e}"))?;
	output(&["commit-tree"], out)
}

/// Refuses a submodule with uncommitted or untracked changes, from `git status
/// --porcelain=v2 -z`: the snapshot holds only the commit each submodule has checked out.
fn dirty_submodules(status: &str) -> Result<(), String> {
	let mut entries = status.split('\0');
	while let Some(entry) = entries.next() {
		// A changed entry is `1 XY sub ... path`, or `2 XY sub ... path` followed by its
		// original path as an entry of its own.
		let count = match entry.as_bytes().first() {
			Some(b'1') => 9,
			Some(b'2') => {
				entries.next();
				10
			}
			_ => continue,
		};
		let fields: Vec<&str> = entry.splitn(count, ' ').collect();
		let (Some(sub), Some(path)) = (fields.get(2), fields.get(count - 1)) else {
			return Err(format!("git status printed an entry it does not document: {entry}"));
		};
		// `S<c><m><u>`: a submodule, then whether its commit, tracked files, or untracked files changed.
		if let [b'S', _, m, u] = sub.as_bytes()
			&& (*m == b'M' || *u == b'U')
		{
			return Err(format!(
				"submodule {path} has uncommitted changes, which compare cannot count: commit them in it"
			));
		}
	}
	Ok(())
}

/// Where the worktree both sides of a compare of the repository at `top` count in lives: in the
/// user's cache, outside the repository, since Cargo reads every `.cargo/config.toml` above
/// where it builds, and only each side's own may configure its build. `top` is canonical.
fn worktree(top: &Path) -> Result<PathBuf, String> {
	let cache = crate::cache()?;
	if cache.starts_with(top) {
		return Err(format!(
			"the cache {} is inside the repository, where its .cargo/config.toml would configure the base: point XDG_CACHE_HOME outside it",
			cache.display()
		));
	}
	let mut hash = Fnv::default();
	hash.write(&top.to_string_lossy());
	let name = top.file_name().map_or("repo".into(), |name| name.to_string_lossy());
	Ok(cache.join(format!("{name}-{:012x}", hash.0 >> 16)))
}

/// `args` for compare's worktree: an absolute `--manifest-path` in the repository at `top`
/// moves to the same file under `tree`, and one outside it, which would count the same
/// workspace twice, is refused.
fn rebase(args: &[String], top: &Path, tree: &Path) -> Result<Vec<String>, String> {
	let mut out = Vec::with_capacity(args.len());
	let mut args = args.iter();
	while let Some(arg) = args.next() {
		if arg == "--" {
			out.push(arg.clone());
			out.extend(args.by_ref().cloned());
			break;
		}
		let (flag, path) = if arg == "--manifest-path" {
			out.push(arg.clone());
			match args.next() {
				Some(path) => ("", path.as_str()),
				None => break,
			}
		} else if let Some(path) = arg.strip_prefix("--manifest-path=") {
			("--manifest-path=", path)
		} else {
			out.push(arg.clone());
			continue;
		};
		let rebased = if Path::new(path).is_absolute() {
			// The nearest directory up that exists, since the manifest may be only the base's.
			let path = Path::new(path);
			let existing = path.ancestors().find(|dir| dir.exists()).expect("the root exists");
			let canonical = existing
				.canonicalize()
				.map_err(|e| format!("cannot resolve {}: {e}", existing.display()))?
				.join(
					path.strip_prefix(existing)
						.expect("an ancestor prefixes its descendant"),
				);
			let path = path.display();
			let relative = canonical.strip_prefix(top).map_err(|_| {
				format!("--manifest-path {path} is outside the repository, so compare would count it twice")
			})?;
			tree.join(relative)
				.to_str()
				.ok_or("the base's worktree path is not UTF-8")?
				.to_owned()
		} else {
			path.to_owned()
		};
		out.push(format!("{flag}{rebased}"));
	}
	Ok(out)
}

/// Checks `sha` out at `tree`, a detached worktree of the repository at `top` kept between
/// compares, so its build products stay warm for the next side and the next compare.
fn checkout(top: &Path, tree: &Path, sha: &str) -> Result<(), String> {
	let path = tree.to_str().ok_or("compare's worktree path is not UTF-8")?;
	if tree.join(".git").exists() {
		git(tree, &["checkout", "--quiet", "--detach", "--force", sha])?;
		// A lockfile the commit does not track is one an earlier side's build resolved, which
		// would pin this side's dependencies to that side's.
		git(
			tree,
			&["clean", "--quiet", "--force", "-x", "--", ":(glob)**/Cargo.lock"],
		)?;
	} else {
		// Forget a worktree whose directory a cache cleanup removed, so it can be added again.
		git(top, &["worktree", "prune"])?;
		git(top, &["worktree", "add", "--quiet", "--detach", path, sha])?;
	}
	if tree.join(".gitmodules").exists() {
		git(tree, &["submodule", "update", "--quiet", "--init", "--recursive"])?;
	}
	Ok(())
}

/// Runs git in `dir`, returning its trimmed stdout.
fn git(dir: &Path, args: &[&str]) -> Result<String, String> {
	let out = Command::new("git")
		.args(args)
		.current_dir(dir)
		.output()
		.map_err(|e| format!("cannot run git: {e}"))?;
	output(args, out)
}

/// The trimmed stdout of `git args`, or its failure.
fn output(args: &[&str], out: std::process::Output) -> Result<String, String> {
	if !out.status.success() {
		return Err(format!(
			"git {} failed: {}",
			args.join(" "),
			String::from_utf8_lossy(&out.stderr).trim()
		));
	}
	String::from_utf8(out.stdout)
		.map(|s| s.trim().to_owned())
		.map_err(|_| "git printed something not UTF-8".to_owned())
}

/// The change of each benchmark from `before` to `after`, a Markdown table, refusing counts from
/// two environments, which do not compare: the largest changes first, sim's estimated cycles
/// among them, then each benchmark one side has, then those that held, with the likely cause
/// beside each regression its measures explain, and a count of several samples their least and
/// most beside their median. And each benchmark in the table's order, with whether it moved.
fn table(before: &Counts, after: &Counts) -> Result<(String, Vec<(String, bool)>), String> {
	if before.fingerprint != after.fingerprint {
		return Err(format!(
			"the base counted under fingerprint {} and the working tree under {}: their counts do not compare",
			before.fingerprint, after.fingerprint
		));
	}
	let names: BTreeSet<&String> = before.counts.keys().chain(after.counts.keys()).collect();
	let cell = |counts: &Counts, name: &str| match (counts.counts.get(name), counts.spread.get(name)) {
		(Some(count), Some((least, most))) => format!("{}{count} ({least} to {most})", metrics::SAMPLED),
		(Some(count), None) => count.to_string(),
		(None, _) => "-".to_owned(),
	};
	// A sampled count compares against its spread, never as one exact count.
	let side = |counts: &Counts, count: u64, name: &str| {
		let spread = counts
			.spread
			.get(name)
			.map(|&(least, most)| (least as f64, most as f64));
		(count as f64, spread)
	};
	let (base, head) = (before.shown(), after.shown());
	let mut rows = Vec::new();
	for name in names {
		let (rank, impact, change, cause) = match (before.counts.get(name), after.counts.get(name)) {
			(Some(&b), Some(&a)) => {
				let (b_side, a_side) = (side(before, b, name), side(after, a, name));
				// Sim's estimated cycles are a headline too, which a change of misses alone moves.
				let cycles = |side: &BTreeMap<String, Vec<crate::Measure>>| {
					side.get(name)?
						.iter()
						.find(|m| m.name == crate::callgrind::ESTIMATED)
						.map(|m| (m.value, m.spread))
				};
				let headlines = std::iter::once((b_side, a_side)).chain(cycles(&base).zip(cycles(&head)));
				// Two sampled counts move only past their spreads; one sampled beside an exact one
				// has no verdict, so it moved without regressing.
				let (mut moved, mut regressed, mut impact) = (false, false, 0.0f64);
				for ((b, b_spread), (a, a_spread)) in headlines {
					let shifted = a != b && a_spread.zip(b_spread).is_none_or(|(a, b)| !metrics::overlap(b, a));
					moved |= shifted;
					regressed |= shifted && a_spread.is_some() == b_spread.is_some() && a > b;
					impact = impact.max(if b == 0.0 { f64::INFINITY } else { (a - b).abs() / b });
				}
				let cause = regressed
					.then(|| crate::diagnose::cause(base.get(name)?, head.get(name)?, None))
					.flatten();
				(
					if moved {
						metrics::Rank::Moved
					} else {
						metrics::Rank::Held
					},
					impact,
					metrics::change(b_side, a_side),
					cause,
				)
			}
			(None, Some(_)) => (metrics::Rank::OneSide, 0.0, "added".to_owned(), None),
			(Some(_), None) => (metrics::Rank::OneSide, 0.0, "removed".to_owned(), None),
			(None, None) => unreachable!("every name comes from one side"),
		};
		rows.push(metrics::Row {
			name,
			cells: format!("{} | {} | {change}", cell(before, name), cell(after, name)),
			rank,
			impact,
			cause,
		});
	}
	Ok(metrics::headline("base | head | change", "---:|---:|---:", rows))
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn builds_the_base_into_its_own_target_directory_alone() {
		let args = |list: &[&str]| list.iter().map(|s| (*s).to_owned()).collect::<Vec<_>>();
		assert_eq!(
			retarget(
				&args(&[
					"--target-dir",
					"a",
					"--locked",
					"--target-dir=b",
					"--",
					"--target-dir",
					"c"
				]),
				"/t"
			),
			args(&["--locked", "--target-dir=/t", "--", "--target-dir", "c"])
		);
		assert_eq!(retarget(&[], "/t"), args(&["--target-dir=/t"]));
	}

	#[test]
	fn rebases_an_absolute_manifest_path_into_the_base() {
		let top = Path::new(env!("CARGO_MANIFEST_DIR")).canonicalize().unwrap();
		let manifest = top.join("Cargo.toml");
		let manifest = manifest.to_str().unwrap();
		let tree = Path::new("/base");
		let args = |list: &[&str]| list.iter().map(|s| (*s).to_owned()).collect::<Vec<_>>();
		let rebased = rebase(
			&args(&[
				"--manifest-path",
				manifest,
				"--locked",
				"--",
				"--manifest-path",
				manifest,
			]),
			&top,
			tree,
		)
		.unwrap();
		assert_eq!(
			rebased,
			args(&[
				"--manifest-path",
				"/base/Cargo.toml",
				"--locked",
				"--",
				"--manifest-path",
				manifest
			])
		);
		let equals = format!("--manifest-path={manifest}");
		assert_eq!(
			rebase(&args(&[&equals]), &top, tree).unwrap(),
			args(&["--manifest-path=/base/Cargo.toml"])
		);
		assert_eq!(
			rebase(&args(&["--manifest-path", "Cargo.toml"]), &top, tree).unwrap(),
			args(&["--manifest-path", "Cargo.toml"])
		);
		// A manifest only the base has, gone from the working tree.
		let gone = top.join("gone/Cargo.toml");
		assert_eq!(
			rebase(&args(&["--manifest-path", gone.to_str().unwrap()]), &top, tree).unwrap(),
			args(&["--manifest-path", "/base/gone/Cargo.toml"])
		);
		let err = rebase(&args(&["--manifest-path", manifest]), &top.join("src"), tree).unwrap_err();
		assert!(err.contains("outside the repository"), "{err}");
	}

	fn counts(fingerprint: &str, names: &[(&str, u64)]) -> Counts {
		Counts {
			fingerprint: fingerprint.to_owned(),
			host: String::new(),
			counts: names.iter().map(|(name, ir)| ((*name).to_owned(), *ir)).collect(),
			spread: BTreeMap::new(),
			threaded: BTreeSet::new(),
			details: BTreeMap::new(),
			throughput: BTreeMap::new(),
		}
	}

	#[test]
	fn tables_each_change_and_each_benchmark_one_side_has() {
		let before = counts("f", &[("a", 200), ("gone", 5), ("held", 9), ("small", 100)]);
		let mut after = counts("f", &[("a", 150), ("new", 7), ("held", 9), ("small", 101)]);
		after.spread.insert("a".to_owned(), (140, 160));
		// The largest change first, then each benchmark one side has, then those that held.
		let (table, order) = table(&before, &after).unwrap();
		assert_eq!(
			table,
			"| benchmark | base | head | change |\n|---|---:|---:|---:|\n\
			 | a | 200 | ~150 (140 to 160) | ~-25.00% |\n| small | 100 | 101 | +1.00% |\n\
			 | gone | 5 | - | removed |\n| new | - | 7 | added |\n| held | 9 | 9 | +0.00% |\n"
		);
		assert_eq!(
			order,
			[
				("a", true),
				("small", true),
				("gone", true),
				("new", true),
				("held", false)
			]
			.map(|(name, moved)| (name.to_owned(), moved))
		);
	}

	#[test]
	fn names_the_likely_cause_beside_a_regression() {
		let measures = |misses: f64| {
			vec![
				crate::Measure::new("instructions", 10_000.0, "instructions"),
				crate::Measure::new("LL instruction misses", 0.0, "events"),
				crate::Measure::new("LL read misses", misses, "events"),
				crate::Measure::new("LL write misses", 0.0, "events"),
			]
		};
		let mut before = counts("f", &[("a", 10_000), ("b", 10_000)]);
		let mut after = counts("f", &[("a", 10_100), ("b", 9_000)]);
		before.details.insert("a".to_owned(), measures(1.0));
		after.details.insert("a".to_owned(), measures(20.0));
		before.details.insert("b".to_owned(), measures(1.0));
		after.details.insert("b".to_owned(), measures(20.0));
		let (rendered, _) = table(&before, &after).unwrap();
		// An improvement names none, even where its misses grew.
		assert_eq!(
			rendered,
			"| benchmark | base | head | change | likely cause |\n|---|---:|---:|---:|---|\n\
			 | b | 10000 | 9000 | -10.00% |  |\n\
			 | a | 10000 | 10100 | +1.00% | memory-bound (LL MPKI 0.100 → 2.000) |\n"
		);
		// Sim's estimated cycles are a headline too: misses alone move them, and the benchmark.
		let cycles = |misses: f64| {
			let mut measures = measures(misses);
			measures.push(crate::Measure::new(
				crate::callgrind::ESTIMATED,
				10_000.0 + 100.0 * misses,
				"cycles",
			));
			measures
		};
		let mut before = counts("f", &[("c", 10_000)]);
		let mut after = counts("f", &[("c", 10_000)]);
		before.details.insert("c".to_owned(), cycles(1.0));
		after.details.insert("c".to_owned(), cycles(20.0));
		let (rendered, order) = table(&before, &after).unwrap();
		assert!(
			rendered.ends_with("| c | 10000 | 10000 | +0.00% | memory-bound (LL MPKI 0.100 → 2.000) |\n"),
			"{rendered}"
		);
		assert_eq!(order, [("c".to_owned(), true)]);
	}

	#[test]
	fn refuses_only_a_submodule_with_uncommitted_changes() {
		let hash = "0".repeat(40);
		let entry = |sub: &str, path: &str| format!("1 .M {sub} 160000 160000 160000 {hash} {hash} {path}\0");
		// A submodule at another commit, which the snapshot records, and a file.
		let fine = entry("SC..", "moved") + &entry("N...", "a file") + "? untracked\0";
		dirty_submodules(&fine).unwrap();
		let err = dirty_submodules(&entry("S.M.", "edited")).unwrap_err();
		assert!(err.contains("submodule edited"), "{err}");
		dirty_submodules(&entry("S..U", "new")).unwrap_err();
		let renamed = format!("2 R. S.M. 160000 160000 160000 {hash} {hash} R100 to\0from\0");
		let err = dirty_submodules(&renamed).unwrap_err();
		assert!(err.contains("submodule to"), "{err}");
	}

	#[test]
	fn refuses_two_fingerprints() {
		let err = table(&counts("f", &[("a", 1)]), &counts("g", &[("a", 1)])).unwrap_err();
		assert!(err.contains("do not compare"), "{err}");
	}

	fn strings(list: &[&str]) -> Vec<String> {
		list.iter().map(|s| (*s).to_owned()).collect()
	}

	#[test]
	fn parses_each_form_of_a_selector() {
		let args = parse(&strings(&[
			"-p",
			"a",
			"-pb",
			"-p=c",
			"--package",
			"d",
			"--package=e",
			"--bench",
			"x",
			"--bench=y",
			"--benches",
			"-F",
			"a/f, g",
			"--features=b/h",
			"--",
			"-p",
			"z",
		]))
		.unwrap();
		let package = |name: &str| Arg::Package(name.to_owned());
		let other = |arg: &str| Arg::Other(arg.to_owned());
		assert_eq!(
			args,
			[
				package("a"),
				package("b"),
				package("c"),
				package("d"),
				package("e"),
				Arg::Bench("x".to_owned()),
				Arg::Bench("y".to_owned()),
				other("--benches"),
				Arg::Features(strings(&["a/f", "g"])),
				Arg::Features(strings(&["b/h"])),
				other("--"),
				other("-p"),
				other("z"),
			]
		);
		for refused in [
			&["-p", "a@1.0"][..],
			&["-p", "moq-*"],
			&["--bench", "x*"],
			&["-p"],
			&["--bench", "--locked"],
			&["--workspace", "--exclude", "legacy-*"],
		] {
			assert!(parse(&strings(refused)).is_err(), "{refused:?}");
		}
	}

	#[test]
	fn selects_only_what_a_side_has() {
		// Run from `a`'s directory, so `a` alone is the default.
		let members = Members {
			benches: [("a", &["x"][..]), ("c", &["z"])]
				.into_iter()
				.map(|(name, benches)| (name.to_owned(), benches.iter().map(|b| (*b).to_owned()).collect()))
				.collect(),
			default: BTreeSet::from(["a".to_owned()]),
			legacy: false,
			// `c` calls a dependency `b`, as the other side calls a member.
			dependencies: [("a", "dep"), ("c", "b")]
				.into_iter()
				.map(|(name, dependency)| (name.to_owned(), BTreeSet::from([dependency.to_owned()])))
				.collect(),
		};
		let select = |args: &[&str]| select(&parse(&strings(args)).unwrap(), &members).unwrap();

		// A package and its feature, and a target, only the other side has.
		let picked = select(&[
			"-p",
			"a",
			"-p",
			"b",
			"--bench",
			"x",
			"--bench",
			"y",
			"--features",
			"b/f,a/g,h",
			"--locked",
		]);
		assert_eq!(
			picked.args.unwrap(),
			strings(&["-p", "a", "--bench", "x", "--features", "a/g,h", "--locked"])
		);
		assert_eq!(
			picked.lacks,
			strings(&["-p b", "--bench y", "feature b/f"]).into_iter().collect()
		);

		// A target is looked for among the packages Cargo builds: those `-p` selects, else every
		// member under `--workspace` but those excluded, else the default ones.
		let picked = select(&["-p", "a", "--bench", "z"]);
		assert!(picked.args.is_none());
		assert_eq!(picked.lacks, BTreeSet::from(["--bench z".to_owned()]));
		assert!(select(&["--bench", "z"]).args.is_none());
		assert_eq!(
			select(&["--workspace", "--bench", "z"]).args.unwrap(),
			["--workspace", "--bench", "z"]
		);
		assert!(
			select(&["--workspace", "--exclude", "c", "--bench", "z"])
				.args
				.is_none()
		);
		assert!(select(&["--all", "--exclude=c", "--bench", "z"]).args.is_none());
		// `--workspace` overrides `-p`, which Cargo still requires to name a member.
		assert_eq!(
			select(&["--workspace", "-p", "a", "--bench", "z"]).args.unwrap(),
			["--workspace", "-p", "a", "--bench", "z"]
		);
		// With every member excluded, the side builds nothing, whatever `-p` names.
		assert!(
			select(&["--workspace", "--exclude", "a", "--exclude", "c", "-p", "b"])
				.args
				.is_none()
		);
		assert!(select(&["--all", "--exclude=a", "--exclude=c"]).args.is_none());
		let picked = select(&["--workspace", "-p", "b"]);
		assert_eq!(picked.args.unwrap(), ["--workspace"]);
		assert_eq!(picked.lacks, BTreeSet::from(["-p b".to_owned()]));
		// `--benches` and `--all-targets` still select every target beside one the side lacks.
		assert_eq!(select(&["--bench", "y", "--benches"]).args.unwrap(), ["--benches"]);
		assert_eq!(
			select(&["--all-targets", "--bench", "y"]).args.unwrap(),
			["--all-targets"]
		);
		assert!(select(&["--bench", "y", "--", "--benches"]).args.is_none());
		// Features split on whitespace as on commas.
		assert_eq!(
			select(&["-F", "a/g\nb/f", "-p", "a", "-p", "b"]).args.unwrap(),
			["--features", "a/g", "-p", "a"]
		);

		// With every package or every target gone, the side selects nothing rather than everything.
		assert!(select(&["-p", "b", "--features", "b/f"]).args.is_none());
		assert!(select(&["--bench", "y"]).args.is_none());
		// A feature stays only of a package Cargo builds or one of their dependencies, so not of
		// `c`, or of its dependency `b`, when only `a` builds.
		let picked = select(&["-p", "a", "-F", "dep/f,dep?/g,c/g,b/f"]);
		assert_eq!(picked.args.unwrap(), ["-p", "a", "--features", "dep/f,dep?/g"]);
		assert_eq!(
			picked.lacks,
			strings(&["feature b/f", "feature c/g"]).into_iter().collect()
		);
		// Without `-p` too.
		let picked = select(&["--workspace", "--features", "e/f,c/g,b/f"]);
		assert_eq!(picked.args.unwrap(), ["--workspace", "--features", "c/g,b/f"]);
		assert_eq!(picked.lacks, BTreeSet::from(["feature e/f".to_owned()]));
		// A side without the directory or manifest lacks every selector.
		let picked = super::select(
			&parse(&strings(&["-p", "a", "--bench", "x", "-F", "a/g,h"])).unwrap(),
			&Members::default(),
		)
		.unwrap();
		assert_eq!(
			picked.lacks,
			strings(&["-p a", "--bench x", "feature a/g"]).into_iter().collect()
		);
		// Under resolver 1, only a `package/feature` is refused.
		let legacy = Members {
			legacy: true,
			..members
		};
		let select = |args: &[&str]| super::select(&parse(&strings(args)).unwrap(), &legacy);
		let err = select(&["-p", "a", "-F", "h,a/g"]).err().unwrap();
		assert!(err.contains("--features a/g"), "{err}");
		assert_eq!(
			select(&["-p", "a", "--bench", "x", "-F", "h"]).unwrap().args.unwrap(),
			["-p", "a", "--bench", "x", "--features", "h"]
		);
	}
}
