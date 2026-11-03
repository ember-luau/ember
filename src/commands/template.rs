/*! `embr template`: project templates. a template is a project folder.
`save` keeps a copy of the current project under ~/.ember/templates,
`embr init --template` copies a template into a new project, and `publish`
puts a saved template on GitHub as a template repository.

a saved template is a folder with two entries:

```text
~/.ember/templates/<name>/template.toml   source = "<the project it came from>"
~/.ember/templates/<name>/files/          the files, as `embr init` copies them
``` */

use crate::error::Error;
use crate::net::github_template::{self as github, API, NewRepository};
use crate::project::manifest::{Environment, MANIFEST_FILE, Manifest, is_github_username};
use crate::registry::pack::tar_path;
use crate::sys::{git, paths};
use crate::ui;
use clap::Subcommand;
use globset::{Glob, GlobBuilder, GlobMatcher, GlobSet, GlobSetBuilder};
use serde::{Deserialize, Serialize};
use std::fs;
use std::io::IsTerminal;
use std::path::{Component, Path, PathBuf};

#[derive(Subcommand, Debug)]
pub enum TemplateCommand {
    /// Save the current project as a template
    Save {
        /// Name of the template: letters, digits, `-` and `_`
        name: String,

        /// Replace a saved template of the same name
        #[arg(long)]
        force: bool,
    },

    /// List the saved templates
    #[command(visible_alias = "ls")]
    List,

    /// Show the files of a saved template and the project it came from
    Show {
        /// Name of the template
        name: String,
    },

    /// Delete a saved template
    #[command(visible_alias = "rm")]
    Remove {
        /// Name of the template
        name: String,
    },

    /// Create a GitHub template repository from a saved template
    Publish {
        /// Name of the saved template
        name: String,

        /// Make the repository private; it is public by default
        #[arg(long)]
        private: bool,

        /// The repository to create, as owner/name; by default your account and the template name
        #[arg(long)]
        repo: Option<String>,

        /// Create the repository without asking first
        #[arg(short, long)]
        yes: bool,
    },
}

pub fn run(command: TemplateCommand) -> Result<(), Error> {
    match command {
        TemplateCommand::Save { name, force } => save(&name, force),
        TemplateCommand::List => list(),
        TemplateCommand::Show { name } => show(&name),
        TemplateCommand::Remove { name } => remove(&name),
        TemplateCommand::Publish {
            name,
            private,
            repo,
            yes,
        } => publish(&name, private, repo.as_deref(), yes),
    }
}

/// what template.toml records about a saved template.
#[derive(Serialize, Deserialize)]
struct Meta {
    /// the folder of the project that `save` copied.
    source: String,
}

/// a name `save` takes. no dot, so a name never ends in the `.tmp` of staging.
fn check_name(name: &str) -> Result<(), Error> {
    let valid = !name.is_empty()
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_');
    match valid {
        true => Ok(()),
        false => Err(Error::Template(format!(
            "'{name}' is not a template name; use letters, digits, `-` and `_`"
        ))),
    }
}

/// the folder of the saved template `name`, which must exist.
fn saved_dir(name: &str) -> Result<PathBuf, Error> {
    let dir = paths::templates_dir()?.join(name);
    if check_name(name).is_err() || !dir.join("files").is_dir() {
        return Err(Error::Template(format!(
            "No template is named '{name}'; `embr template list` shows the saved ones"
        )));
    }
    Ok(dir)
}

/// the names of the saved templates, sorted.
pub fn saved() -> Result<Vec<String>, Error> {
    let Ok(entries) = fs::read_dir(paths::templates_dir()?) else {
        return Ok(Vec::new());
    };
    let mut names: Vec<String> = entries
        .flatten()
        .filter(|entry| entry.path().join("files").is_dir())
        .filter_map(|entry| entry.file_name().into_string().ok())
        .filter(|name| check_name(name).is_ok())
        .collect();
    names.sort();
    Ok(names)
}

