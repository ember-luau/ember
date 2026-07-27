use crate::error::Error;
use crate::net::github::GithubAPI;
use crate::project::lockfile::{LockedPackage, Lockfile};
use crate::project::manifest::{Environment, Manifest, Tool};
use crate::project::package;
use crate::project::requires;
use crate::project::rojo;
use crate::project::workspace::{self, Workspace};
use crate::registry::index;
use crate::registry::resolver;
use crate::sys::hash::fnv1a_parts;
use crate::sys::paths;
use crate::tools;
use crate::ui;
use clap::Args;
use indicatif::ProgressBar;
use std::collections::{BTreeMap, BTreeSet, HashMap, VecDeque};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Instant;

#[derive(Args, Debug)]
#[command(after_long_help = "\
When lpm.toml, lpm.lock, and the global tools file are unchanged since the \
last successful install, `install` trusts the lockfile: within the index \
TTL (5 minutes; LPM_INDEX_TTL_SECS overrides) it skips everything, and \
past it, it re-checks the indices so `^` requirements still pick up new \
releases — rebuilding only when something actually resolved differently. \
Two things this check cannot see: hand-edits inside an installed packages \
folder, and a repository that commits its packages folders (those packages \
really are present, and tools still verify). --refresh forces the full \
pipeline.")]
pub struct InstallArgs {
    /// Install exactly what lpm.lock records, without re-resolving
    #[arg(long)]
    pub locked: bool,

    /// Skip nothing: re-resolve, re-fetch indices, and re-download archives even when caches are fresh
    #[arg(long)]
    pub refresh: bool,
}

struct Job {
    name: String,
    version: String,
    environment: Option<Environment>,
    source: index::DownloadSource,
    index_url: String,
    link: String,
    /// [overrides]-rewritten edges of this package: declared alias -> replacement name.
    redirects: BTreeMap<String, String>,
}

/// how many packages download and unpack at once.
const WORKERS: usize = 8;

/// stamp file the fast path reads, under each `<out>/.lpm/`.
const STATE_FILE: &str = ".state";

/// alias, pinned tool, and whether the pin came from ~/.lpm/tools.toml.
type ToolJob = (String, Tool, bool);

pub fn run(args: InstallArgs) -> Result<(), Error> {
    let manifest = Manifest::load()?;
    install_project(&args, &manifest, true)?;

    /* a workspace root installs every member too, pesde's order: root first,
    then each member. member installs never recurse further (nested
    workspaces aren't a thing) */
    if !manifest.workspace_members().is_empty() {
        let workspace = Workspace::open(Path::new("."))?;
        for member in &workspace.members {
            if member.dir == workspace.root {
                continue;
            }
            println!("Installing {}", member.manifest.package.name);
            workspace::in_dir(&member.dir, || {
                let manifest = Manifest::load()?;
                install_project(&args, &manifest, false)
            })?;
        }
    }
    Ok(())
}

