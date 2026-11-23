/*! `embr doctor`: one read of what embr depends on, in the groups Ember,
Environment, Project, Registry and Cache. it changes nothing, so a person
can run it on a broken setup, and it prints no token: a login shows as its
name. a thing that a project can do without is `skipped` when it is absent,
never a failure. */

use crate::commands::install;
use crate::error::Error;
use crate::net::{auth, http, offline, registry};
use crate::project::lockfile::Lockfile;
use crate::project::manifest::{DEFAULT_INDEX_URL, Manifest};
use crate::project::workspace::{self, Workspace};
use crate::registry::index;
use crate::sys::paths;
use crate::ui::{self, Stylize};
use clap::Args;
use serde_json::json;
use std::collections::BTreeSet;
use std::path::Path;

#[derive(Args, Debug)]
pub struct DoctorArgs {
    /// Make no request: skip the Registry checks
    #[arg(long)]
    pub offline: bool,
    /// Print the checks as one JSON document on stdout
    #[arg(long)]
    pub json: bool,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Status {
    Ok,
    Warning,
    Failure,
    Skipped,
}

impl Status {
    fn as_str(self) -> &'static str {
        match self {
            Status::Ok => "ok",
            Status::Warning => "warning",
            Status::Failure => "failure",
            Status::Skipped => "skipped",
        }
    }
}

struct Check {
    group: &'static str,
    name: &'static str,
    status: Status,
    detail: String,
    /// what to run. each warning and each failure has one.
    fix: Option<String>,
}

/// the checks of one group, in the order they print.
struct Group {
    name: &'static str,
    checks: Vec<Check>,
}

impl Group {
    fn new(name: &'static str) -> Self {
        Group {
            name,
            checks: Vec::new(),
        }
    }

    fn push(&mut self, name: &'static str, status: Status, detail: impl Into<String>) {
        self.checks.push(Check {
            group: self.name,
            name,
            status,
            detail: detail.into(),
            fix: None,
        });
    }

    fn ok(&mut self, name: &'static str, detail: impl Into<String>) {
        self.push(name, Status::Ok, detail);
    }

    fn skipped(&mut self, name: &'static str, detail: impl Into<String>) {
        self.push(name, Status::Skipped, detail);
    }

    fn warning(&mut self, name: &'static str, detail: impl Into<String>, fix: &str) {
        self.push(name, Status::Warning, detail);
        self.checks.last_mut().expect("just pushed").fix = Some(fix.to_string());
    }

    fn failure(&mut self, name: &'static str, detail: impl Into<String>, fix: &str) {
        self.push(name, Status::Failure, detail);
        self.checks.last_mut().expect("just pushed").fix = Some(fix.to_string());
    }
}

/// runs the checks. false when one is a failure, which exits 1.
pub fn run(args: DoctorArgs) -> Result<bool, Error> {
    let offline = args.offline || offline::is_enabled();
    let manifest = Manifest::load();
    let checks: Vec<Check> = [
        ember(),
        environment(manifest.as_ref().ok()),
        project(&manifest),
        registry(offline),
        cache(),
    ]
    .into_iter()
    .flat_map(|group| group.checks)
    .collect();
    let count = |status| checks.iter().filter(|check| check.status == status).count();
    let passed = count(Status::Failure) == 0;

    if args.json {
        let rows: Vec<_> = checks
            .iter()
            .map(|check| {
                json!({
                    "group": check.group,
                    "name": check.name,
                    "status": check.status.as_str(),
                    "detail": check.detail,
                    "fix": check.fix,
                })
            })
            .collect();
        ui::print_json(&json!({ "version": ui::JSON_VERSION, "checks": rows }))?;
        return Ok(passed);
    }

    let mut group = "";
    for check in &checks {
        if check.group != group {
            if !group.is_empty() {
                println!();
            }
            group = check.group;
            println!("{}", group.bold());
        }
        let mark = match check.status {
            Status::Ok => "✓".with(ui::accent()).to_string(),
            Status::Warning => "!".yellow().to_string(),
            Status::Failure => "✗".red().to_string(),
            Status::Skipped => "-".dark_grey().to_string(),
        };
        // registry and file text reaches the terminal here
        let detail = ui::printable(&check.detail);
        println!("  {mark} {:<10} {detail}", check.name);
        if let Some(fix) = &check.fix {
            println!("    {}", format!("fix: {fix}").dark_grey());
        }
    }
    println!();
    println!(
        "{} ok, {} warning{}, {} failure{}, {} skipped",
        count(Status::Ok),
        count(Status::Warning),
        ui::plural(count(Status::Warning)),
        count(Status::Failure),
        ui::plural(count(Status::Failure)),
        count(Status::Skipped),
    );
    Ok(passed)
}