fn save(name: &str, force: bool) -> Result<(), Error> {
    check_name(name)?;
    // a template is a project, so it starts at an ember.toml
    let manifest = Manifest::load()?;
    let root = std::env::current_dir()?;
    let out_dirs: Vec<PathBuf> = Environment::ALL
        .into_iter()
        .map(|environment| manifest.packages_out(environment))
        .collect();
    let selection = select(&root, &out_dirs)?;

    let dir = paths::templates_dir()?.join(name);
    if dir.exists() && !force {
        return Err(Error::Template(format!(
            "A template named '{name}' exists; pass --force to replace it"
        )));
    }
    // staged beside the template, so a failed copy never leaves half of one
    let staging = paths::with_suffix(&dir, ".tmp");
    let _ = fs::remove_dir_all(&staging);
    copy(&root, &selection.kept, &staging.join("files"))?;
    let meta = Meta {
        source: root.display().to_string(),
    };
    fs::write(staging.join("template.toml"), toml::to_string(&meta)?)?;
    if dir.exists() {
        fs::remove_dir_all(&dir)?;
    }
    fs::rename(&staging, &dir)?;

    let count = selection.kept.len();
    ui::print_success(&format!(
        "Saved template {name} with {count} file{}",
        ui::plural(count)
    ));
    if !selection.secrets.is_empty() {
        println!(
            "Left out {} secret file{}: {}",
            selection.secrets.len(),
            ui::plural(selection.secrets.len()),
            selection.secrets.join(", ")
        );
    }
    if !selection.left_out.is_empty() {
        println!("Left out: {}", selection.left_out.join(", "));
    }
    Ok(())
}

fn list() -> Result<(), Error> {
    let names = saved()?;
    if names.is_empty() {
        println!("No templates saved. Save one with `embr template save <name>`");
        return Ok(());
    }
    println!("Templates in {}", paths::templates_dir()?.display());
    for name in names {
        let source = meta(&paths::templates_dir()?.join(&name)).map(|meta| meta.source);
        ui::print_script_entry(&name, &[source.unwrap_or_default()]);
    }
    Ok(())
}

/// the template.toml of the template in `dir`. None when it is missing or broken.
fn meta(dir: &Path) -> Option<Meta> {
    toml::from_str(&fs::read_to_string(dir.join("template.toml")).ok()?).ok()
}

fn show(name: &str) -> Result<(), Error> {
    let dir = saved_dir(name)?;
    let files = files_in(&dir.join("files"))?;
    println!("Template {name}");
    if let Some(meta) = meta(&dir) {
        println!("Source: {}", meta.source);
    }
    println!("Folder: {}", dir.join("files").display());
    println!("Files ({}):", files.len());
    for file in files {
        println!("  {}", tar_path(&file));
    }
    Ok(())
}

fn remove(name: &str) -> Result<(), Error> {
    fs::remove_dir_all(saved_dir(name)?)?;
    ui::print_success(&format!("Removed template {name}"));
    Ok(())
}

/// every file under `root`, relative and sorted. a saved template has no other filter.
fn files_in(root: &Path) -> Result<Vec<PathBuf>, Error> {
    let mut files = Vec::new();
    collect(root, root, &mut files)?;
    files.sort();
    Ok(files)
}

fn collect(dir: &Path, root: &Path, files: &mut Vec<PathBuf>) -> Result<(), Error> {
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        if entry.file_type()?.is_dir() {
            collect(&path, root, files)?;
        } else {
            files.push(path.strip_prefix(root).expect("under root").to_path_buf());
        }
    }
    Ok(())
}

