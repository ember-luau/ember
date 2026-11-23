/*! GitHub-released binaries a project pins by version. this module stores
and installs them. `archive` picks and unpacks the right release asset,
`shim` maps an alias to one of the stored binaries. `npm` does the same
for tools that npm publishes. `ember` turns a tool on the Ember registry
into the release shape of the GitHub path.

A tool can install more than one binary. The tool owner attaches a
`tool.toml` to the release, and its `[tool] bins` names each binary. The
first binary is the main binary, and the alias of [tools] runs it. Each
other binary gets a shim under its own name. */

pub mod archive;
pub mod ember;
pub mod npm;
pub mod shim;
pub(crate) mod shorthands;

use crate::error::Error;
use crate::net::github::GithubAPI;
use crate::net::http;
use crate::net::http::responses::{Asset, Release};
use crate::project::lockfile::LockedTool;
use crate::project::manifest::{EMBER_PREFIX, Tool, split_package_name};
use crate::sys::{hash, paths};
use std::env;
use std::fs;
use std::path::{Path, PathBuf};

/** tools addable and `embr execute`-runnable by bare name. the registry
adds more (`shorthands`), but never one of these. anything else must be
"owner/repo". */
const SHORTHANDS: &[(&str, &str)] = &[
    ("alloy", "alloy-luau/alloy"),
    ("create-chief-project", "ryancundiff/create-chief-project"),
    ("darklua", "seaofvoices/darklua"),
    ("lest", "lest-luau/lest"),
    ("larvae", "larvae-luau/larvae"),
    ("luau-lsp", "johnnymorganz/luau-lsp"),
    ("luaux", "luau-xml/luaux"),
    ("rojo", "rojo-rbx/rojo"),
    ("stylua", "johnnymorganz/stylua"),
];

/// shorthand to full repo name, unknown names pass through as "owner/repo".
pub fn expand_shorthand(name: &str) -> String {
    shorthand_repository(name).unwrap_or_else(|| name.to_string())
}

/** the target behind a shorthand, for matching manifest entries by value,
not alias key. case-insensitive, like alias resolution. windows command
lookup doesn't care and neither should "StyLua". a registry target can be
`npm:<package>` as well as "owner/repo". */
pub fn shorthand_repository(name: &str) -> Option<String> {
    resolve_shorthand(name, shorthands::lookup)
}

/** the built-ins first, always. only a bare name they do not know asks the
registry, which may fetch its list once a run. */
fn resolve_shorthand(name: &str, registry: impl FnOnce(&str) -> Option<String>) -> Option<String> {
    let builtin = SHORTHANDS
        .iter()
        .find(|(short, _)| short.eq_ignore_ascii_case(name))
        .map(|(_, full)| full.to_string());
    builtin.or_else(|| shorthands::is_name(name).then(|| registry(name)).flatten())
}

/** alias names embr's own binaries occupy in ~/.ember/bin. a tool shim under
one of these would shadow the CLI or the embx launcher. case-insensitive,
matching how windows resolves commands. */
const RESERVED_ALIASES: &[&str] = &["embr", "embx"];

/// whether an alias would shadow embr's own binaries as a bin shim.
pub fn is_reserved_alias(alias: &str) -> bool {
    RESERVED_ALIASES
        .iter()
        .any(|reserved| alias.eq_ignore_ascii_case(reserved))
}

/** ~/.ember/tools/{owner}_{repo}/{version}. GitHub owner names can't contain
'_', so the first '_' always marks the owner/repo split when reading the
folder name back. */
pub fn storage_dir(repository: &str, version: &str) -> Result<PathBuf, Error> {
    Ok(tool_root(repository)?.join(version))
}

/** the folder of every stored version of one tool. an Ember tool sits in
~/.ember/tools/ember/{scope}+{name}: a scope may hold '_', so the GitHub
form could not be read back, and a GitHub folder always has a '_', so none
is called `ember`. a package name never holds '+'. */
pub fn tool_root(repository: &str) -> Result<PathBuf, Error> {
    let tools = paths::tools_dir()?;
    Ok(match repository.strip_prefix(EMBER_PREFIX) {
        Some(name) => tools.join("ember").join(name.replace('/', "+")),
        None => tools.join(repository.replace('/', "_")),
    })
}

/** the Ember name to ask first for a bare GitHub owner/repo. lowercase,
since Ember names are, and a pin lives under the GitHub owner in lowercase.
None when Ember cannot hold the name, a '.' in it say. */
pub fn ember_candidate(repository: &str) -> Option<String> {
    let lower = repository.to_ascii_lowercase();
    split_package_name(&lower).is_ok().then_some(lower)
}

/// the release of `version`, from Ember for an `ember:` tool, else from GitHub.
fn get_release(repository: &str, version: &str, github: &GithubAPI) -> Result<Release, Error> {
    match repository.strip_prefix(EMBER_PREFIX) {
        Some(name) => ember::release(name, Some(version)),
        None => github.get_release(repository, version),
    }
}

/// the latest release, from Ember for an `ember:` tool, else from GitHub.
pub fn get_latest_release(repository: &str, github: &GithubAPI) -> Result<Release, Error> {
    match repository.strip_prefix(EMBER_PREFIX) {
        Some(name) => ember::release(name, None),
        None => github.get_latest_release(repository),
    }
}

/// where a tool's executable sits once installed.
fn stored_executable(repository: &str, version: &str) -> Result<PathBuf, Error> {
    Ok(storage_dir(repository, version)?.join(executable_name(repo_short_name(repository))))
}

/** Whether a stored executable is present and runnable.

`exists()` on its own is not enough. An install from before the compressed
formats were sniffed left the asset itself at this path, and that file exists
forever without ever running, so the cache hit repeats and the tool stays
broken. Counting it as a miss re-downloads the release once and repairs the
entry in place. */
fn stored_is_usable(stored: &Path) -> bool {
    stored.exists() && !archive::is_packed(stored)
}

/** whether a pinned tool is fully present, version stored and alias
shimmed. the cheap check install's fast path runs instead of the full tool
phase: a few stats, a 512-byte read per tool, no network. */
pub fn is_installed(alias: &str, tool: &Tool) -> Result<bool, Error> {
    if is_reserved_alias(alias) {
        return Err(Error::ReservedToolAlias(alias.to_string()));
    }
    let stored = match tool.npm_package() {
        Some(package) => npm::is_stored(package, &tool.version)?,
        None => stored_is_usable(&stored_executable(&tool.repository, &tool.version)?),
    };
    // a binary after the main one gets no shim over a command of the computer, see `install_tool`
    Ok(stored
        && shim::path(alias)?.exists()
        && extra_bins(&tool.repository, &tool.version)
            .iter()
            .all(|bin| {
                shim::path(bin).is_ok_and(|shim| shim.exists())
                    || shim::elsewhere_on_path(bin).is_some()
            }))
}