fn ember() -> Group {
    let mut group = Group::new("Ember");
    group.ok("version", format!("embr {}", env!("CARGO_PKG_VERSION")));
    match paths::ember_dir() {
        Err(error) => group.failure(
            "home",
            error.to_string(),
            "Set HOME to your home folder, then run `embr doctor` again",
        ),
        Ok(dir) if dir.is_dir() => group.ok("home", dir.display().to_string()),
        Ok(dir) => group.ok(
            "home",
            format!(
                "{} is not there yet; embr makes it when it needs it",
                dir.display()
            ),
        ),
    }
    // the shims of the tools sit in ~/.ember/bin, and a shell finds them through PATH
    match paths::bin_dir() {
        Ok(bin) if bin.is_dir() => {
            let on_path = std::env::var_os("PATH").is_some_and(|path| {
                std::env::split_paths(&path).any(|dir| paths::same_file(&dir, &bin))
            });
            if on_path {
                group.ok("path", format!("{} is on PATH", bin.display()));
            } else {
                group.warning(
                    "path",
                    format!(
                        "{} is not on PATH, so a shell does not find the tools",
                        bin.display()
                    ),
                    "Run `embr self install`, then open a new shell",
                );
            }
        }
        _ => group.skipped("path", "no tool is installed, so nothing needs PATH"),
    }
    group
}

/** the first line that `<program> --version` prints, None when the program
does not start. the line as it is, not a parsed version: `git version
2.45.1.windows.1` is no semver version, and git is there all the same. */
fn on_path(program: &str) -> Option<String> {
    let output = std::process::Command::new(program)
        .arg("--version")
        .output()
        .ok()?;
    let text = [output.stdout, output.stderr].concat();
    let line = String::from_utf8_lossy(&text)
        .lines()
        .next()
        .unwrap_or_default()
        .trim()
        .to_string();
    Some(if line.is_empty() {
        format!("{program} is on PATH")
    } else {
        line
    })
}

fn environment(manifest: Option<&Manifest>) -> Group {
    let mut group = Group::new("Environment");
    // the indexes are git clones, so no install resolves without git
    match on_path("git") {
        Some(version) => group.ok("git", version),
        None => group.failure(
            "git",
            "git is not on PATH, and embr reads each index with it",
            "Install git, then run `embr doctor` again",
        ),
    }
    let Some(target) = manifest.and_then(Manifest::target) else {
        group.skipped("runtime", "no project here, so no runtime is needed");
        return group;
    };
    // the programs that this project runs with: its runtime, and the compiler of its language
    let programs = [
        ("runtime", target.environment.runtime()),
        ("compiler", target.language.builder()),
    ];
    for (name, program) in programs {
        match program {
            None if name == "runtime" => group.skipped(
                name,
                format!(
                    "{} runs in Roblox, which needs no program on PATH",
                    target.environment.as_str()
                ),
            ),
            None => group.skipped(
                name,
                format!("{} needs no compiler", target.language.as_str()),
            ),
            Some(program) => match on_path(program) {
                Some(version) => group.ok(name, version),
                None => group.warning(
                    name,
                    format!("{program} is not on PATH"),
                    &format!("Install {program}, or pin it in this project with `embr tool add`"),
                ),
            },
        }
    }
    group
}

