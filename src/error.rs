use crate::project::manifest::Environment;
use std::path::PathBuf;
use thiserror::Error;

/// "1 package" / "3 packages" for the workspace publish error.
fn ui_plural_packages(count: usize) -> String {
    format!("{count} package{}", if count == 1 { "" } else { "s" })
}

#[derive(Debug, Error)]
pub enum Error {
    #[error("An lpm.toml manifest already exists in this directory")]
    ManifestExists,

    #[error("Could not determine your home directory")]
    NoHomeDir,

    #[error("lpm is not installed ({} does not exist)", .0.display())]
    NotInstalled(PathBuf),

    #[error("No releases have been published for {0} yet")]
    NoReleases(String),

    #[error("Release {1} for repository {0} doesn't exist")]
    NoSuchRelease(String, String),

    #[error("Release v{version} has no asset named {asset} for this platform")]
    MissingAsset {
        version: semver::Version,
        asset: String,
    },

    #[error("GitHub authorization failed: {0}")]
    AuthFailed(String),

    /** the registry refused a publish. `message` is the human-readable
    `error` field from the API's response body, like a bad token, a scope
    owned by someone else, or a version that already exists. */
    #[error("Publishing failed: {message}")]
    PublishFailed { status: u16, message: String },

    #[error(
        "Package archive is {size_mb:.1} MB; the registry accepts at most {limit_mb} MB. Trim it with `includes`/`excludes` under [target]"
    )]
    PublishTooLarge { size_mb: f64, limit_mb: u64 },

    #[error("Index {0} does not accept publishes (its config.toml has no github_oauth_id)")]
    PublishNotSupported(String),

    #[error("No workspace member is named '{0}'")]
    NoWorkspaceMember(String),

    #[error(
        "'{0}' is a workspace dependency, but this project is not part of a workspace (no ancestor lpm.toml lists it under [target] workspace)"
    )]
    NotInWorkspace(String),

    #[error("Workspace member {} has no lpm.toml", .0.display())]
    WorkspaceMemberMissingManifest(PathBuf),

    #[error("Invalid workspace glob '{glob}': {reason}")]
    WorkspaceGlobInvalid { glob: String, reason: String },

    #[error("Publishing failed for {}: {}", ui_plural_packages(.0.len()), .0.join(", "))]
    WorkspacePublishFailed(Vec<String>),

    #[error("Invalid [patches] key '{key}': {reason}")]
    PatchKeyInvalid { key: String, reason: String },

    #[error(
        "Patch path '{path}' for {key} must be a relative path inside the project (patches travel with the repo)"
    )]
    PatchPathInvalid { key: String, path: String },

    #[error(
        "Patch file {path} for {key} is missing; re-run `lpm patch` and commit it, or remove the [patches] entry"
    )]
    PatchFileMissing { key: String, path: String },

    #[error(
        "Patch for {name}@{version} does not apply: {reason}. Re-run `lpm patch {name}` and commit an updated patch, or remove the [patches] entry"
    )]
    PatchDrift {
        name: String,
        version: String,
        reason: String,
    },

    #[error(
        "{name}@{version} resolved to more than one target in this install ({first}, {second}); patching multi-target packages isn't supported"
    )]
    PatchMultiTarget {
        name: String,
        version: String,
        first: String,
        second: String,
    },

    #[error("Patch {patch} failed to apply to {name}: {stderr}")]
    PatchApplyFailed {
        name: String,
        patch: String,
        stderr: String,
    },

    #[error(
        "Patch {path} recorded in lpm.lock for {name} {reason}; run `lpm install` without --locked to re-lock it"
    )]
    PatchLockInvalid {
        name: String,
        path: String,
        reason: &'static str,
    },

    #[error(
        "A working copy for {key} already exists at {path}; pass --force to start over from the published bytes"
    )]
    PatchCopyExists { key: String, path: String },

    #[error("No patch working copy for '{0}' under .lpm-patch/; run `lpm patch {0}` first")]
    PatchCopyMissing(String),

    #[error("'{spec}' matches more than one {what} ({matches}); add @version to pick one")]
    PatchAmbiguous {
        spec: String,
        what: &'static str,
        matches: String,
    },

    #[error(
        "Patching needs git on PATH (it diffs and applies the patch files); install git and retry"
    )]
    GitMissing,

    #[error("Invalid spec '{spec}': {reason}")]
    PatchSpecInvalid { spec: String, reason: String },

    #[error("Git {action} failed: {stderr}")]
    PatchGitFailed {
        action: &'static str,
        stderr: String,
    },

    #[error("No lpm.toml manifest found in the current directory")]
    ManifestMissing,

    #[error("No tool exists with name '{0}'")]
    ToolMissing(String),

    #[error("Invalid tool spec '{0}': expected 'owner/repo@version'")]
    InvalidToolSpec(String),

    #[error("No release asset of {tool}@{version} matches this platform")]
    NoMatchingAsset { tool: String, version: String },

    #[error("No executable found in the downloaded release of {0}")]
    NoExecutableInAsset(String),

    #[error(
        "Tool '{0}' is not managed in this directory; run `lpm tool add {0}` here or add it globally with --global"
    )]
    ToolNotManaged(String),

    #[error("Tool '{0}' is not installed; run `lpm install` to install it")]
    ToolNotInstalled(String),

    #[error("Alias '{0}' is reserved for lpm itself and can't name a tool")]
    ReservedToolAlias(String),

    #[error("Invalid spec '{0}': expected a name or 'owner/repo', optionally with '@version'")]
    ExecuteSpecInvalid(String),

    #[error(
        "'{alias}' is pinned to {pinned} here; drop '@{requested}' to run the pin, or use the full owner/repo form for another version"
    )]
    ExecutePinConflict {
        alias: String,
        pinned: String,
        requested: String,
    },

    #[error(
        "'{0}' is not a pinned tool alias or a known shorthand; use the full 'owner/repo' GitHub name"
    )]
    ExecuteUnknownName(String),

    #[error("Invalid package name '{0}': expected 'scope/name' (lowercase)")]
    InvalidPackageName(String),

    #[error("Package {name} was not found in index {index}")]
    PackageNotFound { name: String, index: String },

    #[error("No version of {name} matches requirement '{req}'")]
    NoMatchingVersion { name: String, req: String },

    #[error("Index '{0}' is not defined under [indices] in lpm.toml")]
    UnknownIndex(String),

    #[error("Unsupported environment '{0}'")]
    UnsupportedEnvironment(String),

    #[error(
        "Could not determine the environment of {0}; set `environment` under [target] in lpm.toml"
    )]
    UnknownPackageEnvironment(String),

    #[error(
        "Dependency conflict in the {context} tree: {name} is required as both '{first}' and '{second}'"
    )]
    DependencyConflict {
        name: String,
        context: Environment,
        first: String,
        second: String,
    },

    #[error(
        "No [package] table in lpm.toml; publishing needs one with a name and version (run `lpm init` and choose 'package', or add it by hand)"
    )]
    PackageMissing,

    #[error(
        "[config] points {first}-packages-out and {second}-packages-out at the same folder ({path}); environment roots must stay distinct"
    )]
    PackagesOutCollision {
        first: Environment,
        second: Environment,
        path: String,
    },

    #[error("Failed to fetch index {url}: {reason}")]
    IndexFetch { url: String, reason: String },

    #[error("lpm.lock is missing; run `lpm install` without --locked to create it")]
    LockfileMissing,

    #[error(
        "lpm.lock is version {0}, from before self-contained environment roots; run `lpm install` once without --locked to regenerate it"
    )]
    LockfileOutdated(u32),

    #[error("Invalid lpm.toml: {0}")]
    ManifestInvalid(String),

    #[error("No script named '{0}' under [scripts] in lpm.toml")]
    ScriptMissing(String),

    /** a `pre<event>`/`post<event>` script exited non-zero. only hooks land
    here. a script named on the command line exits with its own code and no
    message of ours, since the user already knows which one they ran. */
    #[error("Script '{hook}' failed with exit code {code}")]
    HookFailed { hook: String, code: i32 },

    #[error(
        "[overrides] key '{0}' must be a dot-separated path like 'parent.child' (commas separate multiple paths)"
    )]
    OverrideBadPath(String),

    #[error("Override '{path}' points at '{alias}', which is not under [dependencies]")]
    OverrideAliasMissing { path: String, alias: String },

    #[error("Two [overrides] keys both cover the path '{0}'")]
    OverrideDuplicatePath(String),

    #[error("No [studio] table in lpm.toml; run `lpm studio init` to create one")]
    StudioMissing,

    #[error(
        "A [studio] table already exists in lpm.toml; edit it there, or remove it and re-run `lpm studio init`"
    )]
    StudioExists,

    #[error("[studio] in lpm.toml sets both `file` and `universe`/`place`; remove one of the two")]
    StudioConflict,

    #[error(
        "[studio] in lpm.toml sets `{has}` but not `{needs}`; opening a published place needs both IDs"
    )]
    StudioIncomplete {
        has: &'static str,
        needs: &'static str,
    },

    #[error("[studio] in lpm.toml is empty; run `lpm studio init` to fill it in")]
    StudioUnconfigured,

    #[error("`{0}` under [studio] in lpm.toml must be a non-zero ID")]
    StudioInvalidId(&'static str),

    #[error(
        "Unknown key `{0}` under [studio] in lpm.toml; expected `universe`, `place`, or `file`"
    )]
    StudioUnknownKey(String),

    #[error("`file` under [studio] in lpm.toml is empty; point it at a .rbxl or .rbxlx place file")]
    StudioEmptyFile,

    #[error("Place file {0} does not exist")]
    StudioFileMissing(String),

    #[error("Place file {0} is not a .rbxl or .rbxlx file")]
    StudioFileNotAPlace(String),

    #[error("Place file {0} is a folder, not a file")]
    StudioFileIsFolder(String),

    #[error("Roblox Studio doesn't appear to be installed: {0}")]
    StudioNotInstalled(String),

    #[error("Could not launch Roblox Studio: {0}")]
    StudioLaunch(String),

    #[error("VS Code was not found ({0} were checked under the standard user config folders)")]
    VscodeNotFound(String),

    #[error("Could not update {}: {reason}", .path.display())]
    VscodeSettings { path: PathBuf, reason: String },

    #[error(transparent)]
    Io(#[from] std::io::Error),

    #[error(transparent)]
    TomlDeserialize(#[from] toml::de::Error),

    #[error(transparent)]
    TomlEdit(#[from] toml_edit::TomlError),

    #[error(transparent)]
    Json(#[from] serde_json::Error),

    #[error(transparent)]
    Zip(#[from] zip::result::ZipError),

    #[error(transparent)]
    Prompt(#[from] inquire::InquireError),

    #[error(transparent)]
    TomlSerialize(#[from] toml::ser::Error),

    #[error(transparent)]
    Semver(#[from] semver::Error),

    #[error(transparent)]
    Http(#[from] crate::net::http::error::HttpError),
}

impl Error {
    /** The status lpm should exit with. 1 for anything lpm itself decided,
    but a hook failure forwards the child's own code, so a `preinstall` that
    exits 3 makes `lpm install` exit 3 and CI reads the real reason rather
    than a flattened "something went wrong". */
    pub fn exit_code(&self) -> i32 {
        match self {
            Error::HookFailed { code, .. } if *code != 0 => *code,
            _ => 1,
        }
    }
}
