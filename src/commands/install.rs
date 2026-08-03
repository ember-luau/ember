use crate::error::Error;
use crate::net::github::GithubAPI;
use crate::project::hooks::{self, Lifecycle};
use crate::project::lockfile::{LockedPackage, Lockfile, PatchRecord};
use crate::project::manifest::{self, Environment, Manifest, Tool};
use crate::project::package;
use crate::project::requires;
use crate::project::rojo;
use crate::project::workspace::{self, Workspace};
use crate::registry::index;
use crate::registry::resolver;
use crate::sys::git;
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
releases, rebuilding only when something actually resolved differently. \
Two things this check cannot see: hand-edits inside an installed packages \
folder, and a repository that commits its packages folders (those packages \
really are present, and tools still verify). --refresh forces the full \
pipeline.")]
pub struct InstallArgs {
    /// Install exactly what lpm.lock records, without re-resolving
    #[arg(long)]
    pub locked: bool,

    /// Re-resolve, re-fetch indices, and re-download archives even when caches are fresh
    #[arg(long)]
    pub refresh: bool,
}

struct Job {
    name: String,
    version: String,
    /// the package's own environment, None means detect from the archive.
    environment: Option<Environment>,
    /// the output root, the environment of the dependency tree's root.
    context: Environment,
    source: index::DownloadSource,
    index_url: String,
    /// top level link name, None for transitives, they never get one.
    link: Option<String>,
    /// [overrides] rewritten edges of this package, declared alias -> replacement name.
    redirects: BTreeMap<String, String>,
    /// the [patches] entry this copy gets, when one names it.
    patch: Option<JobPatch>,
}

/** a patch ready to apply, the lockfile record plus the file resolved
to an absolute path up front. workers apply it while `workspace::in_dir`
may have moved the process cwd. */
#[derive(Clone)]
struct JobPatch {
    record: PatchRecord,
    file: PathBuf,
}

/// how many packages download and unpack at once.
const WORKERS: usize = 8;

/// stamp file the fast path reads, under each `<out>/.lpm/`.
const STATE_FILE: &str = ".state";

/// alias, pinned tool, and whether the pin came from ~/.lpm/tools.toml.
type ToolJob = (String, Tool, bool);

pub fn run(args: InstallArgs) -> Result<(), Error> {
    let manifest = Manifest::load()?;

    /* the root's hooks bracket the whole command, members included, so a
    `postinstall` that builds the project sees a fully installed tree.
    each member's own hooks bracket only that member, in its own directory */
    let lifecycle = Lifecycle::of(&manifest, hooks::INSTALL);
    lifecycle.before()?;

    install_project(&args, &manifest, true)?;

    /* a workspace root installs every member too, pesde's order, root
    first then each member. member installs never recurse further, nested
    workspaces aren't a thing */
    if !manifest.workspace_members().is_empty() {
        let workspace = Workspace::open(Path::new("."))?;
        for member in &workspace.members {
            if member.dir == workspace.root {
                continue;
            }
            println!("Installing {}", member.manifest.package.name);
            workspace::in_dir(&member.dir, || {
                let manifest = Manifest::load()?;
                let lifecycle = Lifecycle::of(&manifest, hooks::INSTALL);
                lifecycle.before()?;
                install_project(&args, &manifest, false)?;
                lifecycle.after()
            })?;
        }
    }

    lifecycle.after()
}