/** installs one project from the current directory. global tools only
install on the primary run: workspace members share them, and repeating
the merge per member would just re-print every pin. */
fn install_project(
    args: &InstallArgs,
    manifest: &Manifest,
    include_global_tools: bool,
) -> Result<(), Error> {
    let started = Instant::now();

    /* the fast path: nothing local changed since the last install, so the
    lockfile is what would resolve anyway. within the index TTL that is a
    certainty and everything is skipped; past it, `^` requirements have
    earned a real look — resolution runs below, and only a resolution that
    actually lands somewhere new triggers a rebuild. tools are verified
    either way (cheap stats), because install doubles as their repair path. */
    let fast = fast_path(args, manifest, include_global_tools)?;
    if fast == FastPath::Skip {
        finish_up_to_date(manifest, include_global_tools)?;
        ui::timing("fast-path total", started);
        return Ok(());
    }

    let cache = if args.refresh {
        index::CachePolicy::Bypass
    } else {
        index::CachePolicy::Use
    };

    /* captured before resolution: the stamp must record the inputs this
    install *consumed*, not whatever is on disk once it finishes — an edit
    landing mid-install has to bust the next fast path */
    let state_inputs = state_inputs(include_global_tools);

    let jobs: Vec<Job> = if args.locked {
        Lockfile::load()?
            .packages
            .into_iter()
            .map(|package| Job {
                name: package.name,
                version: package.version,
                environment: Some(package.environment),
                source: package.source,
                index_url: package.index,
                link: package.link,
                redirects: package.redirects,
            })
            .collect()
    } else {
        let resolve_started = Instant::now();
        let mode = if args.refresh {
            index::Refresh::Force
        } else {
            index::Refresh::Ttl
        };
        /* collected, not printed, inside the spinner: a bare eprintln
        would land mid-frame and get redrawn over */
        let mut warnings = Vec::new();
        let resolved = ui::with_spinner("Resolving dependencies", || {
            resolver::resolve(manifest, Path::new("."), mode, &mut warnings)
        })?;
        for warning in &warnings {
            eprintln!("{warning}");
        }
        ui::timing("resolve", resolve_started);
        resolved
            .into_iter()
            .map(|package| Job {
                name: package.name,
                version: package.version.to_string(),
                environment: package.environment,
                source: package.source,
                index_url: package.index_url,
                link: package.link,
                redirects: package.redirects,
            })
            .collect()
    };

    /* the recheck half of the fast path: local inputs were unchanged but
    the indices were stale, so resolution ran fresh (pulling them). when it
    lands exactly on the lockfile — the overwhelmingly common outcome —
    the rebuild is skipped; the refreshed TTL stamps make the next few
    installs full skips. anything new resolves fall through to the rebuild,
    which is `^` doing its job */
    if fast == FastPath::Recheck
        && let Ok(lock) = Lockfile::load()
        && jobs_match_lock(&jobs, &lock)
    {
        finish_up_to_date(manifest, include_global_tools)?;
        ui::timing("recheck total", started);
        return Ok(());
    }

    /* installs rebuild from scratch each run: every env's output folder is
    wiped even with nothing to install, so removing the last dependency
    leaves no stale packages */
    for environment in Environment::ALL {
        let out = manifest.packages_out(environment);
        if out.exists() {
            fs::remove_dir_all(&out)?;
        }
    }

    /* extraction can happen before we know the environment (and so the
    output folder), so stage in a project-local temp dir; a rename then
    moves it into place (same filesystem as the outputs) */
    let staging = Path::new(".lpm-staging").to_path_buf();
    let packages_started = Instant::now();
    let locked = ui::with_progress(jobs.len() as u64, |bar| {
        install_packages(manifest, jobs, &staging, bar, cache)
    });
    // staging cleanup used to be skipped on error, leaking the directory
    if staging.exists() {
        fs::remove_dir_all(&staging)?;
    }
    let locked = locked?;
    ui::timing("packages", packages_started);

    /* second pass, now that everything is extracted: link files *inside*
    each stored package for the dependencies it declares */
    let stored: Vec<StoredPackage> = locked
        .iter()
        .map(|package| match &package.source {
            /* members link in place, so they're link targets like anything
            else but never get written into: that would dirty a source tree
            the user edits */
            index::DownloadSource::Workspace { path } => StoredPackage {
                name: package.name.to_lowercase(),
                storage: PathBuf::from(path),
                environment: package.environment,
                in_place: true,
                redirects: package.redirects.clone(),
            },
            _ => StoredPackage {
                name: package.name.to_lowercase(),
                storage: manifest
                    .packages_out(package.environment)
                    .join(".lpm")
                    .join(package.name.replace('/', "_")),
                environment: package.environment,
                in_place: false,
                redirects: package.redirects.clone(),
            },
        })
        .collect();
    let nested_started = Instant::now();
    link_nested_dependencies(&stored, &mut |message| eprintln!("{message}"));
    ui::timing("nested-links", nested_started);

    let package_count = locked.len();
    let environments: BTreeSet<Environment> =
        locked.iter().map(|package| package.environment).collect();
    if !args.locked {
        // written even when empty, so lpm.lock always mirrors the manifest
        Lockfile::new(locked).save()?;
    }

    /* tool versions are exact pins, so no lockfile entries; normal and
    --locked runs install them the same way. global tools (~/.lpm/tools.toml)
    install here too: `tool add` never downloads, this is the one place
    every tool gets installed */
    let tool_jobs = tool_jobs(manifest, include_global_tools)?;
    let tool_count = tool_jobs.len();
    if !tool_jobs.is_empty() {
        println!("Installing tools");
        ui::with_progress(tool_count as u64, |bar| install_tools(&tool_jobs, bar))?;
    }

    /* everything succeeded: record what this install saw, so the next run
    can skip itself when nothing has changed. never on --locked — that path
    installs the lockfile without reconciling it against the manifest, so
    stamping there would let a later plain install skip a manifest edit the
    lock has never seen */
    if !args.locked {
        write_state_stamps(manifest, &environments, &state_inputs);
    }

    match (package_count, tool_count) {
        (0, 0) => println!("Nothing to install"),
        (p, 0) => println!("Installed {p} package{}", ui::plural(p)),
        (0, t) => println!("Installed {t} tool{}", ui::plural(t)),
        (p, t) => println!(
            "Installed {p} package{} and {t} tool{}",
            ui::plural(p),
            ui::plural(t)
        ),
    }
    ui::timing("install total", started);
    Ok(())
}

/** the manifest's tools plus (on the primary run) global ones, deduped:
the same pin in both scopes only needs one install. */
fn tool_jobs(
    manifest: &Manifest,
    include_global: bool,
) -> Result<Vec<(String, Tool, bool)>, Error> {
    let mut jobs: Vec<ToolJob> = manifest
        .tools
        .iter()
        .map(|(alias, tool)| (alias.clone(), tool.clone(), false))
        .collect();
    if include_global {
        for (alias, tool) in tools::shim::global_tools()? {
            let duplicate = manifest.tools.get(&alias).is_some_and(|project| {
                project.repository.eq_ignore_ascii_case(&tool.repository)
                    && project.version == tool.version
            });
            if !duplicate {
                jobs.push((alias, tool, true));
            }
        }
    }
    Ok(jobs)
}

/** what the fast path saw when it decided to skip: one hash over every
local input that can change what an install produces. coarse on purpose —
a comment edit in lpm.toml busts the fast path (a wasted handful of
milliseconds), while anything finer risks missing an input (a broken
install). */
fn state_hash(manifest_text: &str, lock_text: &str, tools_text: &str) -> String {
    let hash = fnv1a_parts(&[
        manifest_text.as_bytes(),
        lock_text.as_bytes(),
        tools_text.as_bytes(),
    ]);
    format!("lpm-state-v1:{hash:016x}\n")
}

/** the manifest and global-tools inputs of the state hash, as text. read
once per install, *before* resolution, so an edit landing mid-install can
never be stamped as satisfied (it wasn't). None = an input exists but
can't be read; the fast path then stays off, the safe direction.

member installs don't consume global tools (include_global = false), so
those hash a fixed marker instead of the file — editing ~/.lpm/tools.toml
shouldn't wipe and rebuild every member of a workspace. absence is a
distinct marker too, not an empty string. */
fn state_inputs(include_global: bool) -> Option<(String, String)> {
    let manifest_text = fs::read_to_string(crate::project::manifest::MANIFEST_FILE).ok()?;
    let tools_text = if include_global {
        let path = paths::global_tools_file().ok()?;
        if path.exists() {
            fs::read_to_string(path).ok()?
        } else {
            "<absent>".to_string()
        }
    } else {
        "<not consulted>".to_string()
    };
    Some((manifest_text, tools_text))
}