/** the SHA-256 that ember.lock records for the release assets of the
project's tools. a GitHub release keeps its version when its files are
replaced, so the version alone does not say that this computer gets what
the last one got. an asset with a pin must match it, and an asset with
none is taken as it comes and pinned by the install that downloads it. */
#[derive(Default)]
pub struct Pins {
    tools: Vec<LockedTool>,
    /// `--refresh`: a pin that does not match is replaced, not enforced.
    replace: bool,
}

impl Pins {
    /// no pins, for a tool that no project's lock covers: `embx`, a global tool.
    pub fn none() -> Self {
        Pins::default()
    }

    pub fn new(tools: Vec<LockedTool>, replace: bool) -> Self {
        Pins { tools, replace }
    }

    /// the pinned SHA-256 of `asset` in `repository`@`version`.
    fn sha256(&self, repository: &str, version: &str, asset: &str) -> Option<&str> {
        if self.replace {
            return None;
        }
        self.tools
            .iter()
            .find(|tool| {
                tool.name.eq_ignore_ascii_case(repository)
                    && tool.version == version
                    && tool.asset == asset
            })
            .map(|tool| tool.sha256.as_str())
    }

    /// the mismatch error when `asset` has a pin and `received` is not it.
    fn check(
        &self,
        repository: &str,
        version: &str,
        asset: &str,
        received: &str,
    ) -> Result<(), Error> {
        match self.sha256(repository, version, asset) {
            Some(expected) if expected != received => Err(Error::ToolLockMismatch {
                tool: format!("{repository}@{version}"),
                asset: asset.to_string(),
                expected: expected.to_string(),
                received: received.to_string(),
            }),
            _ => Ok(()),
        }
    }
}

/** the file in a stored version's folder that names each release asset it
was unpacked from and that asset's SHA-256, in the form `sha256sum` prints.
a version stored by an embr from before the file has none. */
const ASSET_RECORD: &str = ".embr-asset";

/** the release asset in which a tool owner names the binaries of the tool,
as `bins` under `[tool]`. it is the file that `embr tool publish` reads. */
pub const TOOL_FILE: &str = "tool.toml";

/** the file in a stored version's folder that names each binary after the
main binary, one on each line. a tool with one binary has no such file. */
const BINS_RECORD: &str = ".embr-bins";

/** the name and SHA-256 of each asset that the stored `version` of
`repository` came from. empty for a version that an older embr stored, and
for a tool that npm installs. */
pub fn stored_assets(repository: &str, version: &str) -> Vec<(String, String)> {
    let Ok(storage) = storage_dir(repository, version) else {
        return Vec::new();
    };
    fs::read_to_string(storage.join(ASSET_RECORD))
        .unwrap_or_default()
        .lines()
        .filter_map(|line| line.split_once("  "))
        .filter(|(sha256, asset)| hash::is_sha256_hex(sha256) && !asset.is_empty())
        .map(|(sha256, asset)| (asset.to_string(), sha256.to_string()))
        .collect()
}

/** the binaries that the stored `version` of `repository` holds after its
main binary. each one has a shim under its own name. empty for a tool with
one binary, and for a version that is not stored. */
pub fn extra_bins(repository: &str, version: &str) -> Vec<String> {
    storage_dir(repository, version).map_or_else(|_| Vec::new(), |storage| bins_in(&storage))
}

/// the binaries that the record in the version folder `storage` names.
pub(crate) fn bins_in(storage: &Path) -> Vec<String> {
    fs::read_to_string(storage.join(BINS_RECORD))
        .unwrap_or_default()
        .lines()
        .filter(|bin| paths::is_plain_file_name(bin))
        .map(str::to_string)
        .collect()
}

/// `[tool] bins` of tool.toml. the install reads no other field.
#[derive(serde::Deserialize)]
struct BinsFile {
    tool: Option<BinsTable>,
}

#[derive(serde::Deserialize)]
struct BinsTable {
    bins: Option<Vec<String>>,
}

/** the binaries that the tool.toml in `bytes` names, the main binary first.
empty when the file has no `bins`. the error is the reason. */
fn declared_bins(bytes: &[u8]) -> Result<Vec<String>, String> {
    let text = std::str::from_utf8(bytes).map_err(|_| "the file is not UTF-8".to_string())?;
    let file: BinsFile = toml::from_str(text).map_err(|error| error.message().to_string())?;
    let bins = file.tool.and_then(|tool| tool.bins).unwrap_or_default();
    check_bins(&bins)?;
    Ok(bins)
}

/** the rules for `bins`. a binary name becomes a file name and a command
name, so it must be safe as each. the error is the reason. */
pub(crate) fn check_bins(bins: &[String]) -> Result<(), String> {
    for (index, bin) in bins.iter().enumerate() {
        let valid = (1..=64).contains(&bin.len())
            && !bin.starts_with('.')
            && bin
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || "._+-".contains(c));
        if !valid {
            return Err(format!(
                "binary name '{bin}' must be 1 to 64 of A-Z a-z 0-9 . _ + -, not starting with ."
            ));
        }
        if is_reserved_alias(bin) {
            return Err(format!("binary name '{bin}' is a command of embr"));
        }
        // `nul`, `com1.exe` and a name that ends in a dot are no file names on Windows
        if paths::store_folder(bin).is_err() {
            return Err(format!("binary name '{bin}' is not a file name on Windows"));
        }
        if bins[..index]
            .iter()
            .any(|other| other.eq_ignore_ascii_case(bin))
        {
            return Err(format!("binary '{bin}' is there twice"));
        }
    }
    Ok(())
}

/** the entries of ember.lock's `[[tool]]` after an install of `tools`, the
project's own GitHub and Ember tools. each tool keeps the assets that
`previous` holds for its version, which other platforms wrote, and gains
the assets that this computer has stored. a tool that left [tools], or
moved to another version, loses its entries. */
pub fn locked_assets<'a>(
    tools: impl Iterator<Item = &'a Tool>,
    previous: &[LockedTool],
    replace: bool,
) -> Vec<LockedTool> {
    let mut locked = Vec::new();
    for tool in tools.filter(|tool| tool.npm_package().is_none()) {
        let stored = stored_assets(&tool.repository, &tool.version);
        locked.extend(
            previous
                .iter()
                .filter(|entry| {
                    entry.name.eq_ignore_ascii_case(&tool.repository)
                        && entry.version == tool.version
                })
                // an asset stored here is written below, with the hash it has now
                .filter(|entry| !replace || stored.iter().all(|(asset, _)| *asset != entry.asset))
                .cloned(),
        );
        for (asset, sha256) in stored {
            if !locked.iter().any(|entry: &LockedTool| {
                entry.name.eq_ignore_ascii_case(&tool.repository)
                    && entry.version == tool.version
                    && entry.asset == asset
            }) {
                locked.push(LockedTool {
                    name: tool.repository.clone(),
                    version: tool.version.clone(),
                    asset,
                    sha256,
                });
            }
        }
    }
    locked.sort();
    locked.dedup();
    locked
}