fn publish(name: &str, private: bool, repo: Option<&str>, yes: bool) -> Result<(), Error> {
    let files_dir = saved_dir(name)?.join("files");
    let files = files_in(&files_dir)?;
    let token = github::token().ok_or(Error::GithubTokenMissing)?;
    let login = github::login(API, &token)?;
    let (owner, repo_name) = match repo {
        Some(repo) => repo
            .split_once('/')
            .filter(|(owner, name)| is_github_username(owner) && is_repo_name(name))
            .ok_or_else(|| {
                Error::Template(format!(
                    "'{repo}' is not a GitHub repository; write owner/name"
                ))
            })?,
        None => (login.as_str(), name),
    };

    let visibility = if private { "private" } else { "public" };
    println!(
        "These {} file{} go to the new {visibility} template repository {owner}/{repo_name}:",
        files.len(),
        ui::plural(files.len())
    );
    for file in &files {
        println!("  {}", tar_path(file));
    }
    if !yes {
        if !std::io::stdin().is_terminal() {
            return Err(Error::Template(
                "publish asks before it creates a repository; pass --yes to skip the question"
                    .to_string(),
            ));
        }
        inquire::set_global_render_config(ui::render_config());
        let create = inquire::Confirm::new("Create the repository?")
            .with_default(false)
            .prompt()?;
        if !create {
            println!("Nothing was created");
            return Ok(());
        }
    }

    // git pushes into an empty repository. the API needs a first commit
    let use_git = git::available();
    let created = github::create(
        API,
        &token,
        &login,
        &NewRepository {
            owner,
            name: repo_name,
            private,
            auto_init: !use_git,
        },
    )?;
    let pushed = if use_git {
        let remote = format!("https://github.com/{}.git", created.full_name);
        github::push_with_git(
            &remote,
            Some(&token),
            &files_dir,
            &created.default_branch,
            &login,
        )
    } else {
        github::push(
            API,
            &token,
            &created.full_name,
            &created.default_branch,
            &files_dir,
            &files,
        )
    };
    if let Err(error) = pushed {
        return Err(Error::Template(format!(
            "Created {}, but the push failed: {error}. Delete the repository on GitHub, then publish again",
            created.html_url
        )));
    }
    ui::print_success(&format!(
        "Published template {name} to {}",
        created.html_url
    ));
    println!(
        "Start a project from it with `embr init --template {}`",
        created.full_name
    );
    Ok(())
}

/// the shape of a GitHub repository name.
fn is_repo_name(name: &str) -> bool {
    !name.is_empty()
        && name != "."
        && name != ".."
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
}

/** `owner/repo` or `owner/repo#ref`, the GitHub form of `--template`. the
ref is a branch, a tag, or a commit. */
pub fn github_spec(spec: &str) -> Option<(&str, Option<&str>)> {
    let (repository, reference) = match spec.split_once('#') {
        Some((repository, reference)) => (repository, Some(reference)),
        None => (spec, None),
    };
    let (owner, name) = repository.split_once('/')?;
    let valid_reference = reference.is_none_or(|reference| {
        !reference.is_empty()
            && !reference.contains("..")
            && reference
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | '/'))
    });
    (is_github_username(owner) && is_repo_name(name) && valid_reference)
        .then_some((repository, reference))
}

/// a template that [`fetch`] found and checked, ready to write.
pub struct Fetched {
    /// the text of the ember.toml at the root of the template.
    pub manifest: String,
    files: Files,
}

enum Files {
    /// these files, relative to this folder.
    Folder(PathBuf, Vec<PathBuf>),
    /// a GitHub tarball whose entries all passed [`entries`].
    Tarball(Vec<u8>),
}

/** Finds the template `spec` for `dest`, which must be empty but for a
`.git` folder. `spec` is a saved template's name, then a folder, then a
GitHub `owner/repo[#ref]`. nothing is written yet, so `embr init` can ask
its questions first.

A public repository downloads with no token. on a 404 embr tries again
with the GitHub token, for a private one. */
pub fn fetch(spec: &str, dest: &Path) -> Result<Fetched, Error> {
    let busy = fs::read_dir(dest)?
        .flatten()
        .any(|entry| entry.file_name() != ".git");
    if busy {
        return Err(Error::Template(
            "embr init --template needs an empty folder; this one has files (a .git folder is fine)"
                .to_string(),
        ));
    }

    let saved = check_name(spec)
        .ok()
        .map(|()| paths::templates_dir())
        .transpose()?
        .map(|dir| dir.join(spec).join("files"))
        .filter(|files| files.is_dir());
    let folder = saved.or_else(|| Some(PathBuf::from(spec)).filter(|path| path.is_dir()));
    if let Some(root) = folder {
        let selection = select(&root, &[])?;
        if !selection
            .kept
            .iter()
            .any(|file| file == Path::new(MANIFEST_FILE))
        {
            return Err(no_manifest());
        }
        return Ok(Fetched {
            manifest: fs::read_to_string(root.join(MANIFEST_FILE))?,
            files: Files::Folder(root, selection.kept),
        });
    }

    let Some((repository, reference)) = github_spec(spec) else {
        return Err(Error::Template(format!(
            "No template is named '{spec}', and it is not a folder or a GitHub owner/repo"
        )));
    };
    let tarball = match github::tarball(API, repository, reference, None) {
        Err(Error::Http(crate::net::http::error::HttpError::NotFound)) => {
            let token = github::token().ok_or_else(|| {
                Error::Template(format!(
                    "GitHub has no public repository {repository}; for a private one, log in with `gh auth login`, or set GH_TOKEN or GITHUB_TOKEN"
                ))
            })?;
            github::tarball(API, repository, reference, Some(&token)).map_err(
                |error| match error {
                    Error::Http(crate::net::http::error::HttpError::NotFound) => Error::Template(
                        format!("GitHub has no repository {repository} that your token can read"),
                    ),
                    other => other,
                },
            )?
        }
        other => other?,
    };
    Fetched::from_tarball(tarball)
}