/// the full state hash as things stand right now; None if any input is unreadable.
fn current_state_hash(include_global: bool) -> Option<String> {
    let (manifest_text, tools_text) = state_inputs(include_global)?;
    let lock_text = fs::read_to_string(crate::project::lockfile::LOCKFILE).ok()?;
    Some(state_hash(&manifest_text, &lock_text, &tools_text))
}

/// how much of an install the unchanged-inputs check lets us skip.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum FastPath {
    /// nothing changed and the indices are TTL-fresh: skip everything
    Skip,
    /** nothing changed locally, but the indices are stale: resolve for
    real (so `^` can pick up new releases), then skip the rebuild if
    resolution lands exactly on the lockfile */
    Recheck,
    /// something changed (or the check doesn't apply): full install
    Full,
}

/** decides how much of this install can be skipped, from the stamps the
last successful install left in each output folder.

never anything but Full for workspaces, in either direction: a root's
install reads member manifests and member source, none of which these
stamps see. two documented blind spots, both shared with pesde:
hand-edits *inside* an output folder, and a repo that commits its
packages folders (the packages really are present, and tools still
verify, so skipping is right). */
fn fast_path(
    args: &InstallArgs,
    manifest: &Manifest,
    include_global_tools: bool,
) -> Result<FastPath, Error> {
    if args.locked || args.refresh || !manifest.workspace_members().is_empty() {
        return Ok(FastPath::Full);
    }
    // same include_global flag the stamp writer used, or hashes never match
    let Some(hash) = current_state_hash(include_global_tools) else {
        return Ok(FastPath::Full); // no lockfile yet (or an unreadable input)
    };
    let Ok(lock) = Lockfile::load() else {
        return Ok(FastPath::Full); // unparseable lockfile: let the full path deal with it
    };

    /* an empty lock matches an empty manifest and nothing else: with no
    output dirs there are no stamps to consult, but resolving zero
    dependencies can only produce zero packages — nothing `^` could ever
    upgrade to, so index freshness doesn't matter either */
    if lock.packages.is_empty() {
        return Ok(if manifest.dependencies.is_empty() {
            FastPath::Skip
        } else {
            FastPath::Full
        });
    }

    if lock
        .packages
        .iter()
        .any(|package| matches!(package.source, index::DownloadSource::Workspace { .. }))
    {
        return Ok(FastPath::Full);
    }
    let environments: BTreeSet<Environment> = lock
        .packages
        .iter()
        .map(|package| package.environment)
        .collect();
    for environment in environments {
        let stamp = manifest
            .packages_out(environment)
            .join(".lpm")
            .join(STATE_FILE);
        if fs::read_to_string(&stamp).ok().as_deref() != Some(hash.as_str()) {
            return Ok(FastPath::Full);
        }
    }

    /* local inputs are unchanged, so the lock's own indices decide whether
    resolving could possibly answer differently: fresh caches mean it
    provably can't (same index state, same inputs, deterministic resolver),
    stale ones mean `^` requirements have a real question to ask */
    let fresh = lock
        .packages
        .iter()
        .map(|package| package.index.as_str())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .all(index::is_fresh);
    Ok(if fresh {
        FastPath::Skip
    } else {
        FastPath::Recheck
    })
}

/** whether a fresh resolution landed exactly where the lockfile already
stands — the "checked the indices, nothing new" case that skips the
rebuild. environment is compared only when resolution knows it up front;
None means "detect from the archive", and the same version of the same
archive detects the same. */
fn jobs_match_lock(jobs: &[Job], lock: &Lockfile) -> bool {
    if jobs.len() != lock.packages.len() {
        return false;
    }
    let by_name: HashMap<&str, &LockedPackage> = lock
        .packages
        .iter()
        .map(|package| (package.name.as_str(), package))
        .collect();
    jobs.iter().all(|job| {
        by_name.get(job.name.as_str()).is_some_and(|locked| {
            job.version == locked.version
                && job.link == locked.link
                && job.index_url == locked.index
                && job.source == locked.source
                /* redirects shape the nested links on disk; an [overrides]
                edit that lands on the same versions still has to rebuild */
                && job.redirects == locked.redirects
                && job
                    .environment
                    .is_none_or(|environment| environment == locked.environment)
        })
    })
}

/** the tail every skipped install shares: verify the pinned tools (a few
stats — install is also the repair path for wiped tool storage), download
any that are missing, and say so. */
fn finish_up_to_date(manifest: &Manifest, include_global_tools: bool) -> Result<(), Error> {
    let missing: Vec<ToolJob> = tool_jobs(manifest, include_global_tools)?
        .into_iter()
        .filter(|(alias, tool, _)| !tools::is_installed(alias, tool).unwrap_or(false))
        .collect();
    if missing.is_empty() {
        println!("Nothing to install (up to date)");
    } else {
        println!("Installing tools");
        let count = missing.len();
        ui::with_progress(count as u64, |bar| install_tools(&missing, bar))?;
        println!("Installed {count} tool{}", ui::plural(count));
    }
    Ok(())
}

/** stamps every environment that received packages with the state hash.
best-effort: a failed stamp only costs the next run its shortcut. the
manifest and tools inputs are the ones captured *before* resolution — what
this install actually consumed — while the lock text is read back fresh,
since this install just wrote it. */
fn write_state_stamps(
    manifest: &Manifest,
    environments: &BTreeSet<Environment>,
    inputs: &Option<(String, String)>,
) {
    let Some((manifest_text, tools_text)) = inputs else {
        return;
    };
    let Ok(lock_text) = fs::read_to_string(crate::project::lockfile::LOCKFILE) else {
        return;
    };
    let hash = state_hash(manifest_text, &lock_text, tools_text);
    for environment in environments {
        let dir = manifest.packages_out(*environment).join(".lpm");
        if fs::create_dir_all(&dir).is_ok() {
            let _ = fs::write(dir.join(STATE_FILE), &hash);
        }
    }
}