/** Installs a manifest tool, skipping the network when the version is
already stored. true = downloaded, false = cache reused. the bin shim
gets refreshed either way. `on_bytes` hears the asset download, see
[`http::get_bytes_with`]. */
pub fn install_tool(
    alias: &str,
    tool: &Tool,
    github: &GithubAPI,
    pins: &Pins,
    on_bytes: &mut dyn FnMut(u64, Option<u64>),
) -> Result<bool, Error> {
    if is_reserved_alias(alias) {
        return Err(Error::ReservedToolAlias(alias.to_string()));
    }
    if let Some(package) = tool.npm_package() {
        let installed = npm::install(package, &tool.version)?;
        shim::write(&shim::path(alias)?)?;
        return Ok(installed);
    }
    let stored = stored_executable(&tool.repository, &tool.version)?;
    let downloaded = if stored_is_usable(&stored) {
        check_stored(&tool.repository, &tool.version, pins)?;
        false
    } else {
        let release = get_release(&tool.repository, &tool.version, github)?;
        store_release(&tool.repository, &release, alias, pins, on_bytes)?.1
    };
    shim::write(&shim::path(alias)?)?;
    /* the person wrote the alias in [tools]. the names of the other
    binaries come from the tool.toml of the release, and a tool that names
    one `git` would take the place of git in every terminal. so a name that
    is a command of this computer already gets no shim, and the caller
    warns */
    for bin in extra_bins(&tool.repository, &tool.version) {
        if shim::elsewhere_on_path(&bin).is_none() {
            shim::write(&shim::path(&bin)?)?;
        }
    }
    Ok(downloaded)
}

/** refuses a stored version that came from another asset than the one
ember.lock pins. a version stored with no record of its assets passes: it
is from before embr kept one, and there is nothing to compare. */
fn check_stored(repository: &str, version: &str, pins: &Pins) -> Result<(), Error> {
    stored_assets(repository, version)
        .iter()
        .try_for_each(|(asset, sha256)| pins.check(repository, version, asset, sha256))
}

/** Downloads the platform asset of a fetched release, extracts it, stores
the executable under `storage_dir`, and writes a bin shim named after
the alias. true = downloaded, false = version already stored. the shim
gets refreshed either way. */
pub fn install_release(
    alias: &str,
    repository: &str,
    release: &Release,
    on_bytes: &mut dyn FnMut(u64, Option<u64>),
) -> Result<bool, Error> {
    let (_, downloaded) = store_release(repository, release, alias, &Pins::none(), on_bytes)?;
    shim::write(&shim::path(alias)?)?;
    Ok(downloaded)
}