/** installs one project from the current directory. global tools only
install on the primary run since workspace members share them and
repeating the merge per member would just re-print every pin. */
fn install_project(
    args: &InstallArgs,
    manifest: &Manifest,
    include_global_tools: bool,
) -> Result<(), Error> {
    let started = Instant::now();

    /* fast path. nothing local changed since the last install, so the
    lockfile is what would resolve anyway. within the index TTL that's a
    certainty and everything is skipped. past it, `^` requirements have
    earned a real look, resolution runs below and only a resolution that
    lands somewhere new triggers a rebuild. tools are verified either way,
    cheap stats, install doubles as their repair path. */
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

    /* captured before resolution. the stamp must record the inputs this
    install consumed, not whatever is on disk once it finishes, an edit
    landing mid-install has to bust the next fast path */
    let state_inputs = state_inputs(include_global_tools);

    let mut jobs: Vec<Job> = if args.locked {
        let lock = Lockfile::load()?;
        /* a v1 lock predates contexts. replaying it would scatter a
        dependent and its deps across environment roots, and the nested
        pass looks up within one context so it would silently link
        nothing. --locked never rewrites the lock, refuse loudly
        instead of shipping a broken tree forever */
        if lock.version < 2 {
            return Err(Error::LockfileOutdated(lock.version));
        }
        /* --locked replays the lock's own patch records without consulting
        [patches] at all. a recorded file that's gone or edited is drift
        the lock can't vouch for, so it fails here before any download */
        lock.packages
            .into_iter()
            .map(|package| {
                let patch = locked_patch(&package.name, Path::new("."), package.patch)?;
                Ok(Job {
                    name: package.name,
                    version: package.version,
                    environment: Some(package.environment),
                    context: package.context.unwrap_or(package.environment),
                    source: package.source,
                    index_url: package.index,
                    link: package.link,
                    redirects: package.redirects,
                    patch,
                })
            })
            .collect::<Result<Vec<Job>, Error>>()?
    } else {
        let resolve_started = Instant::now();
        let mode = if args.refresh {
            index::Refresh::Force
        } else {
            index::Refresh::Ttl
        };
        /* collected, not printed, inside the spinner. a bare eprintln
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
                context: package.context,
                source: package.source,
                index_url: package.index_url,
                link: package.link,
                redirects: package.redirects,
                patch: None,
            })
            .collect()
    };

    /* [patches] lands on the resolved graph before anything downloads. a
    key that no longer matches, version drift or package gone, is an error
    here, never a silent skip, an unapplied patch could be someone's
    security fix. --locked skipped this on purpose, its patches came from
    the lock above */
    if !args.locked {
        attach_patches(manifest, Path::new("."), &mut jobs)?;
    }

    /* two [config] keys pointed at one folder would let one context's
    storage silently overwrite the other's. refuse before touching disk */
    assert_distinct_roots(manifest, &jobs)?;

    /* recheck half of the fast path. local inputs were unchanged but the
    indices were stale, so resolution ran fresh and pulled them. when it
    lands exactly on the lockfile, the overwhelmingly common outcome, the
    rebuild is skipped and the refreshed TTL stamps make the next few
    installs full skips. anything new resolves fall through to the
    rebuild, which is `^` doing its job */
    if fast == FastPath::Recheck
        && let Ok(lock) = Lockfile::load()
        && jobs_match_lock(&jobs, &lock)
    {
        finish_up_to_date(manifest, include_global_tools)?;
        ui::timing("recheck total", started);
        return Ok(());
    }

    /* before the wipe below, which would otherwise take a [config]-claimed
    packages/shared with it and leave the guard inside with nothing to see */
    remove_legacy_roblox_out(manifest);

    /* installs rebuild from scratch each run. every env's output folder is
    wiped even with nothing to install, so removing the last dependency
    leaves no stale packages */
    for environment in Environment::ALL {
        let out = manifest.packages_out(environment);
        if out.exists() {
            fs::remove_dir_all(&out)?;
        }
    }

    /* extraction can happen before we know the environment and so the
    output folder, so stage in a project local temp dir. a rename then
    moves it into place, same filesystem as the outputs */
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

    /* second pass now that everything is extracted, link files inside
    each stored package for the dependencies it declares */
    let stored: Vec<StoredPackage> = locked
        .iter()
        .map(|package| match &package.source {
            /* members link in place, so they're link targets like anything
            else but never get written into, that would dirty a source tree
            the user edits */
            index::DownloadSource::Workspace { path } => StoredPackage {
                name: package.name.to_lowercase(),
                storage: PathBuf::from(path),
                environment: package.environment,
                context: package.context(),
                in_place: true,
                redirects: package.redirects.clone(),
            },
            _ => StoredPackage {
                name: package.name.to_lowercase(),
                storage: manifest
                    .packages_out(package.context())
                    .join(".lpm")
                    .join(package.name.replace('/', "_")),
                environment: package.environment,
                context: package.context(),
                in_place: false,
                redirects: package.redirects.clone(),
            },
        })
        .collect();
    let nested_started = Instant::now();
    link_nested_dependencies(&stored, &mut |message| eprintln!("{message}"));
    ui::timing("nested-links", nested_started);

    let package_count = locked.len();
    // stamps land in the folders actually written, the contexts
    let environments: BTreeSet<Environment> =
        locked.iter().map(|package| package.context()).collect();
    if !args.locked {
        // written even when empty, so lpm.lock always mirrors the manifest
        Lockfile::new(locked).save()?;
    }

    /* tool versions are exact pins, so no lockfile entries. normal and
    --locked runs install them the same way. global tools from
    ~/.lpm/tools.toml install here too, `tool add` never downloads, this
    is the one place every tool gets installed */
    let tool_jobs = tool_jobs(manifest, include_global_tools)?;
    let tool_count = tool_jobs.len();
    if !tool_jobs.is_empty() {
        println!("Installing tools");
        ui::with_progress(tool_count as u64, |bar| install_tools(&tool_jobs, bar))?;
    }

    /* everything succeeded, record what this install saw so the next run
    can skip itself when nothing changed. never on --locked, that path
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

/** the manifest's tools plus global ones on the primary run, deduped.
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

/** what the fast path saw when it decided to skip, one hash over every
local input that can change what an install produces. coarse on purpose,
a comment edit in lpm.toml busts the fast path and wastes a few
milliseconds, anything finer risks missing an input and breaking an
install. */
fn state_hash(
    manifest_text: &str,
    lock_text: &str,
    tools_text: &str,
    patches_text: &str,
) -> String {
    let hash = fnv1a_parts(&[
        manifest_text.as_bytes(),
        lock_text.as_bytes(),
        tools_text.as_bytes(),
        /* fnv1a_parts is length prefixed, so growing a fourth part just
        moves every hash once, one rebuild, no version bump needed */
        patches_text.as_bytes(),
    ]);
    /* v2 was bumped with the self contained roots layout change, so every
    project's first install under the new binary misses its old stamps and
    rebuilds into the new layout. the fast path can't skip past a
    migration it can't see in its inputs */
    format!("lpm-state-v2:{hash:016x}\n")
}

/** the manifest and global tools inputs of the state hash, as text. read
once per install, before resolution, so an edit landing mid-install can
never be stamped as satisfied. None means an input exists but can't be
read, the fast path then stays off, the safe direction.

member installs don't consume global tools (include_global = false), so
those hash a fixed marker instead of the file, editing ~/.lpm/tools.toml
shouldn't wipe and rebuild every member of a workspace. absence is a
distinct marker too, not an empty string. */
fn state_inputs(include_global: bool) -> Option<(String, String, String)> {
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
    let patches_text = patches_state_text(&manifest_text);
    Some((manifest_text, tools_text, patches_text))
}

/** the [patches] files' bytes, concatenated in manifest key order, so
editing a patch file without touching lpm.toml still busts the fast path.
a missing file is a distinct marker, not an empty string, same discipline
as the global tools file's `<absent>`. */
fn patches_state_text(manifest_text: &str) -> String {
    let Ok(manifest) = toml::from_str::<Manifest>(manifest_text) else {
        /* an unparseable manifest can't say which patches exist. the fast
        path fails elsewhere on the same input, this just stays stable */
        return "<unparseable manifest>".to_string();
    };
    let mut text = String::new();
    for (key, path) in &manifest.patches {
        text.push_str(key);
        text.push('\0');
        match fs::read_to_string(path) {
            Ok(contents) => text.push_str(&contents),
            Err(_) => text.push_str("<missing>"),
        }
        text.push('\0');
    }
    text
}

/// the full state hash as things stand right now, None if any input is unreadable.
fn current_state_hash(include_global: bool) -> Option<String> {
    let (manifest_text, tools_text, patches_text) = state_inputs(include_global)?;
    let lock_text = fs::read_to_string(crate::project::lockfile::LOCKFILE).ok()?;
    Some(state_hash(
        &manifest_text,
        &lock_text,
        &tools_text,
        &patches_text,
    ))
}

/// how much of an install the unchanged-inputs check lets us skip.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum FastPath {
    /// nothing changed and the indices are TTL fresh, skip everything
    Skip,
    /** nothing changed locally but the indices are stale. resolve for
    real so `^` can pick up new releases, then skip the rebuild if
    resolution lands exactly on the lockfile */
    Recheck,
    /// something changed or the check doesn't apply, full install
    Full,
}

/** decides how much of this install can be skipped, from the stamps the
last successful install left in each output folder.

never anything but Full for workspaces, in either direction, a root's
install reads member manifests and member source, none of which these
stamps see. two documented blind spots, both shared with pesde.
hand edits inside an output folder, and a repo that commits its
packages folders, those packages really are present and tools still
verify, so skipping is right. */
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
        return Ok(FastPath::Full); // no lockfile yet, or an unreadable input
    };
    let Ok(lock) = Lockfile::load() else {
        return Ok(FastPath::Full); // unparseable lockfile, the full path deals with it
    };

    /* an empty lock matches an empty manifest and nothing else. with no
    output dirs there are no stamps to consult, but resolving zero
    dependencies can only produce zero packages, nothing `^` could ever
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
        .map(|package| package.context())
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
    resolving could possibly answer differently. fresh caches mean it
    provably can't, same index state, same inputs, deterministic resolver.
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

/** resolves [patches] against what this install will actually build and
hangs each patch on its matching jobs. every key must land exactly. a key
naming a package that resolved to a different version or isn't in the
graph at all fails before any download, and one key must mean one
archive, a multi target pesde package resolving to different archives
under different roots is refused rather than half patched. */
fn attach_patches(manifest: &Manifest, project_dir: &Path, jobs: &mut [Job]) -> Result<(), Error> {
    for (key, path) in &manifest.patches {
        let (name, version) = manifest::patch_key(key)?;
        let relative = manifest::patch_path(key, path)?;

        let matching: Vec<usize> = jobs
            .iter()
            .enumerate()
            .filter(|(_, job)| job.name == name)
            .map(|(slot, _)| slot)
            .collect();
        if matching.is_empty() {
            return Err(Error::PatchDrift {
                name: name.clone(),
                version: version.to_string(),
                reason: format!("{name} is not a dependency of this project"),
            });
        }

        let version_text = version.to_string();
        let hits: Vec<usize> = matching
            .iter()
            .copied()
            .filter(|slot| jobs[*slot].version == version_text)
            .collect();
        if hits.is_empty() {
            return Err(Error::PatchDrift {
                name: name.clone(),
                version: version_text,
                reason: format!("{name} resolved to {}", jobs[matching[0]].version),
            });
        }
        /* contexts resolve independently, so one name CAN land at different
        versions under different roots. a copy the key doesn't cover would
        install unpatched, which is the silent half skip this feature must
        never do */
        if hits.len() != matching.len() {
            let other = matching
                .iter()
                .find(|slot| jobs[**slot].version != version_text)
                .map(|slot| jobs[*slot].version.clone())
                .unwrap_or_default();
            return Err(Error::PatchDrift {
                name: name.clone(),
                version: version_text,
                reason: format!(
                    "{name} also resolved to {other} in another environment tree, which the patch would not cover"
                ),
            });
        }

        // one key means one archive, check before anything downloads
        let mut urls = BTreeSet::new();
        for slot in &hits {
            match &jobs[*slot].source {
                index::DownloadSource::Zip { url } | index::DownloadSource::TarGz { url } => {
                    urls.insert(url.clone());
                }
                index::DownloadSource::Workspace { .. } => {
                    return Err(Error::ManifestInvalid(format!(
                        "[patches] key '{key}' names a workspace member; edit its source directly instead of patching"
                    )));
                }
            }
        }
        if urls.len() > 1 {
            let mut urls = urls.into_iter();
            return Err(Error::PatchMultiTarget {
                name,
                version: version_text,
                first: urls.next().unwrap_or_default(),
                second: urls.next().unwrap_or_default(),
            });
        }

        let file = std::path::absolute(project_dir.join(&relative))?;
        let bytes = fs::read(&file).map_err(|_| Error::PatchFileMissing {
            key: key.clone(),
            path: path.clone(),
        })?;
        let patch = JobPatch {
            record: PatchRecord::new(path.clone(), &bytes),
            file,
        };
        for slot in hits {
            jobs[slot].patch = Some(patch.clone());
        }
    }
    Ok(())
}

/** turns a lock entry's patch record back into an applicable patch,
verifying the file still holds the exact bytes the lock was built from. */
fn locked_patch(
    name: &str,
    project_dir: &Path,
    record: Option<PatchRecord>,
) -> Result<Option<JobPatch>, Error> {
    let Some(record) = record else {
        return Ok(None);
    };
    let invalid = |reason: &'static str| Error::PatchLockInvalid {
        name: name.to_string(),
        path: record.path.clone(),
        reason,
    };
    let file = std::path::absolute(project_dir.join(&record.path))?;
    let bytes = fs::read(&file).map_err(|_| invalid("is missing"))?;
    if !record.matches(&bytes) {
        return Err(invalid("has changed since it was locked"));
    }
    Ok(Some(JobPatch { record, file }))
}