impl Fetched {
    /** checks every entry of a GitHub tarball. a refused archive is an
    error here, before anything is written. */
    fn from_tarball(tarball: Vec<u8>) -> Result<Self, Error> {
        let manifest = entries(&tarball, None)?.manifest.ok_or_else(no_manifest)?;
        Ok(Fetched {
            manifest,
            files: Files::Tarball(tarball),
        })
    }

    /// writes the files into `dest`, and returns how many.
    pub fn write(&self, dest: &Path) -> Result<usize, Error> {
        match &self.files {
            Files::Folder(root, files) => {
                copy(root, files, dest)?;
                Ok(files.len())
            }
            Files::Tarball(tarball) => Ok(entries(tarball, Some(dest))?.files),
        }
    }
}

fn no_manifest() -> Error {
    Error::Template("The template has no ember.toml at its root".to_string())
}

/// copies each of `files`, relative to `from`, to the same place under `to`.
fn copy(from: &Path, files: &[PathBuf], to: &Path) -> Result<(), Error> {
    for file in files {
        let target = to.join(file);
        if let Some(parent) = target.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::copy(from.join(file), target)?;
    }
    fs::create_dir_all(to)?;
    Ok(())
}

/// what a template takes from a project folder, and what it leaves out.
#[derive(Default, Debug)]
struct Selection {
    /// the files to copy, relative and sorted.
    kept: Vec<PathBuf>,
    /// files that match a secret pattern.
    secrets: Vec<String>,
    /// everything else left out: a folder with a trailing `/`, or a file.
    left_out: Vec<String>,
}

/// folders that hold tool data, at any depth.
const TOOL_DIRS: [&str; 8] = [
    ".git",
    ".ember",
    ".ember-staging",
    ".ember-patch",
    ".alloy",
    ".larvae",
    "node_modules",
    "target",
];

/** folders of installed packages and build output, at the root only. a
`build` folder deeper down is more likely source. compared without case. */
const ROOT_DIRS: [&str; 9] = [
    "packages",
    "serverpackages",
    "devpackages",
    "roblox_packages",
    "lune_packages",
    "luau_packages",
    "build",
    "dist",
    "out",
];

/// the lockfiles of embr and other tools. a new project resolves its own.
const LOCKFILES: [&str; 5] = [
    "*.lock",
    "*.lockb",
    "package-lock.json",
    "npm-shrinkwrap.json",
    "pnpm-lock.yaml",
];

/// file names that hold secrets. `.npmrc` counts only when it holds a token.
const SECRETS: [&str; 11] = [
    ".env*",
    "*.pem",
    "*.key",
    "*.p12",
    "*.pfx",
    "id_rsa*",
    "id_dsa*",
    "id_ecdsa*",
    "id_ed25519*",
    ".netrc",
    ".pypirc",
];

/// the compiled filters of one [`select`].
struct Filters {
    out_dirs: Vec<PathBuf>,
    lockfiles: GlobSet,
    secrets: GlobSet,
    ignore: Vec<Rule>,
}

fn glob_set(patterns: &[&str]) -> GlobSet {
    let mut set = GlobSetBuilder::new();
    for pattern in patterns {
        set.add(Glob::new(pattern).expect("the pattern is a constant"));
    }
    set.build().expect("the patterns are constants")
}