/** the storage half of `install_release`. downloads the platform asset of
a fetched release, extracts it, and stores the executable, no bin shim,
no manifest, which is what `embr execute` runs on. returns the stored
executable and whether a download happened. `name_hint` is an extra
candidate name when picking the executable out of a multi-file asset.

a release that holds a tool.toml with `bins` stores each binary. a binary
that owns release assets, see [`archive::asset_owner`], comes from its
platform asset. a binary that owns none comes from the archive of the main
binary. */
pub fn store_release(
    repository: &str,
    release: &Release,
    name_hint: &str,
    pins: &Pins,
    on_bytes: &mut dyn FnMut(u64, Option<u64>),
) -> Result<(PathBuf, bool), Error> {
    let version = release.tag_name.trim_start_matches('v');
    let storage = storage_dir(repository, version)?;
    let repo = repo_short_name(repository);

    let stored = storage.join(executable_name(repo));
    if stored_is_usable(&stored) {
        check_stored(repository, version, pins)?;
        return Ok((stored, false));
    }

    // what ember.lock pins, and what a later install compares its pins with
    let mut record = String::new();
    let mut fetch = |asset: &Asset| -> Result<Vec<u8>, Error> {
        let bytes = http::get_bytes_with(&asset.browser_download_url, &[], &mut *on_bytes)?;
        /* before anything reaches disk, so a bad download leaves nothing
        behind. Ember lists a hash for an upload, and GitHub for each asset
        uploaded since June 2025. that hash comes from the same place as the
        release, so it catches a bad transfer or a changed mirror. the pin in
        ember.lock is what catches a release that was replaced */
        let received = hash::sha256_hex(&bytes);
        if let Some(expected) = &asset.sha256
            && !received.eq_ignore_ascii_case(expected)
        {
            return Err(Error::ToolAssetMismatch {
                tool: format!("{repository}@{version}"),
                asset: asset.name.clone(),
                expected: expected.clone(),
                received,
            });
        }
        pins.check(repository, version, &asset.name, &received)?;
        record.push_str(&format!("{received}  {}\n", asset.name));
        Ok(bytes)
    };

    let invalid = |reason: String| Error::ToolFileInvalid {
        tool: format!("{repository}@{version}"),
        reason,
    };
    let bins = match release.assets.iter().find(|asset| asset.name == TOOL_FILE) {
        Some(asset) => declared_bins(&fetch(asset)?).map_err(invalid)?,
        None => Vec::new(),
    };
    let (main, extras) = match bins.split_first() {
        Some((main, extras)) => (main.as_str(), extras),
        None => (repo, &[][..]),
    };
    // the main binary is stored under the repo name, so no other binary can take it
    if extras.iter().any(|bin| bin.eq_ignore_ascii_case(repo)) {
        return Err(invalid(format!(
            "`{repo}` is the main binary, so `bins` must list it first"
        )));
    }

    /* the platform asset of one binary, or with None of the main binary:
    an asset that no other binary owns. a tool with one binary has no
    owners, so this is every asset, as before */
    let select = |owner: Option<&str>| {
        archive::select_from(
            release
                .assets
                .iter()
                .filter(|asset| archive::asset_owner(&asset.name, extras) == owner),
            env::consts::OS,
            env::consts::ARCH,
        )
        .ok_or_else(|| Error::NoMatchingAsset {
            tool: repository.to_string(),
            version: version.to_string(),
        })
    };
    let asset = select(None)?;
    let bytes = fetch(asset)?;

    /* extract into a staging sibling first so a failed install never
    leaves a half-written storage dir behind, same dance as install.rs.
    pid-suffixed because concurrent `embx` runs of one cold tool are
    routine and must not tear down each other's staging mid-extraction. */
    let staging = Staging(paths::with_suffix(
        &storage,
        &format!(".{}.tmp", std::process::id()),
    ));
    if staging.0.exists() {
        fs::remove_dir_all(&staging.0)?;
    }
    fs::create_dir_all(&staging.0)?;

    let target = staging.0.join(executable_name(repo));
    archive::extract(&asset.name, &bytes, &staging.0, &target)?;

    let mut files = Vec::new();
    archive::collect_files(&staging.0, &mut files)?;
    let found = archive::pick_executable(&files, main, name_hint, env::consts::EXE_SUFFIX)
        .ok_or_else(|| Error::NoExecutableInAsset(name_hint.to_string()))?;
    place_executable(&found, &target)?;

    for bin in extras {
        let file_name = executable_name(bin);
        let missing = || Error::NoExecutableInAsset(format!("{repository} ({bin})"));
        let target = staging.0.join(&file_name);
        let owns_assets = release
            .assets
            .iter()
            .any(|asset| archive::asset_owner(&asset.name, extras) == Some(bin));
        if owns_assets {
            /* ponytail: only the executable of the asset is kept. keep the
            folder if a binary ever needs the files beside it */
            let asset = select(Some(bin))?;
            let bytes = fetch(asset)?;
            let unpacked = staging.0.join(format!(".{bin}.tmp"));
            fs::create_dir_all(&unpacked)?;
            archive::extract(&asset.name, &bytes, &unpacked, &unpacked.join(&file_name))?;
            let mut files = Vec::new();
            archive::collect_files(&unpacked, &mut files)?;
            let found = archive::pick_executable(&files, bin, bin, env::consts::EXE_SUFFIX)
                .ok_or_else(missing)?;
            place_executable(&found, &target)?;
            fs::remove_dir_all(&unpacked)?;
        } else {
            // no asset of its own: the archive of the main binary holds it, by name
            let found = files
                .iter()
                .map(|(path, _)| path)
                .find(|path| {
                    path.file_name()
                        .and_then(|name| name.to_str())
                        .is_some_and(|name| name.eq_ignore_ascii_case(&file_name))
                })
                .ok_or_else(missing)?;
            place_executable(found, &target)?;
        }
        make_executable(&target)?;
    }
    fs::write(staging.0.join(ASSET_RECORD), &record)?;
    if !extras.is_empty() {
        fs::write(staging.0.join(BINS_RECORD), extras.join("\n") + "\n")?;
    }

    /* a concurrent run may have stored this version while we downloaded.
    its copy of the same release is just as good, and possibly already
    executing, in which case deleting it would fail on windows anyway */
    if stored_is_usable(&stored) {
        return Ok((stored, true));
    }

    // move staging into place, same filesystem, so it's a rename.
    if storage.exists() {
        fs::remove_dir_all(&storage)?;
    }
    fs::create_dir_all(storage.parent().expect("storage dir has a parent"))?;
    fs::rename(&staging.0, &storage)?;

    let stored = storage.join(executable_name(repo));
    make_executable(&stored)?;
    Ok((stored, true))
}

/** the stored executable for an exact version of a repo, downloading that
release when storage is missing. the `embr execute <spec>@version` path,
no shim, no manifest entry. */
pub fn ensure_stored(
    repository: &str,
    version: &str,
    github: &GithubAPI,
) -> Result<PathBuf, Error> {
    let stored = stored_executable(repository, version)?;
    if stored_is_usable(&stored) {
        return Ok(stored);
    }
    let release = get_release(repository, version, github)?;
    Ok(store_release(
        repository,
        &release,
        repo_short_name(repository),
        &Pins::none(),
        &mut |_, _| {},
    )?
    .0)
}

/** the stored executable that answers for `repository` with no request:
`version` when it is stored, or with no version the latest that a fresh
stamp names. `stale` takes a stamp of any age, the offline answer. */
pub fn cached(
    repository: &str,
    version: Option<&str>,
    stale: bool,
) -> Result<Option<PathBuf>, Error> {
    let stamp = latest_stamp(repository)?;
    let version = match version {
        Some(version) => Some(version.to_string()),
        None if stale => stamped_version(&stamp),
        None => fresh_latest_version(&stamp),
    };
    let Some(version) = version else {
        return Ok(None);
    };
    let stored = stored_executable(repository, &version)?;
    Ok(stored_is_usable(&stored).then_some(stored))
}

/** the stored executable for a repo's latest release, asking GitHub at
most once per TTL. the last answer is stamped under the repo's storage
dir, and a fresh stamp whose version is still stored means zero network.
anonymous GitHub API calls are capped at 60/hour, so `embx stylua` in a
loop must coast on the stamp. `refresh` forces the question. */
pub fn ensure_latest(
    repository: &str,
    github: &GithubAPI,
    refresh: bool,
) -> Result<PathBuf, Error> {
    let stamp = latest_stamp(repository)?;
    if !refresh && let Some(version) = fresh_latest_version(&stamp) {
        return ensure_stored(repository, &version, github);
    }

    let release = match get_latest_release(repository, github) {
        Ok(release) => release,
        /* offline or rate-limited past the TTL, a stale answer whose
        binaries are still stored beats dying. the stamp is left as-is,
        so the next run asks again instead of trusting this one. */
        Err(error) => {
            if let Some(version) = stamped_version(&stamp) {
                let stored = stored_executable(repository, &version)?;
                if stored_is_usable(&stored) {
                    eprintln!(
                        "warning: could not check {repository} for a newer release ({error}); running {version}"
                    );
                    return Ok(stored);
                }
            }
            return Err(error);
        }
    };
    let stored = store_release(
        repository,
        &release,
        repo_short_name(repository),
        &Pins::none(),
        &mut |_, _| {},
    )?
    .0;

    /* stamped only after a successful store, so a failed download is
    retried next run instead of being remembered as the answer */
    if let Some(parent) = stamp.parent() {
        let _ = fs::create_dir_all(parent);
    }
    let _ = fs::write(
        &stamp,
        format!("{}\n", release.tag_name.trim_start_matches('v')),
    );

    Ok(stored)
}

/** where a repo's latest-release answer is remembered. the version string,
with the file's mtime marking when GitHub was last asked. lives beside the
version dirs, whose readers all skip non-directories. */
fn latest_stamp(repository: &str) -> Result<PathBuf, Error> {
    Ok(tool_root(repository)?.join(".latest"))
}