/** downloads, stages, and links every package, reporting progress on `bar`.
caller owns the bar's lifecycle so it gets cleared on errors too.

registry packages fan out over a small thread pool: each worker owns one
job at a time end to end (download, extract, rewrite, parse), under its
own staging dir, so nothing is shared but the target `.lpm` parents. this
thread keeps the bar, writes the link files, and fills `locked` by job
index, so lockfile order is exactly the resolver's order no matter who
finishes first. on the first error the pool stops taking work, in-flight
requests run out against their timeouts, and that error is returned. */
fn install_packages(
    manifest: &Manifest,
    jobs: Vec<Job>,
    staging_root: &Path,
    bar: &ProgressBar,
    cache: index::CachePolicy,
) -> Result<Vec<LockedPackage>, Error> {
    let mut slots: Vec<Option<LockedPackage>> = Vec::new();
    slots.resize_with(jobs.len(), || None);

    /* workspace members link in place on this thread (no network, and
    they touch source dirs the user owns); registry jobs queue for the pool */
    let mut queue: VecDeque<(usize, Job)> = VecDeque::new();
    for (slot, job) in jobs.into_iter().enumerate() {
        if matches!(job.source, index::DownloadSource::Workspace { .. }) {
            bar.set_message(job.name.clone());
            slots[slot] = Some(link_workspace_member(manifest, job, bar)?);
            bar.inc(1);
        } else {
            queue.push_back((slot, job));
        }
    }
    if queue.is_empty() {
        return Ok(slots.into_iter().flatten().collect());
    }

    if staging_root.exists() {
        fs::remove_dir_all(staging_root)?;
    }
    fs::create_dir_all(staging_root)?;

    let workers = queue.len().min(WORKERS);
    let queue = Mutex::new(queue);
    let failed = AtomicBool::new(false);
    let (sender, receiver) = std::sync::mpsc::channel();

    let mut first_error: Option<Error> = None;
    std::thread::scope(|scope| {
        for _ in 0..workers {
            let sender = sender.clone();
            let queue = &queue;
            let failed = &failed;
            scope.spawn(move || {
                loop {
                    if failed.load(Ordering::Relaxed) {
                        break;
                    }
                    let next = queue.lock().expect("job queue lock").pop_front();
                    let Some((slot, job)) = next else { break };

                    let staging = staging_root.join(job.name.replace('/', "_"));
                    let result = install_one(manifest, &job, &staging, cache, bar);
                    if result.is_err() {
                        failed.store(true, Ordering::Relaxed);
                    }
                    // a closed channel means the run is over; stop quietly
                    if sender.send((slot, job, result)).is_err() {
                        break;
                    }
                }
            });
        }
        drop(sender);

        /* errors are kept by lowest job index, not arrival order: when an
        outage fails several in-flight downloads at once, the error the
        user sees shouldn't depend on thread timing */
        let mut error_slot = usize::MAX;
        for (slot, job, result) in receiver {
            match result {
                Ok(extracted) => {
                    if let Err(error) = write_registry_link(manifest, &job, &extracted, bar) {
                        failed.store(true, Ordering::Relaxed);
                        if slot < error_slot {
                            error_slot = slot;
                            first_error = Some(error);
                        }
                        continue;
                    }
                    bar.set_message(job.name.clone());
                    ui::bar_println(
                        bar,
                        &ui::success_line(&format!(
                            "{}@{} → {}/{}",
                            job.name, job.version, extracted.environment, job.link
                        )),
                    );
                    bar.inc(1);
                    slots[slot] = Some(LockedPackage {
                        name: job.name,
                        version: job.version,
                        environment: extracted.environment,
                        link: job.link,
                        index: job.index_url,
                        redirects: job.redirects,
                        source: job.source,
                    });
                }
                Err(error) => {
                    if slot < error_slot {
                        error_slot = slot;
                        first_error = Some(error);
                    }
                }
            }
        }
    });

    if let Some(error) = first_error {
        return Err(error);
    }
    /* every slot must have reported; a hole here is an lpm bug, and
    silently writing a shorter lockfile would be far worse than failing */
    slots
        .into_iter()
        .map(|slot| {
            slot.ok_or_else(|| {
                Error::Io(std::io::Error::other(
                    "an install worker never reported its result; please report this as an lpm bug",
                ))
            })
        })
        .collect()
}

/// what a worker learned about one extracted package; links come later.
struct Extracted {
    environment: Environment,
    /// None = no entry point found (warned already at link time)
    entry: Option<String>,
    types: Vec<String>,
}