/** picks the files of `root` that a template keeps. `out_dirs` are the
packages folders that the project's [config] names. */
fn select(root: &Path, out_dirs: &[PathBuf]) -> Result<Selection, Error> {
    let filters = Filters {
        out_dirs: out_dirs
            .iter()
            .map(|dir| {
                dir.components()
                    .filter(|part| !matches!(part, Component::CurDir))
                    .collect()
            })
            .collect(),
        lockfiles: glob_set(&LOCKFILES),
        secrets: glob_set(&SECRETS),
        ignore: gitignore(root),
    };
    let mut selection = Selection::default();
    walk(root, root, &filters, &mut selection)?;
    selection.kept.sort();
    Ok(selection)
}

fn walk(
    dir: &Path,
    root: &Path,
    filters: &Filters,
    selection: &mut Selection,
) -> Result<(), Error> {
    let mut entries = fs::read_dir(dir)?.collect::<Result<Vec<_>, _>>()?;
    entries.sort_by_key(|entry| entry.file_name());
    for entry in entries {
        let path = entry.path();
        let relative = path.strip_prefix(root).expect("under root").to_path_buf();
        let name = entry.file_name().to_string_lossy().into_owned();
        let kind = entry.file_type()?;

        if kind.is_dir() {
            let at_root = relative.components().count() == 1;
            let left_out = TOOL_DIRS.contains(&name.as_str())
                || (at_root && ROOT_DIRS.iter().any(|dir| name.eq_ignore_ascii_case(dir)))
                || filters.out_dirs.contains(&relative)
                || ignored(&filters.ignore, &relative, true);
            match left_out {
                true => selection.left_out.push(format!("{}/", tar_path(&relative))),
                false => walk(&path, root, filters, selection)?,
            }
        } else if filters.secrets.is_match(&name) || (name == ".npmrc" && holds_token(&path)) {
            selection.secrets.push(tar_path(&relative));
        } else if !kind.is_file()
            || filters.lockfiles.is_match(&name)
            || ignored(&filters.ignore, &relative, false)
        {
            // a symlink lands here too: a template is plain files
            selection.left_out.push(tar_path(&relative));
        } else {
            selection.kept.push(relative);
        }
    }
    Ok(())
}

/// whether an `.npmrc` holds a registry token or password.
fn holds_token(path: &Path) -> bool {
    fs::read_to_string(path).is_ok_and(|text| text.contains("_auth") || text.contains("_password"))
}

/// one line of a .gitignore: the glob, whether it starts with `!`, and whether it ends with `/`.
type Rule = (GlobMatcher, bool, bool);

/** the rules of the root .gitignore. the last rule that matches decides,
as in git, so a `!` rule takes a path back.

ponytail: the root file only, so no nested .gitignore files and no global
excludes. move to `git ls-files -co --exclude-standard` if templates need
them. */
fn gitignore(root: &Path) -> Vec<Rule> {
    let Ok(text) = fs::read_to_string(root.join(".gitignore")) else {
        return Vec::new();
    };
    text.lines()
        .filter_map(|line| {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                return None;
            }
            let (negated, line) = match line.strip_prefix('!') {
                Some(rest) => (true, rest),
                None => (false, line),
            };
            let dir_only = line.ends_with('/');
            let line = line.trim_end_matches('/');
            // a slash at the start or in the middle ties the rule to the root
            let glob = match line.strip_prefix('/') {
                Some(anchored) => anchored.to_string(),
                None if line.contains('/') => line.to_string(),
                None => format!("**/{line}"),
            };
            let matcher = GlobBuilder::new(&glob)
                .literal_separator(true)
                .build()
                .ok()?
                .compile_matcher();
            Some((matcher, negated, dir_only))
        })
        .collect()
}

fn ignored(rules: &[Rule], relative: &Path, is_dir: bool) -> bool {
    rules
        .iter()
        .rfind(|(glob, _, dir_only)| (is_dir || !dir_only) && glob.is_match(relative))
        .is_some_and(|(_, negated, _)| !negated)
}

/// what one pass over a tarball saw.
struct Pass {
    files: usize,
    /// the root ember.toml, read on the pass that writes nothing.
    manifest: Option<String>,
}