/** the stamped version if the stamp is younger than the TTL, None
otherwise. deliberately shares the index TTL knob `EMBER_INDEX_TTL_SECS`,
0 = never fresh, one dial for how stale cached answers may be. */
fn fresh_latest_version(stamp: &Path) -> Option<String> {
    use crate::registry::index;
    if !index::stamp_fresh(stamp, index::index_ttl()) {
        return None;
    }
    stamped_version(stamp)
}

/// the version a stamp remembers, fresh or stale, the offline fallback.
fn stamped_version(stamp: &Path) -> Option<String> {
    let version = fs::read_to_string(stamp).ok()?;
    let version = version.trim();
    (!version.is_empty()).then(|| version.to_string())
}

/// the "repo" half of "owner/repo", names the stored binary.
/** the extraction staging dir, removed on the way out.

on success it has already been renamed into storage and there is nothing
left to remove. on failure -- and every step between creating it and that
rename can fail -- it would otherwise stay behind forever, one
`<version>.<pid>.tmp` per attempt, which is how a tool that cannot
install quietly fills its own storage folder. */
struct Staging(PathBuf);

impl Drop for Staging {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

/** puts the executable the archive actually shipped under the name embr
stores it by.

the two are compared through the filesystem rather than as text, because
on windows and macOS they can be one file under two spellings:
JohnnyMorganz/StyLua ships `stylua.exe` while the repo's short name makes
the target `StyLua.exe`. Comparing paths as strings called those
different, so `target.exists()` -- true, being the very file just
extracted -- deleted it, and the rename then failed with a bare "the
system cannot find the file specified". Tools whose archive name matches
their repo, like rojo, never hit it. */
fn place_executable(found: &Path, target: &Path) -> Result<(), Error> {
    if paths::same_file(found, target) {
        return Ok(());
    }
    if target.exists() {
        fs::remove_file(target)?;
    }
    fs::rename(found, target)?;
    Ok(())
}

fn repo_short_name(repository: &str) -> &str {
    repository
        .split_once('/')
        .map_or(repository, |(_, repo)| repo)
}

/// "rojo.exe" on windows, "rojo" elsewhere.
fn executable_name(repo: &str) -> String {
    format!("{repo}{}", env::consts::EXE_SUFFIX)
}

/// chmod 755. no-op on windows, where the .exe extension is what makes it executable.
#[cfg(unix)]
pub(crate) fn make_executable(path: &Path) -> Result<(), Error> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(0o755))?;
    Ok(())
}