fn project(manifest: &Result<Manifest, Error>) -> Group {
    let mut group = Group::new("Project");
    let manifest = match manifest {
        Ok(manifest) => manifest,
        Err(Error::ManifestMissing) => {
            group.skipped("manifest", "no ember.toml in this folder");
            return group;
        }
        Err(error) => {
            group.failure(
                "manifest",
                error.to_string(),
                "Correct ember.toml, then run `embr doctor` again",
            );
            return group;
        }
    };
    group.ok(
        "manifest",
        match manifest.id() {
            Some(id) => format!("ember.toml of {id}"),
            None => "ember.toml reads".to_string(),
        },
    );

    let wants = manifest.all_dependencies().next().is_some();
    let lock = match Lockfile::load() {
        Err(Error::LockfileMissing) if wants => {
            group.warning(
                "lockfile",
                "ember.lock is missing, and ember.toml has dependencies",
                "Run `embr install`",
            );
            None
        }
        Err(Error::LockfileMissing) => {
            group.skipped(
                "lockfile",
                "no ember.lock, and ember.toml has no dependencies",
            );
            None
        }
        Err(error) => {
            group.failure(
                "lockfile",
                error.to_string(),
                "Delete ember.lock, then run `embr install`",
            );
            None
        }
        Ok(lock) => {
            let count = lock.packages.len();
            match &lock.manifest_hash {
                Some(hash) if *hash != manifest.dependency_hash() => group.warning(
                    "lockfile",
                    "ember.toml asks for other dependencies than the ones ember.lock was written for",
                    "Run `embr install`",
                ),
                _ => group.ok(
                    "lockfile",
                    format!("ember.lock matches ember.toml, {count} package{}", ui::plural(count)),
                ),
            }
            Some(lock)
        }
    };

    match &lock {
        None => group.skipped("install", "no ember.lock to compare with"),
        Some(lock) => installed(&mut group, manifest, lock),
    }

    if manifest.workspace_members().is_empty() {
        match workspace::containing(Path::new(".")) {
            Ok(Some(workspace)) => group.ok(
                "workspace",
                format!("a member of the workspace in {}", workspace.root.display()),
            ),
            Ok(None) => group.skipped("workspace", "this project is in no workspace"),
            Err(error) => group.failure(
                "workspace",
                error.to_string(),
                "Correct the ember.toml of the workspace root, then run `embr doctor` again",
            ),
        }
    } else {
        match Workspace::open(Path::new(".")).and_then(|workspace| {
            workspace.order()?;
            Ok(workspace.members.len())
        }) {
            Ok(count) => group.ok("workspace", format!("{count} member{}", ui::plural(count))),
            Err(error) => group.failure(
                "workspace",
                error.to_string(),
                "Correct the ember.toml files of the workspace, then run `embr doctor` again",
            ),
        }
    }
    group
}

/** whether the packages folders hold what ember.lock names: each package
has its folder, and the stamp of the last install is the stamp of the
files as they are now. */
fn installed(group: &mut Group, manifest: &Manifest, lock: &Lockfile) {
    let missing = lock
        .packages
        .iter()
        .filter(|package| {
            !install::stored_at(manifest, package).is_ok_and(|(storage, _)| storage.is_dir())
        })
        .count();
    if missing > 0 {
        group.warning(
            "install",
            format!(
                "{missing} of {} package{} in ember.lock {} not installed",
                lock.packages.len(),
                ui::plural(lock.packages.len()),
                if missing == 1 { "is" } else { "are" }
            ),
            "Run `embr install`",
        );
        return;
    }
    /* an install writes one stamp for each environment. a workspace member
    gets its stamp without the global tools, so either hash is a match */
    let hashes = [true, false].map(install::current_state_hash);
    let environments: BTreeSet<_> = lock
        .packages
        .iter()
        .map(|package| package.context())
        .collect();
    let stale = environments.into_iter().any(|environment| {
        let stamp = manifest
            .packages_out(environment)
            .join(install::STORE_DIR)
            .join(install::STATE_FILE);
        // no stamp is no evidence: a workspace install writes none
        std::fs::read_to_string(stamp)
            .is_ok_and(|stamp| !hashes.iter().flatten().any(|hash| *hash == stamp))
    });
    if stale {
        group.warning(
            "install",
            "ember.toml, ember.lock, a patch or the global tools changed after the last install",
            "Run `embr install`",
        );
    } else {
        group.ok("install", "each package in ember.lock is installed");
    }
}