/** applies a saved patch onto the freshly extracted tree. the diff carries
git's own a/ b/ headers since the working copy is its own repo, so
`git apply` consumes it at default -p1. failure is a hard error naming the
package, the patch, and git's stderr, never a warning.

the staging dir is git-inited first and the .git dropped after, and that's
load bearing. staging lives inside the user's project, almost always a git
repo, and `git apply` under an ENCLOSING repo resolves patch paths against
that repo's root, silently skipping every path outside the subdir with
exit 0. a throwaway repo in staging pins the resolution to the tree being
patched. */
fn apply_patch(name: &str, patch: &JobPatch, staging: &Path) -> Result<(), Error> {
    if !git::available() {
        return Err(Error::GitMissing);
    }
    let in_staging = staging.to_string_lossy().into_owned();
    git::run(&["-C", &in_staging, "init", "--quiet"]).map_err(|stderr| Error::PatchGitFailed {
        action: "init",
        stderr,
    })?;
    let applied = git::run(&git::verbatim(&[
        "-C",
        &in_staging,
        "apply",
        &patch.file.to_string_lossy(),
    ]))
    .map_err(|stderr| Error::PatchApplyFailed {
        name: name.to_string(),
        patch: patch.record.path.clone(),
        stderr,
    });
    // win or lose, the throwaway repo must not travel into the store
    let _ = fs::remove_dir_all(staging.join(".git"));
    applied
}

/** whether a fresh resolution landed exactly where the lockfile already
stands, the "checked the indices, nothing new" case that skips the
rebuild. environment is compared only when resolution knows it up front.
None means "detect from the archive", and the same version of the same
archive detects the same. */
fn jobs_match_lock(jobs: &[Job], lock: &Lockfile) -> bool {
    if jobs.len() != lock.packages.len() {
        return false;
    }
    /* keyed by (name, context) since one package may be locked once per
    environment root, and each copy has to match its own entry */
    let by_key: HashMap<(&str, Environment), &LockedPackage> = lock
        .packages
        .iter()
        .map(|package| ((package.name.as_str(), package.context()), package))
        .collect();
    jobs.iter().all(|job| {
        by_key
            .get(&(job.name.as_str(), job.context))
            .is_some_and(|locked| {
                job.version == locked.version
                    && job.link == locked.link
                    && job.index_url == locked.index
                    && job.source == locked.source
                    /* redirects shape the nested links on disk, an [overrides]
                    edit that lands on the same versions still has to rebuild */
                    && job.redirects == locked.redirects
                    /* same for a patch, an edited file changes the record's
                    hash, so identical versions still rebuild */
                    && job.patch.as_ref().map(|patch| &patch.record) == locked.patch.as_ref()
                    && job
                        .environment
                        .is_none_or(|environment| environment == locked.environment)
            })
    })
}

/** refuses an install whose contexts map to one output folder. `[config]`
lets users repoint each environment's out dir, and two contexts sharing a
folder would race their `.lpm` stores against each other. */
fn assert_distinct_roots(manifest: &Manifest, jobs: &[Job]) -> Result<(), Error> {
    let contexts: Vec<Environment> = jobs
        .iter()
        .map(|job| job.context)
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
    for (position, first) in contexts.iter().enumerate() {
        for second in &contexts[position + 1..] {
            let out_first = std::path::absolute(manifest.packages_out(*first));
            let out_second = std::path::absolute(manifest.packages_out(*second));
            if let (Ok(out_first), Ok(out_second)) = (out_first, out_second)
                && out_first == out_second
            {
                return Err(Error::PackagesOutCollision {
                    first: *first,
                    second: *second,
                    path: out_first.display().to_string(),
                });
            }
        }
    }
    Ok(())
}

/** the tail every skipped install shares. verify the pinned tools with a
few stats, install is also the repair path for wiped tool storage,
download any that are missing, and say so. */
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

/** the pre-rename output folder of what is now the `roblox` environment,
see [`Environment::Roblox`]. `packages/shared` was the default until the
rename, and once installs stopped writing there nothing would ever clear
it: a whole stale tree of link files, which editors go on autocompleting
and Rojo goes on syncing. so the first install after the rename takes it
away, and every install after that finds nothing to do.

two things keep this from touching anything that isn't lpm's. the folder
must still hold the `.lpm` store an install put there, and no [config] key
may still point an environment at it -- `shared-packages-out` is a
supported spelling, and a project that aims one at this exact path means
it. best-effort: a failure to remove costs a stale folder, not an
install.

said out loud, not swept quietly. the project's own source and Rojo files
may spell `packages/shared` in requires and `$path`s that lpm cannot
rewrite, so the one build this breaks has to arrive with its reason
attached. returns whether it removed anything. */
fn remove_legacy_roblox_out(manifest: &Manifest) -> bool {
    let legacy = Path::new("packages").join("shared");
    if !legacy.join(".lpm").is_dir() {
        return false;
    }
    // unresolvable paths can't be compared, so nothing may be removed on them
    let Ok(absolute) = std::path::absolute(&legacy) else {
        return false;
    };
    let claimed = Environment::ALL.into_iter().any(|environment| {
        std::path::absolute(manifest.packages_out(environment)).is_ok_and(|out| out == absolute)
    });
    if claimed || fs::remove_dir_all(&legacy).is_err() {
        return false;
    }
    println!(
        "Removed the old {} folder; the roblox environment now installs to {}",
        legacy.display(),
        manifest.packages_out(Environment::Roblox).display()
    );
    println!("Update any requires or Rojo paths that still spell it");
    true
}

/** stamps every environment that received packages with the state hash.
best effort, a failed stamp only costs the next run its shortcut. the
manifest and tools inputs are the ones captured before resolution, what
this install actually consumed, while the lock text is read back fresh
since this install just wrote it. */
fn write_state_stamps(
    manifest: &Manifest,
    environments: &BTreeSet<Environment>,
    inputs: &Option<(String, String, String)>,
) {
    let Some((manifest_text, tools_text, patches_text)) = inputs else {
        return;
    };
    let Ok(lock_text) = fs::read_to_string(crate::project::lockfile::LOCKFILE) else {
        return;
    };
    let hash = state_hash(manifest_text, &lock_text, tools_text, patches_text);
    for environment in environments {
        let dir = manifest.packages_out(*environment).join(".lpm");
        if fs::create_dir_all(&dir).is_ok() {
            let _ = fs::write(dir.join(STATE_FILE), &hash);
        }
    }
}