/** one registry package, end to end except the link file: download (or
cache hit), extract under `staging`, detect the environment, move into the
store, mirror project files, rewrite instance requires, and pull exported
types. runs on a worker thread. */
fn install_one(
    manifest: &Manifest,
    job: &Job,
    staging: &Path,
    cache: index::CachePolicy,
    bar: &ProgressBar,
) -> Result<Extracted, Error> {
    let started = Instant::now();
    if staging.exists() {
        with_retry(|| fs::remove_dir_all(staging))?;
    }
    index::download(&job.source, staging, cache)?;
    package::flatten_single_dir(staging)?;

    /* indices usually know the environment; otherwise ask the files
    (lpm.toml -> pesde.toml -> wally.toml) */
    let environment = match job.environment {
        Some(environment) => environment,
        None => package::environment(staging)
            .ok_or_else(|| Error::UnknownPackageEnvironment(job.name.clone()))?,
    };

    /* real contents live under <out>/.lpm/<scope>_<name>/. workers race to
    create the same .lpm parent (create_dir_all treats existing as success)
    and Windows occasionally answers concurrent creation or a fresh rename
    with a transient denial worth one retry */
    let folder = job.name.replace('/', "_");
    let out = manifest.packages_out(environment);
    let storage = out.join(".lpm").join(&folder);
    with_retry(|| fs::create_dir_all(storage.parent().expect("storage dir has a parent")))?;
    if storage.exists() {
        with_retry(|| fs::remove_dir_all(&storage))?;
    }
    with_retry(|| fs::rename(staging, &storage))?;

    /* a project file the package ships would mount it under its own
    name and tree, which our link files (and the package's own relative
    requires) don't spell; make it mirror the disk instead. after
    entry_point, so the entry is read from what the package shipped */
    let entry = package::entry_point(&storage);
    rojo::mirror_disk_layout(&storage, &mut bar_warn(bar));

    let (entry, types) = match entry {
        Some(entry) => {
            /* packages from any index can talk roblox instance paths
            (require(script.Parent.X)), wally stuff especially but ports
            published to pesde or our index too; rewrite what we can into
            string requires so they work without an instance tree. string
            require packages come through unchanged */
            requires::rewrite_instance_requires(&storage, &entry)?;
            let types = link_types(&storage, &entry, &job.name, &mut bar_warn(bar));
            (Some(entry), types)
        }
        None => (None, Vec::new()),
    };

    // suspend so a timings line can't tear the live bar from a worker thread
    bar.suspend(|| ui::timing(&format!("package {}", job.name), started));
    Ok(Extracted {
        environment,
        entry,
        types,
    })
}

/// one retry after a beat, for transient Windows file-sharing refusals.
fn with_retry(operation: impl Fn() -> std::io::Result<()>) -> std::io::Result<()> {
    operation().or_else(|_| {
        std::thread::sleep(std::time::Duration::from_millis(50));
        operation()
    })
}

/** the consumer-level `<out>/<link>.luau` for one extracted package;
runs on the main thread once a worker reports in. */
fn write_registry_link(
    manifest: &Manifest,
    job: &Job,
    extracted: &Extracted,
    bar: &ProgressBar,
) -> Result<(), Error> {
    match &extracted.entry {
        Some(entry) => {
            let out = manifest.packages_out(extracted.environment);
            let folder = job.name.replace('/', "_");
            let link_path = out.join(format!("{}.luau", job.link));
            fs::write(
                &link_path,
                package::link_contents(&folder, entry, &extracted.types),
            )?;
        }
        None => warn_no_entry(&job.name, bar),
    }
    Ok(())
}

/** workspace members link in place (no download, no copy under .lpm/)
so edits to the member are picked up without reinstalling, like pesde's
symlinks. */
fn link_workspace_member(
    manifest: &Manifest,
    job: Job,
    bar: &ProgressBar,
) -> Result<LockedPackage, Error> {
    let index::DownloadSource::Workspace { path } = &job.source else {
        unreachable!("only workspace jobs are linked in place");
    };
    let member_dir = Path::new(path);
    let environment = job
        .environment
        .ok_or_else(|| Error::UnknownPackageEnvironment(job.name.clone()))?;
    let out = manifest.packages_out(environment);
    fs::create_dir_all(&out)?;

    match package::entry_point(member_dir) {
        Some(entry) => {
            let mut require = workspace::relative_path(&out, member_dir);
            if !entry.is_empty() {
                require = format!("{require}/{entry}");
            }
            if !require.starts_with("..") {
                require = format!("./{require}");
            }
            let types = link_types(member_dir, &entry, &job.name, &mut bar_warn(bar));
            let link_path = out.join(format!("{}.luau", job.link));
            fs::write(&link_path, package::link_contents_at(&require, &types))?;
        }
        None => warn_no_entry(&job.name, bar),
    }

    ui::bar_println(
        bar,
        &ui::success_line(&format!(
            "{}@{} → {}/{} (workspace)",
            job.name, job.version, environment, job.link
        )),
    );
    Ok(LockedPackage {
        name: job.name,
        version: job.version,
        environment,
        link: job.link,
        index: job.index_url,
        redirects: job.redirects,
        source: job.source,
    })
}

/** one installed package, as the nested-link pass sees it. */
struct StoredPackage {
    /// lowercased "scope/name", the form dependency declarations resolve by
    name: String,
    /// where the contents live: <out>/.lpm/<scope>_<name>, or a member's own directory
    storage: PathBuf,
    environment: Environment,
    /// a workspace member: fine to link *to*, never written into
    in_place: bool,
    /** [overrides]-rewritten edges: declared alias -> replacement package
    name. the manifest on disk still declares the original, so link
    generation must consult this before trusting what it reads back. */
    redirects: BTreeMap<String, String>,
}