#[cfg(not(unix))]
pub(crate) fn make_executable(_path: &Path) -> Result<(), Error> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::net::http::mock;

    #[test]
    fn a_stored_archive_counts_as_a_cache_miss() {
        let dir = std::env::temp_dir().join("embr-test-usable-stored");
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();

        let packed = dir.join("blink");
        fs::write(&packed, [0xfd, b'7', b'z', b'X', b'Z', 0x00, 0x00, 0x04]).unwrap();
        /* the entry exists, so every exists() check passed it and the tool
        stayed broken. it has to read as a miss for the release to be
        fetched again and the entry repaired. */
        assert!(packed.exists());
        assert!(!stored_is_usable(&packed));

        let real = dir.join("stylua");
        fs::write(&real, [0x7f, b'E', b'L', b'F', 2, 1, 1, 0]).unwrap();
        assert!(stored_is_usable(&real));

        assert!(!stored_is_usable(&dir.join("never-installed")));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn storage_paths_include_owner_repo_and_version() {
        let dir = storage_dir("rojo-rbx/rojo", "7.4.4").unwrap();
        assert!(dir.starts_with(paths::tools_dir().unwrap()));
        assert!(dir.ends_with(Path::new("tools").join("rojo-rbx_rojo").join("7.4.4")));

        // Ember keeps its own folder, apart from a GitHub repo of the same name
        let dir = storage_dir("ember:rojo-rbx/rojo", "7.4.4").unwrap();
        assert!(
            dir.ends_with(
                Path::new("tools")
                    .join("ember")
                    .join("rojo-rbx+rojo")
                    .join("7.4.4")
            )
        );
        assert_eq!(
            stored_executable("ember:acme/tool", "1.0.0").unwrap(),
            storage_dir("ember:acme/tool", "1.0.0")
                .unwrap()
                .join(executable_name("tool"))
        );
    }

    /** an asset whose SHA-256 is not the one that Ember lists fails the
    install, and nothing of it is stored. */
    #[test]
    fn an_asset_that_does_not_match_its_sha256_is_not_stored() {
        let (cdn, _) = mock::serve(|_| mock::reply("200 OK", "not the tool"));
        let to_cdn = format!(
            "HTTP/1.1 302 Found\r\nLocation: {cdn}/tools/acme/sha-test/1.0.0/tool.zip\r\n\
             Content-Length: 0\r\nConnection: close\r\n\r\n"
        );
        let (api, api_log) = mock::serve(move |_| to_cdn.clone());
        let asset = format!("tool-{}-{}.zip", env::consts::OS, env::consts::ARCH);
        let release = Release {
            tag_name: "0.0.0-embr-test".to_string(),
            assets: vec![http::responses::Asset {
                name: asset.clone(),
                browser_download_url: format!(
                    "{api}/v1/tools/acme/sha-test/versions/0.0.0-embr-test/assets/{asset}"
                ),
                sha256: Some(hash::sha256_hex(b"the real tool")),
            }],
            draft: false,
        };

        let refused = store_release(
            "ember:acme/sha-test",
            &release,
            "sha-test",
            &Pins::none(),
            &mut |_, _| {},
        );
        let Err(Error::ToolAssetMismatch {
            expected, received, ..
        }) = refused
        else {
            panic!("a wrong hash fails: {:?}", refused.err());
        };
        assert_eq!(expected, hash::sha256_hex(b"the real tool"));
        assert_eq!(received, hash::sha256_hex(b"not the tool"));
        // the redirect was followed, and no folder was made
        assert_eq!(api_log.lock().unwrap().len(), 1);
        assert!(!tool_root("ember:acme/sha-test").unwrap().exists());
    }

    /** regression. JohnnyMorganz/StyLua ships `stylua.exe`, and the repo's
    short name makes embr want `StyLua.exe`. On a case-insensitive
    filesystem those are one file, and comparing the paths as text treated
    them as two: the target "already existed", so it was deleted -- it was
    the extracted binary -- and the rename then failed with os error 2.
    Every attempt also left its staging folder behind. */
    #[test]
    fn an_archive_naming_its_binary_in_another_case_still_installs() {
        let staging = std::env::temp_dir().join("embr-test-place-executable");
        let _ = fs::remove_dir_all(&staging);
        fs::create_dir_all(&staging).unwrap();

        // what the archive shipped, and what embr wants to store it as
        let found = staging.join("stylua.exe");
        let target = staging.join("StyLua.exe");
        fs::write(&found, b"the real binary").unwrap();

        place_executable(&found, &target).unwrap();

        // readable under the name embr stores by, whichever spelling won
        assert_eq!(fs::read(&target).unwrap(), b"the real binary");
        // and exactly one binary, never a deleted one or a duplicate
        let left: Vec<_> = fs::read_dir(&staging).unwrap().flatten().collect();
        assert_eq!(
            left.len(),
            1,
            "{:?}",
            left.iter().map(|e| e.path()).collect::<Vec<_>>()
        );

        // a genuinely different name is still moved into place
        let other = staging.join("stylua-linux");
        fs::write(&other, b"another").unwrap();
        place_executable(&other, &target).unwrap();
        assert_eq!(fs::read(&target).unwrap(), b"another");
        assert!(!other.exists());

        let _ = fs::remove_dir_all(&staging);
    }

    /// a failed install must not leave its `<version>.<pid>.tmp` folder behind.
    #[test]
    fn staging_is_removed_however_the_install_ends() {
        let dir = std::env::temp_dir().join("embr-test-staging-guard");
        let _ = fs::remove_dir_all(&dir);

        {
            let staging = Staging(dir.clone());
            fs::create_dir_all(&staging.0).unwrap();
            fs::write(staging.0.join("half-extracted"), b"x").unwrap();
            assert!(dir.is_dir());
        }
        assert!(!dir.exists(), "staging outlived the install");
    }

    #[test]
    fn expands_known_shorthands_only() {
        assert_eq!(expand_shorthand("rojo"), "rojo-rbx/rojo");
        assert_eq!(expand_shorthand("acme/tool"), "acme/tool");
        assert_eq!(
            shorthand_repository("stylua").as_deref(),
            Some("johnnymorganz/stylua")
        );
        assert_eq!(shorthand_repository("acme/tool"), None);
        assert_eq!(expand_shorthand("luaux"), "luau-xml/luaux");
        // case-insensitive, like alias resolution and windows command lookup
        assert_eq!(expand_shorthand("StyLua"), "johnnymorganz/stylua");
        assert_eq!(
            shorthand_repository("ROJO").as_deref(),
            Some("rojo-rbx/rojo")
        );
    }

    #[test]
    fn built_ins_win_and_only_bare_names_ask_the_registry() {
        let asked = std::cell::RefCell::new(Vec::new());
        let registry = |name: &str| {
            asked.borrow_mut().push(name.to_string());
            match name {
                "rojo" => Some("evil/rojo".to_string()),
                "azul" => Some("npm:azul-sync".to_string()),
                "lint" => Some("ember:acme/lint".to_string()),
                _ => None,
            }
        };

        // a built-in never asks, so the registry can never re-point it
        assert_eq!(
            resolve_shorthand("rojo", registry).as_deref(),
            Some("rojo-rbx/rojo")
        );
        assert_eq!(
            resolve_shorthand("StyLua", registry).as_deref(),
            Some("johnnymorganz/stylua")
        );
        assert!(asked.borrow().is_empty());

        // then the registry, for a name the built-ins do not know
        assert_eq!(
            resolve_shorthand("azul", registry).as_deref(),
            Some("npm:azul-sync")
        );
        // an Ember target passes through as it is, `tool add` sends it to Ember
        assert_eq!(
            resolve_shorthand("lint", registry).as_deref(),
            Some("ember:acme/lint")
        );
        assert_eq!(resolve_shorthand("wally", registry), None);
        assert_eq!(*asked.borrow(), ["azul", "lint", "wally"]);

        // a full name is never a shorthand
        for full in [
            "acme/tool",
            "npm:azul-sync",
            "ember:acme/lint",
            "rojo@7.4.4",
        ] {
            assert_eq!(resolve_shorthand(full, registry), None, "{full}");
        }
        assert_eq!(asked.borrow().len(), 3);
    }

    #[test]
    fn shorthands_are_valid_repositories_and_never_reserved() {
        for (short, full) in SHORTHANDS {
            assert!(!short.contains('/'), "{short} is not a bare name");
            assert!(Tool::split_repository(full).is_ok(), "{full}");
            assert!(!is_reserved_alias(short), "{short} is reserved");
        }
        // the marquee embx demo stays wired to its repo
        assert_eq!(
            expand_shorthand("create-chief-project"),
            "ryancundiff/create-chief-project"
        );
    }

    #[test]
    fn reserved_aliases_refuse_tool_installs() {
        assert!(is_reserved_alias("embr"));
        assert!(is_reserved_alias("EMBX"));
        assert!(!is_reserved_alias("rojo"));

        let tool = Tool {
            repository: "acme/tool".to_string(),
            version: "1.0.0".to_string(),
        };
        assert!(matches!(
            is_installed("embx", &tool),
            Err(Error::ReservedToolAlias(_))
        ));
        assert!(matches!(
            install_tool(
                "EMBR",
                &tool,
                &GithubAPI::new(),
                &Pins::none(),
                &mut |_, _| {}
            ),
            Err(Error::ReservedToolAlias(_))
        ));
        // a binary name is a file name and a command name on every system
        let bins =
            |names: &[&str]| -> Vec<String> { names.iter().map(|n| n.to_string()).collect() };
        assert!(check_bins(&bins(&["kit", "kit-fmt", "kit.v2"])).is_ok());
        for bad in [
            "embr", "nul", "COM1", "aux.exe", "kit.", "../kit", "a/b", "a\\b", ".kit", "",
        ] {
            assert!(check_bins(&bins(&["kit", bad])).is_err(), "{bad}");
        }
    }

    #[test]
    fn latest_stamp_freshness_gates_reuse() {
        // the assertions below assume the default TTL
        if std::env::var_os("EMBER_INDEX_TTL_SECS").is_some()
            || std::env::var_os("LPM_INDEX_TTL_SECS").is_some()
        {
            return;
        }
        let dir = std::env::temp_dir().join("embr-test-latest-stamp");
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let stamp = dir.join(".latest");

        // no stamp, no answer
        assert_eq!(fresh_latest_version(&stamp), None);

        // a fresh stamp gives the stored version, whitespace trimmed
        fs::write(&stamp, "2.0.2\n").unwrap();
        assert_eq!(fresh_latest_version(&stamp).as_deref(), Some("2.0.2"));

        // a fresh but empty stamp is never an answer
        fs::write(&stamp, "\n").unwrap();
        assert_eq!(fresh_latest_version(&stamp), None);

        // aged past the TTL means stale, but still the offline fallback answer
        fs::write(&stamp, "2.0.2\n").unwrap();
        let old = std::time::SystemTime::now() - std::time::Duration::from_secs(24 * 60 * 60);
        fs::File::options()
            .write(true)
            .open(&stamp)
            .unwrap()
            .set_modified(old)
            .unwrap();
        assert_eq!(fresh_latest_version(&stamp), None);
        assert_eq!(stamped_version(&stamp).as_deref(), Some("2.0.2"));

        let _ = fs::remove_dir_all(&dir);
    }

    /// a release whose one asset is the bare executable `body`, served by a mock.
    fn raw_release(body: &'static str) -> (Release, String) {
        let (cdn, _) = mock::serve(move |_| mock::reply("200 OK", body));
        let asset = format!("tool-{}-{}", env::consts::OS, env::consts::ARCH);
        let release = Release {
            tag_name: "0.0.0-embr-test".to_string(),
            assets: vec![http::responses::Asset {
                name: asset.clone(),
                browser_download_url: format!("{cdn}/{asset}"),
                sha256: None,
            }],
            draft: false,
        };
        (release, asset)
    }

    fn pin(name: &str, version: &str, asset: &str, of: &[u8]) -> LockedTool {
        LockedTool {
            name: name.to_string(),
            version: version.to_string(),
            asset: asset.to_string(),
            sha256: hash::sha256_hex(of),
        }
    }

    /** the release was replaced after ember.lock recorded it: the asset has
    the name and the version that the lock pins, and other bytes. */
    #[test]
    fn an_asset_that_is_not_the_pinned_one_is_not_stored() {
        let repository = "ember:acme/pin-refused";
        let _ = fs::remove_dir_all(tool_root(repository).unwrap());
        let (release, asset) = raw_release("a replaced tool");
        let pins = Pins::new(
            vec![pin(repository, "0.0.0-embr-test", &asset, b"the tool")],
            false,
        );

        let refused = store_release(repository, &release, "pin-refused", &pins, &mut |_, _| {});
        let Err(Error::ToolLockMismatch {
            expected, received, ..
        }) = refused
        else {
            panic!("a changed release fails: {:?}", refused.err());
        };
        assert_eq!(expected, hash::sha256_hex(b"the tool"));
        assert_eq!(received, hash::sha256_hex(b"a replaced tool"));
        assert!(!tool_root(repository).unwrap().exists());
    }

    #[test]
    fn a_stored_tool_remembers_its_asset_and_answers_to_the_pin() {
        let repository = "ember:acme/pin-kept";
        let version = "0.0.0-embr-test";
        let _ = fs::remove_dir_all(tool_root(repository).unwrap());
        let (release, asset) = raw_release("the tool");

        // the pin of another platform's asset says nothing about this one
        let other = pin(repository, version, "tool-plan9-mips", b"another build");
        let pins = Pins::new(vec![other.clone()], false);
        let (stored, downloaded) =
            store_release(repository, &release, "pin-kept", &pins, &mut |_, _| {}).unwrap();
        assert!(downloaded && stored.is_file());
        assert_eq!(
            stored_assets(repository, version),
            [(asset.clone(), hash::sha256_hex(b"the tool"))]
        );
        // a release with no tool.toml has one binary, and no record of others
        assert!(extra_bins(repository, version).is_empty());
        let storage = storage_dir(repository, version).unwrap();
        assert!(!storage.join(BINS_RECORD).exists());
        assert_eq!(fs::read_dir(&storage).unwrap().count(), 2);

        // the lock gains this asset, keeps the other platform's, and drops an old version
        let tool = Tool {
            repository: repository.to_string(),
            version: version.to_string(),
        };
        let old = pin(repository, "0.0.0-older", &asset, b"an older tool");
        let locked = locked_assets([&tool].into_iter(), &[other.clone(), old], false);
        let mine = pin(repository, version, &asset, b"the tool");
        let mut expected = vec![mine.clone(), other.clone()];
        expected.sort();
        assert_eq!(locked, expected);

        // stored already: the same pin passes with no download, another one fails
        let (_, again) = store_release(
            repository,
            &release,
            "pin-kept",
            &Pins::new(vec![mine], false),
            &mut |_, _| {},
        )
        .unwrap();
        assert!(!again);
        let wrong = vec![pin(repository, version, &asset, b"what the lock expected")];
        assert!(matches!(
            store_release(
                repository,
                &release,
                "pin-kept",
                &Pins::new(wrong.clone(), false),
                &mut |_, _| {}
            ),
            Err(Error::ToolLockMismatch { .. })
        ));

        // --refresh takes the stored asset as the new pin
        assert!(
            store_release(
                repository,
                &release,
                "pin-kept",
                &Pins::new(wrong.clone(), true),
                &mut |_, _| {}
            )
            .is_ok()
        );
        let repinned = locked_assets([&tool].into_iter(), &wrong, true);
        assert_eq!(repinned, [pin(repository, version, &asset, b"the tool")]);

        let _ = fs::remove_dir_all(tool_root(repository).unwrap());
    }

    /** a release of `files`, each a name and a body, served by a mock. `{p}`
    in a name becomes the platform of this computer. */
    fn release_of(files: &[(&str, String)]) -> Release {
        let platform = format!("{}-{}", env::consts::OS, env::consts::ARCH);
        let files: Vec<(String, String)> = files
            .iter()
            .map(|(name, body)| (name.replace("{p}", &platform), body.clone()))
            .collect();
        let names: Vec<String> = files.iter().map(|(name, _)| name.clone()).collect();
        let (cdn, _) = mock::serve(move |request| {
            let body = files.iter().find_map(|(name, body)| {
                let line = format!("get /{} ", name.to_lowercase());
                request.starts_with(&line).then_some(body)
            });
            match body {
                Some(body) => mock::reply("200 OK", body),
                None => mock::reply("404 Not Found", ""),
            }
        });
        Release {
            tag_name: "0.0.0-embr-test".to_string(),
            assets: names
                .into_iter()
                .map(|name| http::responses::Asset {
                    browser_download_url: format!("{cdn}/{name}"),
                    name,
                    sha256: None,
                })
                .collect(),
            draft: false,
        }
    }

    fn bins_file(bins: &str) -> (&'static str, String) {
        (TOOL_FILE, format!("[tool]\nbins = [{bins}]\n"))
    }

    /** the Alloy shape: the release has one asset for each binary, and its
    tool.toml names the two binaries. */
    #[test]
    fn a_release_with_bins_stores_each_binary_from_its_asset() {
        let repository = "ember:acme/multi";
        let version = "0.0.0-embr-test";
        let _ = fs::remove_dir_all(tool_root(repository).unwrap());
        let release = release_of(&[
            ("multi-lsp-1.0.0-{p}", "the server".to_string()),
            bins_file("\"multi\", \"multi-lsp\""),
            ("multi-1.0.0-{p}", "the tool".to_string()),
            ("multi-lsp-1.0.0-plan9-mips", "another build".to_string()),
        ]);

        let (stored, downloaded) =
            store_release(repository, &release, "multi", &Pins::none(), &mut |_, _| {}).unwrap();
        assert!(downloaded);
        let storage = storage_dir(repository, version).unwrap();
        assert_eq!(stored, storage.join(executable_name("multi")));
        assert_eq!(fs::read(&stored).unwrap(), b"the tool");
        assert_eq!(
            fs::read(storage.join(executable_name("multi-lsp"))).unwrap(),
            b"the server"
        );
        assert_eq!(extra_bins(repository, version), ["multi-lsp"]);
        // the two executables and the two records, with no folder left from the unpack
        assert_eq!(fs::read_dir(&storage).unwrap().count(), 4);

        // ember.lock pins the tool.toml and the asset of each binary
        let tool = Tool {
            repository: repository.to_string(),
            version: version.to_string(),
        };
        let locked = locked_assets([&tool].into_iter(), &[], false);
        let assets: Vec<&str> = locked.iter().map(|entry| entry.asset.as_str()).collect();
        let platform = format!("{}-{}", env::consts::OS, env::consts::ARCH);
        assert_eq!(
            assets,
            [
                format!("multi-1.0.0-{platform}"),
                format!("multi-lsp-1.0.0-{platform}"),
                TOOL_FILE.to_string()
            ]
        );
        assert_eq!(locked[1].sha256, hash::sha256_hex(b"the server"));

        // a stored copy answers to the pin of each asset
        let again = |pins: Vec<LockedTool>| {
            store_release(
                repository,
                &release,
                "multi",
                &Pins::new(pins, false),
                &mut |_, _| {},
            )
        };
        assert!(!again(locked.clone()).unwrap().1);
        let mut wrong = locked;
        wrong[1].sha256 = hash::sha256_hex(b"what the lock expected");
        assert!(matches!(
            again(wrong.clone()),
            Err(Error::ToolLockMismatch { .. })
        ));

        // and a download of the second binary answers to its pin too
        fs::remove_dir_all(tool_root(repository).unwrap()).unwrap();
        assert!(matches!(again(wrong), Err(Error::ToolLockMismatch { .. })));
        assert!(!storage.exists());
        let _ = fs::remove_dir_all(tool_root(repository).unwrap());
    }

    /// one archive holds each binary, so no binary after the first owns an asset.
    #[test]
    fn a_release_with_bins_takes_each_binary_from_one_archive() {
        let repository = "ember:acme/kit";
        let version = "0.0.0-embr-test";
        let _ = fs::remove_dir_all(tool_root(repository).unwrap());

        let mut builder = tar::Builder::new(Vec::new());
        for (path, body) in [
            ("README.md", "a long text about the two binaries of the kit"),
            (&executable_name("kit")[..], "the tool"),
            (
                &format!("bin/{}", executable_name("kit-fmt"))[..],
                "the formatter",
            ),
        ] {
            let mut header = tar::Header::new_gnu();
            header.set_size(body.len() as u64);
            header.set_mode(0o755);
            header.set_cksum();
            builder
                .append_data(&mut header, path, body.as_bytes())
                .unwrap();
        }
        let archive = String::from_utf8(builder.into_inner().unwrap()).unwrap();

        let release = release_of(&[
            ("kit-{p}.tar", archive.clone()),
            bins_file("\"kit\", \"kit-fmt\""),
        ]);
        let (stored, _) =
            store_release(repository, &release, "kit", &Pins::none(), &mut |_, _| {}).unwrap();
        assert_eq!(fs::read(&stored).unwrap(), b"the tool");
        let storage = storage_dir(repository, version).unwrap();
        assert_eq!(
            fs::read(storage.join(executable_name("kit-fmt"))).unwrap(),
            b"the formatter"
        );
        assert_eq!(extra_bins(repository, version), ["kit-fmt"]);
        fs::remove_dir_all(tool_root(repository).unwrap()).unwrap();

        // a binary that the release does not hold fails the install, and nothing is stored
        let release = release_of(&[("kit-{p}.tar", archive), bins_file("\"kit\", \"kit-lint\"")]);
        let refused = store_release(repository, &release, "kit", &Pins::none(), &mut |_, _| {});
        assert!(matches!(refused, Err(Error::NoExecutableInAsset(_))));
        assert!(!storage.exists());
        let _ = fs::remove_dir_all(tool_root(repository).unwrap());
    }

    #[test]
    fn a_tool_file_that_breaks_a_rule_fails_the_install() {
        let repository = "ember:acme/rules";
        let _ = fs::remove_dir_all(tool_root(repository).unwrap());
        for (bins, why) in [
            ("\"rules\", \"../rules-lsp\"", "binary name"),
            ("\"rules\", \"embx\"", "command of embr"),
            ("\"rules\", \"Rules\"", "twice"),
            ("\"rules-lsp\", \"rules\"", "list it first"),
            ("1", "invalid type"),
        ] {
            let release = release_of(&[("rules-{p}", "the tool".to_string()), bins_file(bins)]);
            let refused =
                store_release(repository, &release, "rules", &Pins::none(), &mut |_, _| {});
            let Err(Error::ToolFileInvalid { reason, .. }) = refused else {
                panic!("{bins} fails: {:?}", refused.err());
            };
            assert!(reason.contains(why), "{bins}: {reason}");
            assert!(!tool_root(repository).unwrap().exists());
        }

        // a tool.toml with no `bins` changes nothing, whatever else it holds
        assert_eq!(
            declared_bins(b"[tool]\nname = \"acme/rules\"\ntags = [\"cli\"]\n"),
            Ok(Vec::new())
        );
        assert_eq!(declared_bins(b""), Ok(Vec::new()));
    }

    #[test]
    fn a_tool_that_npm_installs_gets_no_pin() {
        let tool = Tool::parse("npm:azul-sync@2.4.0").unwrap();
        assert!(locked_assets([&tool].into_iter(), &[], false).is_empty());
    }
}