fn registry(offline: bool) -> Group {
    let mut group = Group::new("Registry");
    if offline {
        group.skipped("api", "offline");
        group.skipped("login", "offline");
        return group;
    }
    let api = registry::api_url();
    // EMBER_API_URL can hold a password, and this report goes into bug reports
    let shown = registry::without_userinfo(&api);
    // an answer with an error status still shows that the host is there
    let reachable = match http::agent().get(&api).call() {
        Ok(_) | Err(ureq::Error::Status(..)) => true,
        Err(error) => {
            let error = error.to_string().replace(&api, &shown);
            group.failure(
                "api",
                format!("{shown} does not answer: {error}"),
                "Check the network, then run `embr doctor` again; `embr doctor --offline` leaves this group out",
            );
            false
        }
    };
    if reachable {
        group.ok("api", format!("{shown} answers"));
    }

    /* the token as it is stored. `auth::token` trades an old GitHub token
    for a new one and saves it, and this command changes nothing */
    let from_env = auth::env_token();
    let stored = match &from_env {
        Some(_) => None,
        None => auth::load().ok().flatten(),
    };
    let token = from_env
        .as_deref()
        .or(stored.as_ref().map(|stored| stored.token.as_str()));
    let source = if from_env.is_some() {
        auth::TOKEN_VAR
    } else {
        "the stored login"
    };
    match token {
        None => group.skipped("login", "not logged in, which only a publish needs"),
        Some(token) if auth::check_token(token, source).is_err() => group.failure(
            "login",
            format!(
                "{source} holds a character that no token has, such as a space or a line break"
            ),
            "Run `embr auth login`, or set a new EMBER_TOKEN",
        ),
        Some(_) if !reachable => group.skipped("login", "the API does not answer"),
        Some(token) if !token.starts_with(auth::TOKEN_PREFIX) => group.warning(
            "login",
            "the stored login is from embr 0.9",
            "Run `embr auth login`",
        ),
        Some(token) => match auth::check(&api, token) {
            Ok(credentials) => {
                group.ok("login", format!("{} from {source}", credentials.login));
            }
            Err(Error::TokenRejected) => group.failure(
                "login",
                format!("the registry refuses the token from {source}"),
                "Run `embr auth login`, or set a new EMBER_TOKEN",
            ),
            Err(error) => group.warning(
                "login",
                format!("the token from {source} did not check: {error}"),
                "Run `embr doctor` again; if it stays, run `embr auth login`",
            ),
        },
    }
    group
}

fn cache() -> Group {
    let mut group = Group::new("Cache");
    let archives = index::cached_archives();
    let bad = archives
        .iter()
        .filter(|archive| index::check_archive(&archive.path).bad.is_some())
        .count();
    let count = archives.len();
    if bad > 0 {
        group.warning(
            "archives",
            format!(
                "{bad} of {count} archive{} did not pass the check; `embr cache verify` lists them",
                ui::plural(count)
            ),
            "Run `embr cache clean --bad`",
        );
    } else if count == 0 {
        group.skipped("archives", "no archive is cached");
    } else {
        group.ok(
            "archives",
            format!("{count} archive{} pass the check", ui::plural(count)),
        );
    }

    let cloned = index::cache_dir(DEFAULT_INDEX_URL).is_ok_and(|dir| dir.join(".git").exists());
    if !cloned {
        group.skipped(
            "index",
            "the ember index is not cloned yet; the first install clones it",
        );
    } else if index::is_fresh(DEFAULT_INDEX_URL) {
        group.ok("index", "the copy of the ember index is fresh");
    } else {
        group.ok(
            "index",
            "the copy of the ember index is past its refresh time; the next install refreshes it",
        );
    }
    group
}