/** One pass over a GitHub tarball, without the `owner-repo-sha/` folder
that wraps it. it checks every entry, and writes it under `dest` when a
`dest` is given. refused: an absolute path, a `..`, a `.git` folder, a
symlink whose target leaves the folder, a path through a symlink, and any
entry that is not a file, a folder, or a symlink. */
fn entries(tarball: &[u8], dest: Option<&Path>) -> Result<Pass, Error> {
    let mut archive = tar::Archive::new(flate2::read::GzDecoder::new(tarball));
    let mut links: Vec<PathBuf> = Vec::new();
    let mut pass = Pass {
        files: 0,
        manifest: None,
    };
    for entry in archive.entries()? {
        let mut entry = entry?;
        let kind = entry.header().entry_type();
        // GitHub puts the commit in a global header, which is no file
        if kind == tar::EntryType::XGlobalHeader {
            continue;
        }
        let path = entry.path()?.into_owned();
        let Some(relative) = inner_path(&path)? else {
            continue;
        };
        if links.iter().any(|link| relative.starts_with(link)) {
            return Err(refused(&path, "goes through a symlink"));
        }
        match kind {
            tar::EntryType::Directory => {}
            tar::EntryType::Regular | tar::EntryType::Continuous => {
                pass.files += 1;
                if dest.is_none() && relative == Path::new(MANIFEST_FILE) {
                    let mut text = String::new();
                    std::io::Read::read_to_string(&mut entry, &mut text)?;
                    pass.manifest = Some(text);
                }
            }
            tar::EntryType::Symlink => {
                let target = entry
                    .link_name()?
                    .ok_or_else(|| refused(&path, "is a symlink with no target"))?;
                check_link(&relative, &target).map_err(|reason| refused(&path, reason))?;
                links.push(relative.clone());
            }
            _ => return Err(refused(&path, "is not a file, a folder, or a symlink")),
        }
        if let Some(dest) = dest {
            let out = dest.join(&relative);
            if let Some(parent) = out.parent() {
                fs::create_dir_all(parent)?;
            }
            entry.unpack(&out)?;
        }
    }
    Ok(pass)
}

fn refused(path: &Path, reason: &str) -> Error {
    Error::Template(format!(
        "The template archive has an unsafe entry: {} {reason}",
        path.display()
    ))
}

/** the path of an entry inside the wrapping folder. None for the folder
itself. an absolute path, a `..`, a drive letter, and a `.git` folder are
refused. */
fn inner_path(path: &Path) -> Result<Option<PathBuf>, Error> {
    let mut parts = Vec::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            // a `:` or `\` would be a drive or a separator on Windows
            Component::Normal(part) if !part.to_string_lossy().contains([':', '\\']) => {
                parts.push(part)
            }
            _ => return Err(refused(path, "leaves the folder")),
        }
    }
    if parts.get(1).is_some_and(|part| *part == ".git") {
        return Err(refused(path, "is in a .git folder"));
    }
    Ok((parts.len() > 1).then(|| parts[1..].iter().collect()))
}