/** link files *inside* a stored package for its own declared dependencies,
at `<storage>/packages/<env>/<alias>.luau`. that folder name is the default
layout, literally: published code was compiled against it (see Chief's
build), so a consumer's `[config]` output customization deliberately doesn't
apply inside packages. (a package published from a project that customized
*its* output dirs would want those names instead; nothing has needed that
yet.) each link requires the dependency's store entry directly.

nothing here is fatal: the install has already downloaded and extracted
everything, so a package that can't be linked warns and is skipped rather
than taking the whole run (and the lockfile write that follows) with it. */
fn link_nested_dependencies(packages: &[StoredPackage], warn: &mut impl FnMut(String)) {
    let by_name: HashMap<&str, &StoredPackage> = packages
        .iter()
        .map(|package| (package.name.as_str(), package))
        .collect();
    // exported types depend only on the dependency, so parse each once
    let mut types_cache: HashMap<String, Vec<String>> = HashMap::new();

    for package in packages.iter().filter(|package| !package.in_place) {
        for (alias, declared) in package::declared_dependencies(&package.storage) {
            /* the shipped manifest declares the ORIGINAL package; an
            [overrides] redirect on this edge means the alias must link to
            the replacement instead */
            let dependency = package
                .redirects
                .get(&alias)
                .map_or(declared, |replacement| replacement.clone());
            /* aliases are TOML keys from a *downloaded* manifest, and TOML
            keys can be quoted anything: a "../.." or absolute one would put
            the link outside the package (or outside the project) */
            if !is_plain_file_name(&alias) {
                warn(format!(
                    "warning: {} declares dependency {dependency} under the unusable alias '{alias}'; no nested link generated",
                    package.name
                ));
                continue;
            }
            let Some(dep) = by_name.get(dependency.as_str()) else {
                warn(format!(
                    "warning: {} declares dependency {dependency} which is not installed; no nested link generated",
                    package.name
                ));
                continue;
            };
            let Some(entry) = package::entry_point(&dep.storage) else {
                warn(format!(
                    "warning: could not find an entry point for {dependency}; no nested link generated in {}",
                    package.name
                ));
                continue;
            };

            let link_dir = package
                .storage
                .join("packages")
                .join(dep.environment.dir_name());
            if let Err(error) = fs::create_dir_all(&link_dir) {
                warn(format!(
                    "warning: could not create {} ({error}); no nested link generated for {dependency}",
                    link_dir.display()
                ));
                continue;
            }

            /* relative_path is lexical, so both sides go through
            std::path::absolute first: that drops the "." component a
            `[config]` dir like "./packages/shared" would otherwise
            contribute, and gives absolute out dirs a common prefix to
            measure from */
            let from = std::path::absolute(&link_dir).unwrap_or_else(|_| link_dir.clone());
            let to = std::path::absolute(&dep.storage).unwrap_or_else(|_| dep.storage.clone());
            let mut require = workspace::relative_path(&from, &to);
            if !entry.is_empty() {
                require = format!("{require}/{entry}");
            }
            if !require.starts_with("..") {
                require = format!("./{require}");
            }

            let types = types_cache
                .entry(dependency.clone())
                .or_insert_with(|| {
                    /* install_packages already parsed (and complained about)
                    every stored entry point, so no warn sink here: it would
                    just say the same thing twice */
                    package::entry_source(&dep.storage, &entry)
                        .and_then(|path| fs::read_to_string(path).ok())
                        .and_then(|source| package::exported_types(&source))
                        .unwrap_or_default()
                })
                .clone();
            let link_path = link_dir.join(format!("{alias}.luau"));
            if let Err(error) = fs::write(&link_path, package::link_contents_at(&require, &types)) {
                warn(format!(
                    "warning: could not write {} ({error})",
                    link_path.display()
                ));
            }
        }
    }
}

/// one ordinary path segment: no separators, no drive letter, not `.`/`..`.
fn is_plain_file_name(alias: &str) -> bool {
    !alias.is_empty()
        && alias != "."
        && alias != ".."
        && !alias.contains(['/', '\\', ':'])
        && !Path::new(alias).is_absolute()
}

/** exported types have to be restated in the link file to survive the
wrapper; an unparseable entry point still links, just without its types. */
fn link_types(
    package_dir: &Path,
    entry: &str,
    name: &str,
    warn: &mut impl FnMut(String),
) -> Vec<String> {
    let Some(source) =
        package::entry_source(package_dir, entry).and_then(|path| fs::read_to_string(path).ok())
    else {
        return Vec::new();
    };
    package::exported_types(&source).unwrap_or_else(|| {
        warn(format!(
            "warning: could not parse the entry point of {name}; its types are not re-exported"
        ));
        Vec::new()
    })
}

/// routes a warning line around the live progress bar.
fn bar_warn(bar: &ProgressBar) -> impl FnMut(String) + '_ {
    move |message: String| bar.suspend(|| eprintln!("{message}"))
}

fn warn_no_entry(name: &str, bar: &ProgressBar) {
    bar.suspend(|| {
        eprintln!("warning: could not find an entry point for {name}; no link file generated")
    });
}