/** downloads, stages, and links every package, reporting progress on `bar`.
caller owns the bar's lifecycle so it gets cleared on errors too.

registry packages fan out over a small thread pool. each worker owns one
job at a time end to end, download, extract, rewrite, parse, under its
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

    /* workspace members link in place on this thread, no network and
    they touch source dirs the user owns. registry jobs queue for the pool */
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

                    /* context suffixed, the same package installing under
                    two roots is two concurrent jobs, and they must not
                    tear down each other's staging */
                    let staging = staging_root.join(format!(
                        "{}_{}",
                        job.name.replace('/', "_"),
                        job.context.dir_name()
                    ));
                    let result = install_one(manifest, &job, &staging, cache, bar);
                    if result.is_err() {
                        failed.store(true, Ordering::Relaxed);
                    }
                    // a closed channel means the run is over, stop quietly
                    if sender.send((slot, job, result)).is_err() {
                        break;
                    }
                }
            });
        }
        drop(sender);

        /* errors are kept by lowest job index, not arrival order. when an
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
                    // transitives have no link, show where they landed instead
                    let shown = job
                        .link
                        .clone()
                        .unwrap_or_else(|| format!(".lpm/{}", job.name.replace('/', "_")));
                    ui::bar_println(
                        bar,
                        &ui::success_line(&format!(
                            "{}@{} → {}/{shown}",
                            job.name, job.version, job.context
                        )),
                    );
                    bar.inc(1);
                    slots[slot] = Some(LockedPackage {
                        name: job.name,
                        version: job.version,
                        environment: extracted.environment,
                        context: (job.context != extracted.environment).then_some(job.context),
                        link: job.link,
                        index: job.index_url,
                        redirects: job.redirects,
                        patch: job.patch.map(|patch| patch.record),
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
    /* every slot must have reported. a hole here is an lpm bug, and
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

/// what a worker learned about one extracted package, links come later.
struct Extracted {
    environment: Environment,
    /// None means no entry point found, warned already at link time
    entry: Option<String>,
    types: Vec<String>,
}

/** one registry package, end to end except the link file. download or
cache hit, extract under `staging`, detect the environment, move into the
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

    /* the tree is staged and pristine here, patch it now so environment
    detection, rojo mirroring, require rewriting, and the nested links all
    see patched files. two contexts of one package run this twice and both
    copies get the same patch */
    if let Some(patch) = &job.patch {
        apply_patch(&job.name, patch, staging)?;
    }

    /* indices usually know the environment, otherwise ask the files,
    lpm.toml then pesde.toml then wally.toml */
    let environment = match job.environment {
        Some(environment) => environment,
        None => package::environment(staging)
            .ok_or_else(|| Error::UnknownPackageEnvironment(job.name.clone()))?,
    };

    /* real contents live under <out>/.lpm/<scope>_<name>/ where <out> is
    the CONTEXT's folder, not the package's own environment. each root is
    self contained, so a server package's roblox deps stay under server.
    workers race to create the same .lpm parent, create_dir_all treats
    existing as success, and Windows occasionally answers concurrent
    creation or a fresh rename with a transient denial worth one retry */
    let folder = job.name.replace('/', "_");
    let out = manifest.packages_out(job.context);
    let storage = out.join(".lpm").join(&folder);
    with_retry(|| fs::create_dir_all(storage.parent().expect("storage dir has a parent")))?;
    if storage.exists() {
        with_retry(|| fs::remove_dir_all(&storage))?;
    }
    with_retry(|| fs::rename(staging, &storage))?;

    /* a project file the package ships would mount it under its own
    name and tree, which our link files and the package's own relative
    requires don't spell. make it mirror the disk instead. after
    entry_point, so the entry is read from what the package shipped */
    let entry = package::entry_point(&storage);
    rojo::mirror_disk_layout(&storage, &mut bar_warn(bar));

    let (entry, types) = match entry {
        Some(entry) => {
            /* packages from any index can talk roblox instance paths
            like require(script.Parent.X), wally stuff especially but ports
            published to pesde or our index too. rewrite what we can into
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

/** the consumer level `<out>/<link>.luau` for one extracted package,
runs on the main thread once a worker reports in. only direct
dependencies (link = Some) get one, transitives are reachable through
their dependents' nested links alone. */
fn write_registry_link(
    manifest: &Manifest,
    job: &Job,
    extracted: &Extracted,
    bar: &ProgressBar,
) -> Result<(), Error> {
    match (&job.link, &extracted.entry) {
        (Some(link), Some(entry)) => {
            let out = manifest.packages_out(job.context);
            let folder = job.name.replace('/', "_");
            let link_path = out.join(format!("{link}.luau"));
            fs::write(
                &link_path,
                package::link_contents(&folder, entry, &extracted.types),
            )?;
        }
        // entry-less packages warn even without a link to write, nothing
        // else warns when the only dependent links in place
        (_, None) => warn_no_entry(&job.name, bar),
        (None, Some(_)) => {}
    }
    Ok(())
}

/** workspace members link in place, no download, no copy under .lpm/,
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
    let out = manifest.packages_out(job.context);
    fs::create_dir_all(&out)?;

    match (&job.link, package::entry_point(member_dir)) {
        (Some(link), Some(entry)) => {
            let mut require = workspace::relative_path(&out, member_dir);
            if !entry.is_empty() {
                require = format!("{require}/{entry}");
            }
            if !require.starts_with("..") {
                require = format!("./{require}");
            }
            let types = link_types(member_dir, &entry, &job.name, &mut bar_warn(bar));
            let link_path = out.join(format!("{link}.luau"));
            fs::write(&link_path, package::link_contents_at(&require, &types))?;
        }
        (_, None) => warn_no_entry(&job.name, bar),
        // a transitively pulled member, link targets exist, no link written
        (None, Some(_)) => {}
    }

    let shown = job
        .link
        .clone()
        .unwrap_or_else(|| job.name.replace('/', "_"));
    ui::bar_println(
        bar,
        &ui::success_line(&format!(
            "{}@{} → {}/{shown} (workspace)",
            job.name, job.version, job.context
        )),
    );
    Ok(LockedPackage {
        name: job.name,
        version: job.version,
        environment,
        context: (job.context != environment).then_some(job.context),
        link: job.link,
        index: job.index_url,
        redirects: job.redirects,
        // members are the user's own source, attach_patches refused them
        patch: None,
        source: job.source,
    })
}

/** one installed package, as the nested-link pass sees it. */
struct StoredPackage {
    /// lowercased "scope/name", the form dependency declarations resolve by
    name: String,
    /// where the contents live, <context out>/.lpm/<scope>_<name> or a member's own directory
    storage: PathBuf,
    /// the package's own environment, names the nested link folder dependents use
    environment: Environment,
    /// the environment root it installed under, lookups stay within one context
    context: Environment,
    /// a workspace member, fine to link to, never written into
    in_place: bool,
    /** [overrides] rewritten edges, declared alias -> replacement package
    name. the manifest on disk still declares the original, so link
    generation must consult this before trusting what it reads back. */
    redirects: BTreeMap<String, String>,
}

/** link files inside a stored package for its own declared dependencies,
at `<storage>/packages/<env>/<alias>.luau`. that folder name is the default
layout, literally. published code was compiled against it, see Chief's
build, so a consumer's `[config]` output customization deliberately doesn't
apply inside packages. a package published from a project that customized
its own output dirs would want those names instead, nothing has needed that
yet. each link requires the dependency's store entry directly.

nothing here is fatal. the install has already downloaded and extracted
everything, so a package that can't be linked warns and is skipped rather
than taking the whole run, and the lockfile write that follows, with it. */
fn link_nested_dependencies(packages: &[StoredPackage], warn: &mut impl FnMut(String)) {
    /* keyed by (name, context), a dependent only ever links dependencies
    installed in its own tree, which is what keeps every require inside
    one output root */
    let by_name: HashMap<(&str, Environment), &StoredPackage> = packages
        .iter()
        .map(|package| ((package.name.as_str(), package.context), package))
        .collect();
    /* exported types depend on the dependency AND its context, the same
    name can resolve to different targets, different archives, per tree */
    let mut types_cache: HashMap<(String, Environment), Vec<String>> = HashMap::new();

    for package in packages.iter().filter(|package| !package.in_place) {
        /* alias -> environment folder, for the escape require rewrite
        below. only aliases that actually linked make it in */
        let mut escape_aliases: BTreeMap<String, String> = BTreeMap::new();
        for (alias, declared) in package::declared_dependencies(&package.storage) {
            /* the shipped manifest declares the ORIGINAL package. an
            [overrides] redirect on this edge means the alias must link to
            the replacement instead */
            let dependency = package
                .redirects
                .get(&alias)
                .map_or(declared, |replacement| replacement.clone());
            /* aliases are TOML keys from a downloaded manifest, and TOML
            keys can be quoted anything. a "../.." or absolute one would put
            the link outside the package or outside the project */
            if !is_plain_file_name(&alias) {
                warn(format!(
                    "warning: {} declares dependency {dependency} under the unusable alias '{alias}'; no nested link generated",
                    package.name
                ));
                continue;
            }
            let Some(dep) = by_name.get(&(dependency.as_str(), package.context)) else {
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
                .join(rojo::PACKAGES_DIR)
                .join(dep.environment.dir_name());
            if let Err(error) = fs::create_dir_all(&link_dir) {
                warn(format!(
                    "warning: could not create {} ({error}); no nested link generated for {dependency}",
                    link_dir.display()
                ));
                continue;
            }

            /* relative_path is lexical, so both sides go through
            std::path::absolute first. that drops the "." component a
            `[config]` dir like "./packages/roblox" would otherwise
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
                .entry((dependency.clone(), package.context))
                .or_insert_with(|| {
                    /* install_packages already parsed and complained about
                    every stored entry point, so no warn sink here, it would
                    just say the same thing twice */
                    package::entry_source(&dep.storage, &entry)
                        .and_then(|path| fs::read_to_string(path).ok())
                        .and_then(|source| package::exported_types(&source))
                        .unwrap_or_default()
                })
                .clone();
            let link_path = link_dir.join(format!("{alias}.luau"));
            match fs::write(&link_path, package::link_contents_at(&require, &types)) {
                /* only now is the alias safe to retarget escape requires
                at. rewriting toward a link that failed to write would
                turn a maybe working chain into a certainly broken one */
                Ok(()) => {
                    escape_aliases.insert(alias.clone(), dep.environment.dir_name().to_string());
                }
                Err(error) => warn(format!(
                    "warning: could not write {} ({error})",
                    link_path.display()
                )),
            }
        }

        /* an alias only lands here once its link file is really written, so
        an empty map means this package got none: nothing to retarget, and
        nothing of ours in any `packages/` folder it might ship itself */
        if escape_aliases.is_empty() {
            continue;
        }

        /* second half of the instance require rewrite. escape chains
        couldn't map on the workers since dependency environments weren't
        known yet, now the nested links are decided so retarget them at
        packages/<env>/<alias> */
        if let Some(entry) = package::entry_point(&package.storage)
            && let Err(error) =
                requires::rewrite_escape_requires(&package.storage, &entry, &escape_aliases)
        {
            warn(format!(
                "warning: could not rewrite requires in {} ({error})",
                package.name
            ));
        }

        /* the links exist now, so a project file this package ships can
        finally mount the folder holding them. it wasn't there to mount when
        extraction rewrote that file, and Rojo syncs the tree, not the disk */
        rojo::mount_nested_packages(&package.storage, warn);
    }
}

/// one ordinary path segment, no separators, no drive letter, not `.` or `..`.
fn is_plain_file_name(alias: &str) -> bool {
    !alias.is_empty()
        && alias != "."
        && alias != ".."
        && !alias.contains(['/', '\\', ':'])
        && !Path::new(alias).is_absolute()
}

/** exported types have to be restated in the link file to survive the
wrapper. an unparseable entry point still links, just without its types. */
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

        /* another toolchain manager's shim earlier in PATH, aftman or
        rokit, would run instead of ours and report its own errors. surface
        that or the tool looks broken for no visible reason */
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
        let base = state_hash("manifest", "lock", "tools", "patches");
        assert!(base.starts_with("lpm-state-v2:"));
        assert_eq!(base, state_hash("manifest", "lock", "tools", "patches"));

        // each input moves the hash, and boundaries are unambiguous
        assert_ne!(base, state_hash("manifest2", "lock", "tools", "patches"));
        assert_ne!(base, state_hash("manifest", "lock2", "tools", "patches"));
        assert_ne!(base, state_hash("manifest", "lock", "tools2", "patches"));
        assert_ne!(base, state_hash("manifest", "lock", "tools", "patches2"));
        assert_ne!(
            state_hash("ab", "c", "", ""),
            state_hash("a", "bc", "", ""),
            "input boundaries must be part of the hash"
        );
    }

    #[test]
    fn patch_state_text_reads_the_files_in_key_order() {
        let base = std::env::temp_dir().join("lpm-test-patch-state");
        let _ = fs::remove_dir_all(&base);
        fs::create_dir_all(base.join("patches")).unwrap();
        fs::write(base.join("patches/b.patch"), "bbb").unwrap();

        /* paths in the manifest are project relative. this test runs from
        the crate dir, so spell them absolute to stay hermetic */
        let patch = |file: &str| {
            base.join("patches")
                .join(file)
                .to_string_lossy()
                .replace('\\', "/")
        };
        let manifest_text = format!(
            "[package]\nname = \"acme/x\"\nversion = \"0.1.0\"\n\n[patches]\n\
             \"acme/a@1.0.0\" = \"{}\"\n\"acme/b@1.0.0\" = \"{}\"\n",
            patch("a.patch"),
            patch("b.patch"),
        );

        // key order, content included, missing file a distinct marker
        let text = patches_state_text(&manifest_text);
        assert!(text.contains("acme/a@1.0.0\0<missing>\0"), "{text:?}");
        assert!(text.contains("acme/b@1.0.0\0bbb\0"), "{text:?}");

        // editing a patch file with the manifest untouched moves the hash
        fs::write(base.join("patches/b.patch"), "ccc").unwrap();
        let edited = patches_state_text(&manifest_text);
        assert_ne!(text, edited);
        assert_ne!(
            state_hash("m", "l", "t", &text),
            state_hash("m", "l", "t", &edited)
        );

        // no [patches] means an empty part, not a missing one
        assert_eq!(
            patches_state_text("[package]\nname = \"acme/x\"\nversion = \"0.1.0\"\n"),
            ""
        );

        let _ = fs::remove_dir_all(&base);
    }

    fn patch_job(name: &str, version: &str, url: &str, context: Environment) -> Job {
        Job {
            name: name.to_string(),
            version: version.to_string(),
            environment: None,
            context,
            source: index::DownloadSource::TarGz {
                url: url.to_string(),
            },
            index_url: "https://example.com/index".to_string(),
            link: None,
            redirects: BTreeMap::new(),
            patch: None,
        }
    }

    #[test]
    fn patches_attach_before_anything_downloads_or_not_at_all() {
        let base = std::env::temp_dir().join("lpm-test-attach-patches");
        let _ = fs::remove_dir_all(&base);
        fs::create_dir_all(base.join("patches")).unwrap();
        fs::write(base.join("patches/traits.patch"), "diff").unwrap();

        let manifest = |key: &str| -> Manifest {
            toml::from_str(&format!(
                "[package]\nname = \"acme/app\"\nversion = \"0.1.0\"\n\n\
                 [patches]\n\"{key}\" = \"patches/traits.patch\"\n"
            ))
            .unwrap()
        };

        /* the happy path, one key, two contexts of the same archive, both
        copies get the same patch */
        let mut jobs = vec![
            patch_job(
                "chief/traits",
                "0.2.0",
                "https://cdn.example/t/0.2.0.tar.gz",
                Environment::Roblox,
            ),
            patch_job(
                "chief/traits",
                "0.2.0",
                "https://cdn.example/t/0.2.0.tar.gz",
                Environment::Server,
            ),
        ];
        attach_patches(&manifest("chief/traits@0.2.0"), &base, &mut jobs).unwrap();
        assert!(jobs.iter().all(|job| job.patch.is_some()));
        let records: Vec<_> = jobs
            .iter()
            .map(|job| job.patch.as_ref().unwrap().record.clone())
            .collect();
        assert_eq!(records[0], records[1]);

        // version drift is an error, not a skip
        let mut jobs = vec![patch_job(
            "chief/traits",
            "0.3.0",
            "https://cdn.example/t/0.3.0.tar.gz",
            Environment::Roblox,
        )];
        let drift = attach_patches(&manifest("chief/traits@0.2.0"), &base, &mut jobs);
        assert!(
            matches!(&drift, Err(Error::PatchDrift { reason, .. }) if reason.contains("resolved to 0.3.0")),
            "{drift:?}"
        );

        // a package that's not in the graph at all is the other drift flavor
        let mut jobs = vec![patch_job(
            "acme/other",
            "1.0.0",
            "https://cdn.example/o/1.0.0.tar.gz",
            Environment::Roblox,
        )];
        let missing = attach_patches(&manifest("chief/traits@0.2.0"), &base, &mut jobs);
        assert!(
            matches!(&missing, Err(Error::PatchDrift { reason, .. }) if reason.contains("not a dependency")),
            "{missing:?}"
        );

        /* same name@version resolving to two different archives, a
        multi target pesde package under two roots, is refused */
        let mut jobs = vec![
            patch_job(
                "chief/traits",
                "0.2.0",
                "https://cdn.example/t/0.2.0/shared.tar.gz",
                Environment::Roblox,
            ),
            patch_job(
                "chief/traits",
                "0.2.0",
                "https://cdn.example/t/0.2.0/server.tar.gz",
                Environment::Server,
            ),
        ];
        assert!(matches!(
            attach_patches(&manifest("chief/traits@0.2.0"), &base, &mut jobs),
            Err(Error::PatchMultiTarget { .. })
        ));

        // a missing patch file fails cleanly too
        let mut jobs = vec![patch_job(
            "chief/traits",
            "0.2.0",
            "https://cdn.example/t/0.2.0.tar.gz",
            Environment::Roblox,
        )];
        let mut gone = manifest("chief/traits@0.2.0");
        gone.patches.insert(
            "chief/traits@0.2.0".to_string(),
            "patches/nope.patch".to_string(),
        );
        assert!(matches!(
            attach_patches(&gone, &base, &mut jobs),
            Err(Error::PatchFileMissing { .. })
        ));

        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn one_name_at_two_versions_across_contexts_is_drift_not_a_half_patch() {
        let base = std::env::temp_dir().join("lpm-test-partial-patch");
        let _ = fs::remove_dir_all(&base);
        fs::create_dir_all(base.join("patches")).unwrap();
        fs::write(base.join("patches/traits.patch"), "diff").unwrap();
        let manifest: Manifest = toml::from_str(
            "[package]\nname = \"acme/app\"\nversion = \"0.1.0\"\n\n\
             [patches]\n\"chief/traits@0.2.0\" = \"patches/traits.patch\"\n",
        )
        .unwrap();

        /* contexts resolve independently, roblox landed on 0.2.0, server on
        0.3.0. patching only the 0.2.0 copy would be a silent half skip */
        let mut jobs = vec![
            patch_job(
                "chief/traits",
                "0.2.0",
                "https://cdn.example/t/0.2.0.tar.gz",
                Environment::Roblox,
            ),
            patch_job(
                "chief/traits",
                "0.3.0",
                "https://cdn.example/t/0.3.0.tar.gz",
                Environment::Server,
            ),
        ];
        let partial = attach_patches(&manifest, &base, &mut jobs);
        assert!(
            matches!(&partial, Err(Error::PatchDrift { reason, .. })
                if reason.contains("also resolved to 0.3.0")),
            "{partial:?}"
        );

        let _ = fs::remove_dir_all(&base);
    }

    /** regression. staging lives inside the user's project, normally a
    git repo, and `git apply` under an enclosing repo silently skips
    paths outside its subdir with exit 0. the apply must isolate itself
    or patches no-op in every real project. */
    #[test]
    fn patches_apply_inside_an_enclosing_git_repo() {
        if !git::available() {
            return;
        }
        let base = std::env::temp_dir().join("lpm-test-apply-enclosed");
        let _ = fs::remove_dir_all(&base);
        let staging = base.join("project/.lpm-staging/pkg");
        fs::create_dir_all(staging.join("src")).unwrap();
        // the trap, an enclosing repo above the staging dir
        git::run(&[
            "-C",
            &base.join("project").to_string_lossy(),
            "init",
            "--quiet",
        ])
        .unwrap();
        fs::write(staging.join("src/init.luau"), "return 1\n").unwrap();

        let patch_file = base.join("the.patch");
        fs::write(
            &patch_file,
            "diff --git a/src/init.luau b/src/init.luau\n\
             --- a/src/init.luau\n\
             +++ b/src/init.luau\n\
             @@ -1 +1 @@\n\
             -return 1\n\
             +return 2 -- patched\n",
        )
        .unwrap();
        let patch = JobPatch {
            record: PatchRecord::new("patches/x.patch".to_string(), b"diff"),
            file: patch_file,
        };

        apply_patch("acme/pkg", &patch, &staging).unwrap();
        assert_eq!(
            fs::read_to_string(staging.join("src/init.luau")).unwrap(),
            "return 2 -- patched\n"
        );
        // the throwaway repo never travels into the store
        assert!(!staging.join(".git").exists());

        // and a patch that doesn't fit is a hard error, not a warning
        let bad = JobPatch {
            record: PatchRecord::new("patches/x.patch".to_string(), b"diff"),
            file: base.join("the.patch"),
        };
        fs::write(staging.join("src/init.luau"), "something else\n").unwrap();
        assert!(matches!(
            apply_patch("acme/pkg", &bad, &staging),
            Err(Error::PatchApplyFailed { .. })
        ));

        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn locked_patches_verify_the_recorded_bytes() {
        let base = std::env::temp_dir().join("lpm-test-locked-patch");
        let _ = fs::remove_dir_all(&base);
        fs::create_dir_all(&base).unwrap();
        let file = base.join("x.patch");
        fs::write(&file, "diff").unwrap();

        // matching bytes replay fine
        let record = PatchRecord::new("x.patch".to_string(), b"diff");
        assert!(
            locked_patch("acme/x", &base, Some(record.clone()))
                .unwrap()
                .is_some()
        );
        assert!(locked_patch("acme/x", &base, None).unwrap().is_none());

        // edited since it was locked
        fs::write(&file, "different").unwrap();
        assert!(matches!(
            locked_patch("acme/x", &base, Some(record.clone())),
            Err(Error::PatchLockInvalid { reason, .. }) if reason.contains("changed")
        ));

        // gone entirely
        fs::remove_file(&file).unwrap();
        assert!(matches!(
            locked_patch("acme/x", &base, Some(record)),
            Err(Error::PatchLockInvalid { reason, .. }) if reason.contains("missing")
        ));

        let _ = fs::remove_dir_all(&base);
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
            context: Environment::Roblox,
            source: index::DownloadSource::Zip {
                url: url.to_string(),
            },
            index_url: "https://example.com/index".to_string(),
            link: Some("thing".to_string()),
            redirects: BTreeMap::new(),
            patch: None,
        };
        let locked = |context: Option<Environment>| LockedPackage {
            name: "acme/thing".to_string(),
            version: "1.0.0".to_string(),
            environment: Environment::Roblox,
            context,
            link: Some("thing".to_string()),
            index: "https://example.com/index".to_string(),
            redirects: BTreeMap::new(),
            patch: None,
            source: index::DownloadSource::Zip {
                url: "https://example.com/thing/1.0.0".to_string(),
            },
        };
        let lock = Lockfile::new(vec![locked(None)]);

        // identical resolution, rebuild skippable, env None tolerated
        assert!(jobs_match_lock(
            &[job("1.0.0", "https://example.com/thing/1.0.0")],
            &lock
        ));
        // a new version is `^` doing its job, rebuild
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
        let mut roblox = job("1.0.0", "https://example.com/thing/1.0.0");
        roblox.environment = Some(Environment::Roblox);
        assert!(jobs_match_lock(&[roblox], &lock));

        // a moved context or a dropped link is a layout change, rebuild
        let mut moved = job("1.0.0", "https://example.com/thing/1.0.0");
        moved.context = Environment::Server;
        assert!(!jobs_match_lock(&[moved], &lock));
        let mut unlinked = job("1.0.0", "https://example.com/thing/1.0.0");
        unlinked.link = None;
        assert!(!jobs_match_lock(&[unlinked], &lock));

        /* one package under two contexts matches a two-entry lock. keyed
        by name alone the entries would collapse and never match */
        let two_contexts = Lockfile::new(vec![locked(None), locked(Some(Environment::Server))]);
        let mut server_copy = job("1.0.0", "https://example.com/thing/1.0.0");
        server_copy.context = Environment::Server;
        assert!(jobs_match_lock(
            &[job("1.0.0", "https://example.com/thing/1.0.0"), server_copy],
            &two_contexts
        ));

        /* an identical resolution with a different, new, or edited
        patch is a mismatch, patch bytes shape what lands on disk */
        let with_patch = |bytes: &[u8]| {
            let mut patched = job("1.0.0", "https://example.com/thing/1.0.0");
            patched.patch = Some(JobPatch {
                record: PatchRecord::new("patches/thing.patch".to_string(), bytes),
                file: PathBuf::from("patches/thing.patch"),
            });
            patched
        };
        assert!(!jobs_match_lock(&[with_patch(b"diff")], &lock));
        let mut locked_patched = locked(None);
        locked_patched.patch = Some(PatchRecord::new("patches/thing.patch".to_string(), b"diff"));
        let patched_lock = Lockfile::new(vec![locked_patched]);
        assert!(jobs_match_lock(&[with_patch(b"diff")], &patched_lock));
        assert!(!jobs_match_lock(&[with_patch(b"edited")], &patched_lock));
        assert!(!jobs_match_lock(
            &[job("1.0.0", "https://example.com/thing/1.0.0")],
            &patched_lock
        ));
    }

    #[test]
    fn top_level_links_are_for_direct_dependencies_only() {
        let base = std::env::temp_dir().join("lpm-test-top-links");
        let _ = fs::remove_dir_all(&base);
        let out = base.join("packages/roblox");
        fs::create_dir_all(&out).unwrap();
        let manifest: Manifest = toml::from_str(&format!(
            "[package]\nname = \"acme/x\"\nversion = \"0.1.0\"\n\n[config]\n\
             roblox-packages-out = \"{}/packages/roblox\"\n",
            base.to_string_lossy().replace('\\', "/")
        ))
        .unwrap();
        let job = |link: Option<&str>| Job {
            name: "acme/thing".to_string(),
            version: "1.0.0".to_string(),
            environment: Some(Environment::Roblox),
            context: Environment::Roblox,
            source: index::DownloadSource::Zip {
                url: "https://example.com/thing/1.0.0".to_string(),
            },
            index_url: "https://example.com/index".to_string(),
            link: link.map(str::to_string),
            redirects: BTreeMap::new(),
            patch: None,
        };
        let extracted = |entry: Option<&str>| Extracted {
            environment: Environment::Roblox,
            entry: entry.map(str::to_string),
            types: Vec::new(),
        };
        let bar = ProgressBar::hidden();

        // a direct dependency gets its consumer-level link
        write_registry_link(&manifest, &job(Some("thing")), &extracted(Some("")), &bar).unwrap();
        let link = fs::read_to_string(out.join("thing.luau")).unwrap();
        assert!(link.contains("./.lpm/acme_thing"), "{link}");

        // a transitive writes nothing at the top level
        write_registry_link(&manifest, &job(None), &extracted(Some("")), &bar).unwrap();
        // an entry-less package, direct or not, writes nothing either
        write_registry_link(&manifest, &job(Some("broken")), &extracted(None), &bar).unwrap();
        let files: Vec<String> = fs::read_dir(&out)
            .unwrap()
            .flatten()
            .map(|entry| entry.file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(files, ["thing.luau"]);

        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn colliding_output_roots_are_refused() {
        let manifest: Manifest = toml::from_str(
            "[package]\nname = \"acme/x\"\nversion = \"0.1.0\"\n\n[config]\n\
             roblox-packages-out = \"packages/everything\"\n\
             server-packages-out = \"packages/everything\"\n",
        )
        .unwrap();
        let job = |context: Environment| Job {
            name: "acme/thing".to_string(),
            version: "1.0.0".to_string(),
            environment: None,
            context,
            source: index::DownloadSource::Zip {
                url: "https://example.com/thing/1.0.0".to_string(),
            },
            index_url: "https://example.com/index".to_string(),
            link: None,
            redirects: BTreeMap::new(),
            patch: None,
        };

        // one context, no collision possible, whatever [config] says
        assert!(assert_distinct_roots(&manifest, &[job(Environment::Roblox)]).is_ok());
        // two contexts mapped onto one folder must refuse the install
        assert!(matches!(
            assert_distinct_roots(
                &manifest,
                &[job(Environment::Roblox), job(Environment::Server)]
            ),
            Err(Error::PackagesOutCollision { .. })
        ));
        // distinct folders stay fine
        assert!(
            assert_distinct_roots(
                &manifest,
                &[job(Environment::Roblox), job(Environment::Lune)]
            )
            .is_ok()
        );
    }

    /** the one destructive path this rename added. it must take the store
    an older lpm left behind, and nothing else: not a folder of the user's
    own that happens to sit there, and not the output folder of a project
    that deliberately still points an environment at that path. */
    #[test]
    fn the_legacy_output_folder_goes_only_when_it_is_lpm_s() {
        /* a [package] table neither case cares about, so the fixture stays
        readable to any lpm: what this varies is [config] */
        const PACKAGE: &str = "[package]\nname = \"acme/app\"\nversion = \"0.1.0\"\n";
        let base = std::env::temp_dir().join("lpm-test-legacy-out");
        let plain: Manifest = toml::from_str(PACKAGE).unwrap();
        let claimed: Manifest = toml::from_str(&format!(
            "{PACKAGE}\n[config]\nshared-packages-out = \"packages/shared\"\n"
        ))
        .unwrap();

        let setup = |store: bool| {
            let _ = fs::remove_dir_all(&base);
            fs::create_dir_all(base.join("packages/shared")).unwrap();
            if store {
                fs::create_dir_all(base.join("packages/shared/.lpm/acme_thing")).unwrap();
            }
            fs::write(base.join("packages/shared/Thing.luau"), "return nil\n").unwrap();
        };
        // the function reads relative paths, so it has to run from the project
        let in_base = |manifest: &Manifest| {
            crate::project::workspace::in_dir(&base, || Ok(remove_legacy_roblox_out(manifest)))
                .unwrap()
        };

        // an lpm store from before the rename goes, and says so
        setup(true);
        assert!(in_base(&plain));
        assert!(!base.join("packages/shared").exists());
        // ...and a second install finds nothing left to do
        assert!(!in_base(&plain));

        /* a folder of the user's own that merely shares the name is not an
        install output: no `.lpm`, no removal */
        setup(false);
        assert!(!in_base(&plain));
        assert!(base.join("packages/shared/Thing.luau").exists());

        // and a project that still aims an environment there keeps it
        setup(true);
        assert!(!in_base(&claimed));
        assert!(base.join("packages/shared/.lpm").exists());

        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn state_stamps_land_in_each_environment() {
        let base = std::env::temp_dir().join("lpm-test-state-stamps");
        let _ = fs::remove_dir_all(&base);
        let manifest: Manifest = toml::from_str(&format!(
            "[package]\nname = \"acme/x\"\nversion = \"0.1.0\"\n\n[config]\n\
             roblox-packages-out = \"{0}/packages/roblox\"\n\
             luau-packages-out = \"{0}/packages/luau\"\n",
            base.to_string_lossy().replace('\\', "/")
        ))
        .unwrap();

        /* the lock half of the hash reads from the cwd, the repo root under
        cargo test. the stamps just have to be consistent with whatever it
        hashes to, and absent when there's no lockfile at all */
        let inputs = Some((
            "manifest text".to_string(),
            "<absent>".to_string(),
            String::new(),
        ));
        let environments: BTreeSet<Environment> = [Environment::Roblox, Environment::Luau]
            .into_iter()
            .collect();
        write_state_stamps(&manifest, &environments, &inputs);

        let Ok(lock_text) = fs::read_to_string(crate::project::lockfile::LOCKFILE) else {
            assert!(!base.join("packages/roblox/.lpm").join(STATE_FILE).exists());
            let _ = fs::remove_dir_all(&base);
            return;
        };
        let expected = state_hash("manifest text", &lock_text, "<absent>", "");
        for environment in ["roblox", "luau"] {
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
        stored_in(name, storage, environment, environment)
    }

    fn stored_in(
        name: &str,
        storage: PathBuf,
        environment: Environment,
        context: Environment,
    ) -> StoredPackage {
        StoredPackage {
            // production lowercases here, mirror that so the tests exercise it
            name: name.to_lowercase(),
            storage,
            environment,
            context,
            in_place: false,
            redirects: BTreeMap::new(),
        }
    }

    #[test]
    fn mounts_the_packages_folder_of_a_package_shipping_a_project_file() {
        let base = std::env::temp_dir().join("lpm-test-nested-links-rojo");
        let _ = fs::remove_dir_all(&base);
        let roblox = base.join("packages/roblox");

        /* both ship a project file, as wally packages built from Rojo
        projects do. lyra declares its dependency the way a server realm
        package does, under [server-dependencies] */
        let promise = roblox.join(".lpm/evaera_promise");
        write(
            &promise,
            "default.project.json",
            r#"{"name": "promise", "tree": {"$path": "lib"}}"#,
        );
        write(&promise, "lib/init.luau", "return {}\n");

        let lyra = roblox.join(".lpm/lyra_lyra");
        write(
            &lyra,
            "wally.toml",
            "[package]\nrealm = \"server\"\n\n\
             [server-dependencies]\nPromise = \"evaera/promise@4.0.0\"\n",
        );
        write(
            &lyra,
            "default.project.json",
            r#"{"name": "lyra", "tree": {"$path": "lib"}}"#,
        );
        write(
            &lyra,
            "lib/init.luau",
            "local Promise = require(script.Parent.Promise)\nreturn Promise\n",
        );

        // extraction mirrors the disk, then the second pass links
        let mut warnings = Vec::new();
        for package in [&promise, &lyra] {
            rojo::mirror_disk_layout(package, &mut |message| warnings.push(message));
        }
        let packages = [
            stored("evaera/promise", promise.clone(), Environment::Roblox),
            stored("lyra/lyra", lyra.clone(), Environment::Roblox),
        ];
        link_nested_dependencies(&packages, &mut |message| warnings.push(message));
        assert_eq!(warnings, Vec::<String>::new());

        // the link lands and the instance require is retargeted at it...
        assert!(lyra.join("packages/roblox/Promise.luau").exists());
        assert_eq!(
            fs::read_to_string(lyra.join("lib/init.luau")).unwrap(),
            "local Promise = require(\"./packages/roblox/Promise\")\nreturn Promise\n"
        );

        let project = |dir: &Path| -> serde_json::Value {
            serde_json::from_str(&fs::read_to_string(dir.join("default.project.json")).unwrap())
                .unwrap()
        };
        /* ...and the tree finally mounts the folder that require reaches
        for, alongside what mirror_disk_layout re-nested */
        let lyra_project = project(&lyra);
        assert_eq!(lyra_project["tree"]["packages"]["$path"], "packages");
        assert_eq!(lyra_project["tree"]["lib"]["$path"], "lib");

        // the dependency linked nothing, so no missing path enters its tree
        assert!(project(&promise)["tree"]["packages"].is_null());

        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn a_packages_folder_we_did_not_write_is_left_unmounted() {
        /* the folder being there isn't the gate, having linked into it is.
        a package that published a `packages` folder of its own keeps the
        tree it published, we don't graft its contents into the sync */
        let base = std::env::temp_dir().join("lpm-test-nested-links-vendored");
        let _ = fs::remove_dir_all(&base);
        let vendored = base.join("packages/roblox/.lpm/acme_vendored");

        // no manifest, so nothing is declared and nothing links
        let project = r#"{"name": "acme_vendored", "tree": {"$className": "Folder",
            "src": {"$path": "src"}}}"#;
        write(&vendored, "default.project.json", project);
        write(&vendored, "src/init.luau", "return {}\n");
        write(&vendored, "packages/vendor.luau", "return nil\n");

        let mut warnings = Vec::new();
        link_nested_dependencies(
            &[stored(
                "acme/vendored",
                vendored.clone(),
                Environment::Roblox,
            )],
            &mut |message| warnings.push(message),
        );

        assert_eq!(warnings, Vec::<String>::new());
        assert_eq!(
            fs::read_to_string(vendored.join("default.project.json")).unwrap(),
            project
        );

        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn a_mounted_package_still_names_its_entry_to_later_dependents() {
        /* the mount adds a second child to the tree, and `entry_point`
        refuses a tree whose root names more than one thing. a dependency
        that linked first is read back by its own dependent later in this
        same loop, so the mount must not change what it answers.

        the slice is ordered the way the resolver's BTreeMap hands it over,
        alphabetically, which is what puts the dependency first here */
        let base = std::env::temp_dir().join("lpm-test-nested-links-mounted-dep");
        let _ = fs::remove_dir_all(&base);
        let roblox = base.join("packages/roblox");

        let promise = roblox.join(".lpm/evaera_promise");
        write(&promise, "lib/init.luau", "return {}\n");

        /* roblox-ts shaped: `out` is an entry none of the conventional
        fallbacks can recover, so a broken tree read shows up as a miss */
        let tslib = roblox.join(".lpm/a_tslib");
        write(
            &tslib,
            "wally.toml",
            "[dependencies]\nPromise = \"evaera/promise@4.0.0\"\n",
        );
        write(
            &tslib,
            "default.project.json",
            r#"{"name": "tslib", "tree": {"$path": "out"}}"#,
        );
        write(&tslib, "out/init.lua", "return {}\n");

        let app = roblox.join(".lpm/z_app");
        write(
            &app,
            "wally.toml",
            "[dependencies]\nTsLib = \"a/tslib@1.0.0\"\n",
        );
        write(&app, "src/init.luau", "return {}\n");

        let mut warnings = Vec::new();
        for package in [&promise, &tslib, &app] {
            rojo::mirror_disk_layout(package, &mut |message| warnings.push(message));
        }
        let packages = [
            stored("a/tslib", tslib.clone(), Environment::Roblox),
            stored("evaera/promise", promise, Environment::Roblox),
            stored("z/app", app.clone(), Environment::Roblox),
        ];
        link_nested_dependencies(&packages, &mut |message| warnings.push(message));
        assert_eq!(warnings, Vec::<String>::new());

        // tslib linked first, so it was mounted before app read it back
        let project: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(tslib.join("default.project.json")).unwrap())
                .unwrap();
        assert_eq!(project["tree"]["packages"]["$path"], "packages");

        // and app still linked against tslib's real entry, not a fallback
        assert_eq!(
            fs::read_to_string(app.join("packages/roblox/TsLib.luau")).unwrap(),
            "return require(\"../../../a_tslib/out\")\n"
        );

        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn writes_nested_links_for_stored_dependencies() {
        let base = std::env::temp_dir().join("lpm-test-nested-links");
        let _ = fs::remove_dir_all(&base);
        let roblox = base.join("packages/roblox");

        /* chief shaped fixture. `lifecycles` depends on `core`, which
        exports a type, and on `util`, a luau environment package pulled
        into the roblox tree. same context, so its storage sits in the
        roblox root while its link folder keeps the luau name */
        let core = roblox.join(".lpm/acme_core");
        write(&core, "lpm.toml", "[target]\nmain = \"out/lpm\"\n");
        write(
            &core,
            "out/lpm/init.luau",
            "export type Entry = { id: number }\nreturn {}\n",
        );
        let util = roblox.join(".lpm/acme_util");
        write(&util, "init.luau", "return {}\n");
        let lifecycles = roblox.join(".lpm/acme_lifecycles");
        write(
            &lifecycles,
            "lpm.toml",
            "[dependencies]\ncore = { name = \"acme/core\", version = \"^\" }\n\
             util = { name = \"acme/util\", version = \"^\" }\n",
        );
        write(&lifecycles, "out/lpm/init.luau", "return {}\n");

        let packages = [
            stored("Acme/Core", core.clone(), Environment::Roblox),
            stored_in("acme/util", util, Environment::Luau, Environment::Roblox),
            stored("acme/lifecycles", lifecycles.clone(), Environment::Roblox),
        ];
        let mut warnings = Vec::new();
        link_nested_dependencies(&packages, &mut |message| warnings.push(message));
        assert_eq!(warnings, Vec::<String>::new());

        /* the same environment link, three hops up to the store, entry
        appended, exported types restated */
        assert_eq!(
            fs::read_to_string(lifecycles.join("packages/roblox/core.luau")).unwrap(),
            "local module = require(\"../../../acme_core/out/lpm\")\n\
             export type Entry = module.Entry\n\
             return module\n"
        );
        /* the luau environment dependency. the link folder keeps the
        dependency's own environment name, but its target stays inside
        this same output root, no `..` ever escapes it */
        assert_eq!(
            fs::read_to_string(lifecycles.join("packages/luau/util.luau")).unwrap(),
            "return require(\"../../../acme_util\")\n"
        );
        // packages without dependencies get no packages/ folder at all
        assert!(!core.join("packages").exists());

        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn contexts_keep_their_trees_apart() {
        let base = std::env::temp_dir().join("lpm-test-nested-links-contexts");
        let _ = fs::remove_dir_all(&base);
        let roblox = base.join("packages/roblox");
        let server = base.join("packages/server");

        /* the same dependency name installed under both roots, each
        consumer must link the copy in its OWN tree */
        let roblox_dep = roblox.join(".lpm/acme_dep");
        write(&roblox_dep, "init.luau", "return { tree = \"roblox\" }\n");
        let server_dep = server.join(".lpm/acme_dep");
        write(&server_dep, "init.luau", "return { tree = \"server\" }\n");
        let consumer = server.join(".lpm/acme_service");
        write(
            &consumer,
            "lpm.toml",
            "[dependencies]\ndep = { name = \"acme/dep\", version = \"^\" }\n",
        );
        write(&consumer, "init.luau", "return {}\n");

        let packages = [
            stored("acme/dep", roblox_dep, Environment::Roblox),
            stored_in(
                "acme/dep",
                server_dep,
                Environment::Roblox,
                Environment::Server,
            ),
            stored_in(
                "acme/service",
                consumer.clone(),
                Environment::Server,
                Environment::Server,
            ),
        ];
        let mut warnings = Vec::new();
        link_nested_dependencies(&packages, &mut |message| warnings.push(message));
        assert_eq!(warnings, Vec::<String>::new());

        /* the link folder is named for the dep's own environment, roblox,
        but the require resolves within the server root */
        let link = fs::read_to_string(consumer.join("packages/roblox/dep.luau")).unwrap();
        assert_eq!(link, "return require(\"../../../acme_dep\")\n");

        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn redirected_edges_link_the_replacement() {
        let base = std::env::temp_dir().join("lpm-test-nested-links-redirect");
        let _ = fs::remove_dir_all(&base);
        let roblox = base.join("packages/roblox");

        /* the shipped manifest still declares acme/bar, but [overrides]
        swapped the edge for acme/qux. the nested link must follow the
        redirect, not the manifest */
        let qux = roblox.join(".lpm/acme_qux");
        write(&qux, "init.luau", "return {}\n");
        let consumer = roblox.join(".lpm/acme_foo");
        write(
            &consumer,
            "lpm.toml",
            "[dependencies]\nbar = { name = \"acme/bar\", version = \"^1\" }\n",
        );

        let mut redirected = stored("acme/foo", consumer.clone(), Environment::Roblox);
        redirected.redirects = [("bar".to_string(), "acme/qux".to_string())].into();
        let packages = [stored("acme/qux", qux, Environment::Roblox), redirected];
        let mut warnings = Vec::new();
        link_nested_dependencies(&packages, &mut |message| warnings.push(message));

        assert_eq!(warnings, Vec::<String>::new());
        assert_eq!(
            fs::read_to_string(consumer.join("packages/roblox/bar.luau")).unwrap(),
            "return require(\"../../../acme_qux\")\n"
        );

        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn missing_dependencies_warn_and_skip() {
        let base = std::env::temp_dir().join("lpm-test-nested-links-missing");
        let _ = fs::remove_dir_all(&base);
        let storage = base.join("packages/roblox/.lpm/acme_thing");
        write(
            &storage,
            "lpm.toml",
            "[dependencies]\ngone = { name = \"acme/gone\", version = \"^\" }\n",
        );

        let packages = [stored("acme/thing", storage.clone(), Environment::Roblox)];
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
        let roblox = base.join("packages/roblox");

        let dep = roblox.join(".lpm/acme_dep");
        write(&dep, "init.luau", "return {}\n");
        /* a downloaded manifest can quote anything as a key, neither of
        these may put a file outside the package */
        let hostile = roblox.join(".lpm/acme_hostile");
        write(
            &hostile,
            "lpm.toml",
            "[dependencies]\n\"../../../../../../escaped\" = { name = \"acme/dep\", version = \"^\" }\n\
             \"C:/Windows/Temp/lpm-escaped\" = { name = \"acme/dep\", version = \"^\" }\n",
        );

        let packages = [
            stored("acme/dep", dep, Environment::Roblox),
            stored("acme/hostile", hostile.clone(), Environment::Roblox),
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
        let consumer = base.join("packages/roblox/.lpm/acme_extras");
        write(
            &consumer,
            "lpm.toml",
            "[dependencies]\ncore = { name = \"acme/core\", version = \"^\" }\n",
        );

        let packages = [
            StoredPackage {
                name: "acme/core".to_string(),
                storage: member.clone(),
                environment: Environment::Roblox,
                context: Environment::Roblox,
                in_place: true,
                redirects: BTreeMap::new(),
            },
            stored("acme/extras", consumer.clone(), Environment::Roblox),
        ];
        let mut warnings = Vec::new();
        link_nested_dependencies(&packages, &mut |message| warnings.push(message));

        assert_eq!(warnings, Vec::<String>::new());
        assert_eq!(
            fs::read_to_string(consumer.join("packages/roblox/core.luau")).unwrap(),
            "return require(\"../../../../../core/src\")\n"
        );
        // the member's own source tree stays untouched
        assert!(!member.join("packages").exists());

        let _ = fs::remove_dir_all(&base);
    }
}