/** whether a symlink at `at` with `target` stays inside the folder that
holds `at`. relative targets only, and no `..` past the top. */
fn check_link(at: &Path, target: &Path) -> Result<(), &'static str> {
    let mut depth = at.components().count().saturating_sub(1);
    for component in target.components() {
        match component {
            Component::Normal(_) => depth += 1,
            Component::CurDir => {}
            Component::ParentDir if depth > 0 => depth -= 1,
            _ => return Err("links outside the folder"),
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// an empty temp folder, cleaned first so reruns are honest.
    fn folder(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("embr-test-template-{name}"));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn write(root: &Path, file: &str, text: &str) {
        let path = root.join(file);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, text).unwrap();
    }

    fn kept(selection: &Selection) -> Vec<String> {
        selection.kept.iter().map(|file| tar_path(file)).collect()
    }

    #[test]
    fn a_save_leaves_out_secrets_tool_data_lockfiles_and_ignored_files() {
        let root = folder("select");
        for file in [
            "ember.toml",
            "src/init.luau",
            "src/build/init.luau",
            ".env",
            ".env.local",
            "certs/server.pem",
            "id_ed25519",
            "packages/roblox/x.luau",
            "ServerPackages/y.luau",
            "Shared/Pkgs/z.luau",
            "node_modules/a/index.js",
            "src/node_modules/b.js",
            ".git/HEAD",
            "ember.lock",
            "wally.lock",
            "package-lock.json",
            "build/game.rbxl",
            "game.rbxl",
            "keep.rbxl",
            "logs/today.txt",
            "web/.npmrc",
            "tools/.npmrc",
        ] {
            write(&root, file, "x");
        }
        write(
            &root,
            "web/.npmrc",
            "//registry.npmjs.org/:_authToken=npm_secret\n",
        );
        write(&root, "tools/.npmrc", "save-exact=true\n");
        write(&root, ".gitignore", "# places\n*.rbxl\n!keep.rbxl\nlogs/\n");

        let selection = select(&root, &[PathBuf::from("./Shared/Pkgs")]).unwrap();
        assert_eq!(
            kept(&selection),
            [
                ".gitignore",
                "ember.toml",
                "keep.rbxl",
                "src/build/init.luau",
                "src/init.luau",
                "tools/.npmrc",
            ]
        );
        assert_eq!(
            selection.secrets,
            [
                ".env",
                ".env.local",
                "certs/server.pem",
                "id_ed25519",
                "web/.npmrc"
            ]
        );
        for left_out in [
            ".git/",
            "packages/",
            "ServerPackages/",
            "Shared/Pkgs/",
            "node_modules/",
            "src/node_modules/",
            "build/",
            "logs/",
            "ember.lock",
            "wally.lock",
            "package-lock.json",
            "game.rbxl",
        ] {
            assert!(
                selection.left_out.iter().any(|path| path == left_out),
                "{left_out} in {:?}",
                selection.left_out
            );
        }
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn a_local_template_fills_an_empty_folder() {
        let template = folder("local-source");
        write(&template, "ember.toml", "[package]\nname = \"me/old\"\n");
        write(&template, "src/init.luau", "return {}\n");
        write(&template, ".env", "SECRET=1\n");
        let dest = folder("local-dest");
        fs::create_dir_all(dest.join(".git")).unwrap();

        let fetched = fetch(&template.to_string_lossy(), &dest).unwrap();
        assert!(fetched.manifest.contains("me/old"));
        assert_eq!(fetched.write(&dest).unwrap(), 2);
        assert_eq!(
            fs::read_to_string(dest.join("src/init.luau")).unwrap(),
            "return {}\n"
        );
        assert!(!dest.join(".env").exists());

        // the folder has files now, so a second unpack refuses
        assert!(matches!(
            fetch(&template.to_string_lossy(), &dest),
            Err(Error::Template(message)) if message.contains("empty folder")
        ));
        // and a folder with no ember.toml is no template
        fs::remove_file(template.join("ember.toml")).unwrap();
        assert!(matches!(
            fetch(&template.to_string_lossy(), &folder("local-dest-2")),
            Err(Error::Template(message)) if message.contains("no ember.toml")
        ));
        let _ = fs::remove_dir_all(&template);
        let _ = fs::remove_dir_all(&dest);
    }

    #[test]
    fn github_specs_take_an_owner_a_repo_and_a_ref() {
        assert_eq!(github_spec("octocat/game"), Some(("octocat/game", None)));
        assert_eq!(
            github_spec("octocat/game#v1.2"),
            Some(("octocat/game", Some("v1.2")))
        );
        assert_eq!(
            github_spec("octocat/game#feature/x"),
            Some(("octocat/game", Some("feature/x")))
        );
        for spec in [
            "game",
            "octocat/",
            "/game",
            "octo cat/game",
            "octocat/game#",
            "octocat/game#a b",
            "octocat/..",
            "a/b/c",
        ] {
            assert_eq!(github_spec(spec), None, "{spec}");
        }
    }

    #[test]
    fn template_names_are_plain_words() {
        assert!(check_name("rojo-game_2").is_ok());
        for name in ["", "a/b", "..", "a.b", "a b", "a\\b"] {
            assert!(check_name(name).is_err(), "{name}");
        }
    }

    /// one tar entry with a raw name, which the tar crate's own setters refuse to write.
    fn raw_entry(
        builder: &mut tar::Builder<Vec<u8>>,
        name: &str,
        kind: tar::EntryType,
        link: Option<&str>,
        data: &[u8],
    ) {
        let mut header = tar::Header::new_gnu();
        header.as_old_mut().name[..name.len()].copy_from_slice(name.as_bytes());
        if let Some(link) = link {
            header.as_old_mut().linkname[..link.len()].copy_from_slice(link.as_bytes());
        }
        header.set_entry_type(kind);
        header.set_size(data.len() as u64);
        header.set_mode(0o644);
        header.set_cksum();
        builder.append(&header, data).unwrap();
    }

    /// a gzipped tarball of `entries`, each a name, a kind, a link target and the data.
    fn tarball(entries: &[(&str, tar::EntryType, Option<&str>, &str)]) -> Vec<u8> {
        let mut builder = tar::Builder::new(Vec::new());
        for (name, kind, link, data) in entries {
            raw_entry(&mut builder, name, *kind, *link, data.as_bytes());
        }
        let tar = builder.into_inner().unwrap();
        let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
        std::io::Write::write_all(&mut gz, &tar).unwrap();
        gz.finish().unwrap()
    }

    use tar::EntryType::{Directory, Link, Regular, Symlink};

    /// checks a tarball, then writes it, as `embr init` does.
    fn extract(tarball: Vec<u8>, dest: &Path) -> Result<usize, Error> {
        Fetched::from_tarball(tarball)?.write(dest)
    }

    #[test]
    fn a_tarball_unpacks_without_its_wrapping_folder() {
        let dest = folder("extract");
        let archive = tarball(&[
            ("octocat-game-abc/", Directory, None, ""),
            ("octocat-game-abc/ember.toml", Regular, None, "[target]\n"),
            (
                "octocat-game-abc/src/init.luau",
                Regular,
                None,
                "return {}\n",
            ),
        ]);
        assert_eq!(extract(archive, &dest).unwrap(), 2);
        assert_eq!(
            fs::read_to_string(dest.join("ember.toml")).unwrap(),
            "[target]\n"
        );
        assert!(dest.join("src/init.luau").is_file());
        assert!(!dest.join("octocat-game-abc").exists());
        let _ = fs::remove_dir_all(&dest);
    }

    #[cfg(unix)]
    #[test]
    fn a_symlink_inside_the_template_is_kept() {
        let dest = folder("extract-link");
        let archive = tarball(&[
            ("top/ember.toml", Regular, None, "[target]\n"),
            ("top/src/init.luau", Regular, None, "return {}\n"),
            ("top/src/main.luau", Symlink, Some("init.luau"), ""),
            ("top/docs/src", Symlink, Some("../src"), ""),
        ]);
        extract(archive, &dest).unwrap();
        assert_eq!(
            fs::read_link(dest.join("src/main.luau")).unwrap(),
            Path::new("init.luau")
        );
        let _ = fs::remove_dir_all(&dest);
    }

    #[test]
    fn an_unsafe_tarball_writes_nothing() {
        let manifest = ("top/ember.toml", Regular, None, "[target]\n");
        for bad in [
            ("top/../evil.txt", Regular, None, "x"),
            ("/etc/evil.txt", Regular, None, "x"),
            ("top/.git/hooks/post-checkout", Regular, None, "x"),
            ("top/out", Symlink, Some("../../outside"), ""),
            ("top/abs", Symlink, Some("/etc"), ""),
            ("top/hard", Link, Some("top/ember.toml"), ""),
        ] {
            let dest = folder("extract-bad");
            let archive = tarball(&[manifest, bad]);
            assert!(
                matches!(extract(archive, &dest), Err(Error::Template(message)) if message.contains("unsafe")),
                "{}",
                bad.0
            );
            assert_eq!(fs::read_dir(&dest).unwrap().count(), 0, "{}", bad.0);
        }

        // a file written through an earlier symlink is refused too
        let dest = folder("extract-through");
        let archive = tarball(&[
            manifest,
            ("top/dir", Symlink, Some("."), ""),
            ("top/dir/x.txt", Regular, None, "x"),
        ]);
        assert!(matches!(extract(archive, &dest), Err(Error::Template(_))));
        assert_eq!(fs::read_dir(&dest).unwrap().count(), 0);

        // and an archive with no ember.toml at its root is no template
        let archive = tarball(&[("top/src/ember.toml", Regular, None, "")]);
        assert!(matches!(
            extract(archive, &dest),
            Err(Error::Template(message)) if message.contains("no ember.toml")
        ));
        let _ = fs::remove_dir_all(&dest);
    }
}