fn install_tools(jobs: &[ToolJob], bar: &ProgressBar) -> Result<(), Error> {
    let github = GithubAPI::new();
    for (alias, tool, global) in jobs {
        bar.set_message(tool.repository.clone());
        let downloaded = tools::install_tool(alias, tool, &github)?;
        let mut notes = Vec::new();
        if *global {
            notes.push("global");
        }
        if !downloaded {
            notes.push("cached");
        }
        let notes = if notes.is_empty() {
            String::new()
        } else {
            format!(" ({})", notes.join(", "))
        };
        ui::bar_println(
            bar,
            &ui::success_line(&format!(
                "{}@{} → {alias}{notes}",
                tool.repository, tool.version
            )),
        );

        /* another toolchain manager's shim earlier in PATH (aftman, rokit)
        would run instead of ours and report its own errors; surface that
        or the tool looks broken for no visible reason */
        if let Some(shadow) = tools::shim::shadowing_executable(alias) {
            bar.suspend(|| {
                eprintln!(
                    "warning: `{alias}` resolves to {} on PATH before lpm's shims; that copy will run instead",
                    shadow.display()
                )
            });
        }
        bar.inc(1);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicUsize;

    #[test]
    fn state_hash_tracks_every_input() {
        let base = state_hash("manifest", "lock", "tools");
        assert!(base.starts_with("lpm-state-v1:"));
        assert_eq!(base, state_hash("manifest", "lock", "tools"));

        // each input moves the hash, and boundaries are unambiguous
        assert_ne!(base, state_hash("manifest2", "lock", "tools"));
        assert_ne!(base, state_hash("manifest", "lock2", "tools"));
        assert_ne!(base, state_hash("manifest", "lock", "tools2"));
        assert_ne!(
            state_hash("ab", "c", ""),
            state_hash("a", "bc", ""),
            "input boundaries must be part of the hash"
        );
    }

    #[test]
    fn with_retry_retries_exactly_once() {
        let attempts = AtomicUsize::new(0);
        with_retry(|| {
            if attempts.fetch_add(1, Ordering::Relaxed) == 0 {
                Err(std::io::Error::other("transient"))
            } else {
                Ok(())
            }
        })
        .unwrap();
        assert_eq!(attempts.load(Ordering::Relaxed), 2);

        let attempts = AtomicUsize::new(0);
        assert!(
            with_retry(|| {
                attempts.fetch_add(1, Ordering::Relaxed);
                Err::<(), _>(std::io::Error::other("permanent"))
            })
            .is_err()
        );
        assert_eq!(attempts.load(Ordering::Relaxed), 2);
    }

    #[test]
    fn recheck_matches_resolutions_against_the_lock() {
        let job = |version: &str, url: &str| Job {
            name: "acme/thing".to_string(),
            version: version.to_string(),
            environment: None,
            source: index::DownloadSource::Zip {
                url: url.to_string(),
            },
            index_url: "https://example.com/index".to_string(),
            link: "thing".to_string(),
            redirects: BTreeMap::new(),
        };
        let lock = Lockfile::new(vec![LockedPackage {
            name: "acme/thing".to_string(),
            version: "1.0.0".to_string(),
            environment: Environment::Shared,
            link: "thing".to_string(),
            index: "https://example.com/index".to_string(),
            redirects: BTreeMap::new(),
            source: index::DownloadSource::Zip {
                url: "https://example.com/thing/1.0.0".to_string(),
            },
        }]);

        // identical resolution: rebuild skippable (env None tolerated)
        assert!(jobs_match_lock(
            &[job("1.0.0", "https://example.com/thing/1.0.0")],
            &lock
        ));
        // a new version is `^` doing its job: rebuild
        assert!(!jobs_match_lock(
            &[job("1.1.0", "https://example.com/thing/1.1.0")],
            &lock
        ));
        // same version from somewhere else is also a change
        assert!(!jobs_match_lock(
            &[job("1.0.0", "https://elsewhere.example/thing/1.0.0")],
            &lock
        ));
        // count mismatches never match
        assert!(!jobs_match_lock(&[], &lock));
        // a resolved environment must agree with the locked one
        let mut server = job("1.0.0", "https://example.com/thing/1.0.0");
        server.environment = Some(Environment::Server);
        assert!(!jobs_match_lock(&[server], &lock));
        let mut shared = job("1.0.0", "https://example.com/thing/1.0.0");
        shared.environment = Some(Environment::Shared);
        assert!(jobs_match_lock(&[shared], &lock));
    }

    #[test]
    fn state_stamps_land_in_each_environment() {
        let base = std::env::temp_dir().join("lpm-test-state-stamps");
        let _ = fs::remove_dir_all(&base);
        let manifest: Manifest = toml::from_str(&format!(
            "[package]\nname = \"acme/x\"\nversion = \"0.1.0\"\n\n[config]\n\
             shared-packages-out = \"{0}/packages/shared\"\n\
             luau-packages-out = \"{0}/packages/luau\"\n",
            base.to_string_lossy().replace('\\', "/")
        ))
        .unwrap();

        /* the lock half of the hash reads from the cwd (the repo root under
        cargo test); the stamps just have to be consistent with whatever it
        hashes to, and absent when there's no lockfile at all */
        let inputs = Some(("manifest text".to_string(), "<absent>".to_string()));
        let environments: BTreeSet<Environment> = [Environment::Shared, Environment::Luau]
            .into_iter()
            .collect();
        write_state_stamps(&manifest, &environments, &inputs);

        let Ok(lock_text) = fs::read_to_string(crate::project::lockfile::LOCKFILE) else {
            assert!(!base.join("packages/shared/.lpm").join(STATE_FILE).exists());
            let _ = fs::remove_dir_all(&base);
            return;
        };
        let expected = state_hash("manifest text", &lock_text, "<absent>");
        for environment in ["shared", "luau"] {
            assert_eq!(
                fs::read_to_string(
                    base.join("packages")
                        .join(environment)
                        .join(".lpm")
                        .join(STATE_FILE)
                )
                .unwrap(),
                expected
            );
        }
        // environments that received nothing get no stamp
        assert!(!base.join("packages/server").exists());

        let _ = fs::remove_dir_all(&base);
    }

    fn write(dir: &Path, file: &str, contents: &str) {
        let path = dir.join(file);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, contents).unwrap();
    }

    fn stored(name: &str, storage: PathBuf, environment: Environment) -> StoredPackage {
        StoredPackage {
            // production lowercases here; mirror that so the tests exercise it
            name: name.to_lowercase(),
            storage,
            environment,
            in_place: false,
            redirects: BTreeMap::new(),
        }
    }

    #[test]
    fn writes_nested_links_for_stored_dependencies() {
        let base = std::env::temp_dir().join("lpm-test-nested-links");
        let _ = fs::remove_dir_all(&base);
        let shared = base.join("packages/shared");
        let luau = base.join("packages/luau");

        /* chief-shaped fixture: `lifecycles` depends on same-environment
        `core` (which exports a type) and cross-environment `util` (whose
        entry is its root init file) */
        let core = shared.join(".lpm/acme_core");
        write(&core, "lpm.toml", "[target]\nmain = \"out/lpm\"\n");
        write(
            &core,
            "out/lpm/init.luau",
            "export type Entry = { id: number }\nreturn {}\n",
        );
        let util = luau.join(".lpm/acme_util");
        write(&util, "init.luau", "return {}\n");
        let lifecycles = shared.join(".lpm/acme_lifecycles");
        write(
            &lifecycles,
            "lpm.toml",
            "[dependencies]\ncore = { name = \"acme/core\", version = \"^\" }\n\
             util = { name = \"acme/util\", version = \"^\" }\n",
        );
        write(&lifecycles, "out/lpm/init.luau", "return {}\n");

        let packages = [
            stored("Acme/Core", core.clone(), Environment::Shared),
            stored("acme/util", util, Environment::Luau),
            stored("acme/lifecycles", lifecycles.clone(), Environment::Shared),
        ];
        let mut warnings = Vec::new();
        link_nested_dependencies(&packages, &mut |message| warnings.push(message));
        assert_eq!(warnings, Vec::<String>::new());

        /* the same-environment link: three hops up to the store, entry
        appended, exported types restated */
        assert_eq!(
            fs::read_to_string(lifecycles.join("packages/shared/core.luau")).unwrap(),
            "local module = require(\"../../../acme_core/out/lpm\")\n\
             export type Entry = module.Entry\n\
             return module\n"
        );
        /* the cross-environment link climbs out to the other output root;
        a root-init entry adds no suffix */
        assert_eq!(
            fs::read_to_string(lifecycles.join("packages/luau/util.luau")).unwrap(),
            "return require(\"../../../../../luau/.lpm/acme_util\")\n"
        );
        // packages without dependencies get no packages/ folder at all
        assert!(!core.join("packages").exists());

        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn redirected_edges_link_the_replacement() {
        let base = std::env::temp_dir().join("lpm-test-nested-links-redirect");
        let _ = fs::remove_dir_all(&base);
        let shared = base.join("packages/shared");

        /* the shipped manifest still declares acme/bar, but [overrides]
        swapped the edge for acme/qux: the nested link must follow the
        redirect, not the manifest */
        let qux = shared.join(".lpm/acme_qux");
        write(&qux, "init.luau", "return {}\n");
        let consumer = shared.join(".lpm/acme_foo");
        write(
            &consumer,
            "lpm.toml",
            "[dependencies]\nbar = { name = \"acme/bar\", version = \"^1\" }\n",
        );

        let mut redirected = stored("acme/foo", consumer.clone(), Environment::Shared);
        redirected.redirects = [("bar".to_string(), "acme/qux".to_string())].into();
        let packages = [stored("acme/qux", qux, Environment::Shared), redirected];
        let mut warnings = Vec::new();
        link_nested_dependencies(&packages, &mut |message| warnings.push(message));

        assert_eq!(warnings, Vec::<String>::new());
        assert_eq!(
            fs::read_to_string(consumer.join("packages/shared/bar.luau")).unwrap(),
            "return require(\"../../../acme_qux\")\n"
        );

        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn missing_dependencies_warn_and_skip() {
        let base = std::env::temp_dir().join("lpm-test-nested-links-missing");
        let _ = fs::remove_dir_all(&base);
        let storage = base.join("packages/shared/.lpm/acme_thing");
        write(
            &storage,
            "lpm.toml",
            "[dependencies]\ngone = { name = \"acme/gone\", version = \"^\" }\n",
        );

        let packages = [stored("acme/thing", storage.clone(), Environment::Shared)];
        let mut warnings = Vec::new();
        link_nested_dependencies(&packages, &mut |message| warnings.push(message));

        assert_eq!(warnings.len(), 1);
        assert!(warnings[0].contains("acme/gone"));
        assert!(warnings[0].contains("not installed"));
        assert!(!storage.join("packages").exists());

        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn aliases_cannot_escape_the_package() {
        let base = std::env::temp_dir().join("lpm-test-nested-links-escape");
        let _ = fs::remove_dir_all(&base);
        let shared = base.join("packages/shared");

        let dep = shared.join(".lpm/acme_dep");
        write(&dep, "init.luau", "return {}\n");
        /* a downloaded manifest can quote anything as a key; neither of
        these may put a file outside the package */
        let hostile = shared.join(".lpm/acme_hostile");
        write(
            &hostile,
            "lpm.toml",
            "[dependencies]\n\"../../../../../../escaped\" = { name = \"acme/dep\", version = \"^\" }\n\
             \"C:/Windows/Temp/lpm-escaped\" = { name = \"acme/dep\", version = \"^\" }\n",
        );

        let packages = [
            stored("acme/dep", dep, Environment::Shared),
            stored("acme/hostile", hostile.clone(), Environment::Shared),
        ];
        let mut warnings = Vec::new();
        link_nested_dependencies(&packages, &mut |message| warnings.push(message));

        assert_eq!(warnings.len(), 2, "{warnings:?}");
        assert!(warnings.iter().all(|line| line.contains("unusable alias")));
        assert!(!hostile.join("packages").exists());
        assert!(!base.join("escaped.luau").exists());
        assert!(!Path::new("C:/Windows/Temp/lpm-escaped.luau").exists());

        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn workspace_members_are_link_targets_but_never_written_into() {
        let base = std::env::temp_dir().join("lpm-test-nested-links-member");
        let _ = fs::remove_dir_all(&base);

        /* a member consumed by the root shadows the registry copy of the
        same name, so packages depending on it must still link */
        let member = base.join("packages/core");
        write(&member, "lpm.toml", "[target]\nmain = \"src/init.luau\"\n");
        write(&member, "src/init.luau", "return {}\n");
        let consumer = base.join("packages/shared/.lpm/acme_extras");
        write(
            &consumer,
            "lpm.toml",
            "[dependencies]\ncore = { name = \"acme/core\", version = \"^\" }\n",
        );

        let packages = [
            StoredPackage {
                name: "acme/core".to_string(),
                storage: member.clone(),
                environment: Environment::Shared,
                in_place: true,
                redirects: BTreeMap::new(),
            },
            stored("acme/extras", consumer.clone(), Environment::Shared),
        ];
        let mut warnings = Vec::new();
        link_nested_dependencies(&packages, &mut |message| warnings.push(message));

        assert_eq!(warnings, Vec::<String>::new());
        assert_eq!(
            fs::read_to_string(consumer.join("packages/shared/core.luau")).unwrap(),
            "return require(\"../../../../../core/src\")\n"
        );
        // the member's own source tree stays untouched
        assert!(!member.join("packages").exists());

        let _ = fs::remove_dir_all(&base);
    }
}
