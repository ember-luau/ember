/*! git dependencies, `{ git = "https://github.com/acme/lib", tag = "v1.2.0" }`.

each repository has one bare clone under ~/.ember/git-cache, keyed as the
index caches are, and fetched with the same stall limit and retries. no
hook of the repository runs: git never fetches the hooks of a remote, and a
bare clone has no working tree, so no checkout and no checkout hook happen.
the resolver reads ember.toml at a commit with `git show`, and install takes
the files of that commit with `git archive`, which leaves submodules out. */

use super::{STALL_SECS, cache_key};
use crate::error::Error;
use crate::sys::{git, paths};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

/// what a git entry asks for. at most one of `rev`, `tag` and `branch`.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum Reference {
    Rev(String),
    Tag(String),
    Branch(String),
    /// no `rev`, `tag` or `branch`: the default branch of the remote.
    DefaultBranch,
}

impl Reference {
    /// the reference of an entry. the manifest check refuses more than one.
    pub fn of(rev: Option<&str>, tag: Option<&str>, branch: Option<&str>) -> Self {
        match (rev, tag, branch) {
            (Some(rev), ..) => Reference::Rev(rev.to_string()),
            (_, Some(tag), _) => Reference::Tag(tag.to_string()),
            (_, _, Some(branch)) => Reference::Branch(branch.to_string()),
            _ => Reference::DefaultBranch,
        }
    }

    /// what git resolves to a commit.
    fn spec(&self) -> String {
        match self {
            Reference::Rev(rev) => rev.clone(),
            Reference::Tag(tag) => format!("refs/tags/{tag}"),
            Reference::Branch(branch) => format!("refs/heads/{branch}"),
            Reference::DefaultBranch => "HEAD".to_string(),
        }
    }

    /// the `branch` and `tag` that ember.lock keeps beside the commit.
    pub fn locked(&self) -> (Option<String>, Option<String>) {
        match self {
            Reference::Branch(branch) => (Some(branch.clone()), None),
            Reference::Tag(tag) => (None, Some(tag.clone())),
            Reference::Rev(_) | Reference::DefaultBranch => (None, None),
        }
    }

    fn describe(&self) -> String {
        match self {
            Reference::Rev(rev) => format!("commit '{rev}'"),
            Reference::Tag(tag) => format!("tag '{tag}'"),
            Reference::Branch(branch) => format!("branch '{branch}'"),
            Reference::DefaultBranch => "default branch".to_string(),
        }
    }
}

/** refuses a git URL that is not https or ssh: `https://`, `ssh://` or the
scp form `user@host:path`. other transports can run a program (`ext::`),
read this computer (`file://`, a path), or go without encryption
(`http://`, `git://`). a leading `-` would reach git as an option. the text
completes "dependency 'lib' ...". */
pub fn check_url(url: &str) -> Result<(), String> {
    let refused = || {
        Err(format!(
            "has the git URL '{url}', which is not an https or ssh URL; use https://, ssh:// or user@host:path"
        ))
    };
    if url.starts_with('-') || url.chars().any(|c| c.is_whitespace() || c.is_control()) {
        return refused();
    }
    if let Some(rest) = url
        .strip_prefix("https://")
        .or_else(|| url.strip_prefix("ssh://"))
    {
        return if rest.is_empty() || rest.starts_with('/') {
            refused()
        } else {
            Ok(())
        };
    }
    if !url.contains("://")
        && let Some((user_host, path)) = url.split_once(':')
        && let Some((user, host)) = user_host.split_once('@')
        && !user.is_empty()
        && !host.is_empty()
        && !user_host.contains('/')
        && !path.is_empty()
    {
        return Ok(());
    }
    // the tests clone a bare repository in a temp folder, never the network
    if cfg!(test) && Path::new(url).is_absolute() {
        return Ok(());
    }
    refused()
}

/// the bare clone of `url`.
fn clone_dir(url: &str) -> Result<PathBuf, Error> {
    Ok(paths::git_cache_dir()?.join(cache_key(b"git-cache", url)))
}

/** runs a clone or a fetch with the index retries, and names the git
dependency, not an index, when it fails. */
fn run(url: &str, args: &[&str]) -> Result<(), Error> {
    super::fetch(url, args, STALL_SECS).map_err(|error| match error {
        Error::IndexFetch { reason, .. } | Error::IndexUnavailable { reason, .. } => {
            Error::GitFetch {
                url: url.to_string(),
                reason,
            }
        }
        other => other,
    })
}

/** brings the clone of `url` up to date: a bare clone the first time, then
a fetch of every branch and tag. */
fn fetch(url: &str) -> Result<PathBuf, Error> {
    let dir = clone_dir(url)?;
    let dir_text = dir.to_string_lossy().into_owned();
    if dir.join("HEAD").exists() {
        run(
            url,
            &[
                "-C",
                &dir_text,
                "fetch",
                "--quiet",
                "--prune",
                "--force",
                "--",
                url,
                "+refs/heads/*:refs/heads/*",
                "+refs/tags/*:refs/tags/*",
            ],
        )?;
        return Ok(dir);
    }
    // a clone that stops half way leaves no folder that looks whole
    let partial = paths::with_suffix(&dir, ".partial");
    let _ = fs::remove_dir_all(&partial);
    fs::create_dir_all(dir.parent().expect("the git cache has a parent"))?;
    run(
        url,
        &[
            "clone",
            "--bare",
            "--quiet",
            "--",
            url,
            &partial.to_string_lossy(),
        ],
    )?;
    fs::rename(&partial, &dir)?;
    Ok(dir)
}

/// the full commit that `reference` names in the clone at `dir`, None when it names none.
fn commit(dir: &Path, reference: &Reference) -> Option<String> {
    git::output(&[
        "-C",
        &dir.to_string_lossy(),
        "rev-parse",
        "--verify",
        "--quiet",
        &format!("{}^{{commit}}", reference.spec()),
    ])
}

/** the commit that `reference` names in `url`, and the text of ember.toml
there. `pinned` is the commit that ember.lock holds: it stands while the
clone has it, so a branch stays where the lock put it. a `rev` that the
clone has needs no fetch either. `refresh` fetches and drops the pin. */
pub fn locate(
    url: &str,
    reference: &Reference,
    pinned: Option<&str>,
    refresh: bool,
) -> Result<(String, String), Error> {
    let dir = clone_dir(url)?;
    let pinned = pinned
        .filter(|_| !refresh)
        .map(|rev| Reference::Rev(rev.to_string()));
    let fixed: Vec<&Reference> = pinned
        .iter()
        .chain(matches!(reference, Reference::Rev(_)).then_some(reference))
        .collect();
    let found = match fixed.iter().find_map(|fixed| commit(&dir, fixed)) {
        Some(found) => found,
        None => {
            fetch(url)?;
            let found = fixed
                .iter()
                .copied()
                .chain([reference])
                .find_map(|wanted| commit(&dir, wanted));
            match (found, reference) {
                (Some(found), _) => found,
                // a commit on no branch or tag, which a host serves by its full id
                (None, Reference::Rev(rev)) => {
                    run(
                        url,
                        &[
                            "-C",
                            &dir.to_string_lossy(),
                            "fetch",
                            "--quiet",
                            "--",
                            url,
                            rev,
                        ],
                    )?;
                    commit(&dir, reference).ok_or_else(|| missing(url, reference))?
                }
                (None, _) => return Err(missing(url, reference)),
            }
        }
    };
    let manifest = git::output(&[
        "-C",
        &dir.to_string_lossy(),
        "show",
        &format!("{found}:{}", crate::project::manifest::MANIFEST_FILE),
    ])
    .ok_or_else(|| Error::GitManifestMissing {
        url: url.to_string(),
        commit: found.clone(),
    })?;
    Ok((found, manifest))
}

fn missing(url: &str, reference: &Reference) -> Error {
    Error::GitReferenceMissing {
        url: url.to_string(),
        reference: reference.describe(),
    }
}

/** writes the files of `rev` in `url` into `dest`. a clone without the
commit fetches first, as a `--locked` install on a new computer needs. */
pub fn export(url: &str, rev: &str, dest: &Path) -> Result<(), Error> {
    let commit_of = Reference::Rev(rev.to_string());
    let dir = clone_dir(url)?;
    if commit(&dir, &commit_of).is_none() {
        fetch(url)?;
        if commit(&dir, &commit_of).is_none() {
            run(
                url,
                &[
                    "-C",
                    &dir.to_string_lossy(),
                    "fetch",
                    "--quiet",
                    "--",
                    url,
                    rev,
                ],
            )?;
        }
    }
    let failed = |reason: String| Error::GitFetch {
        url: url.to_string(),
        reason,
    };
    /* `verbatim`: with core.autocrlf, as git for windows sets it, the
    archive would get CRLF line ends that the commit does not have */
    let output = Command::new("git")
        .args(git::verbatim(&[
            "-C",
            &dir.to_string_lossy(),
            "archive",
            "--format=tar",
            rev,
        ]))
        .output()
        .map_err(|error| failed(format!("could not run git: {error}")))?;
    if !output.status.success() {
        return Err(failed(
            String::from_utf8_lossy(&output.stderr).trim().to_string(),
        ));
    }
    fs::create_dir_all(dest)?;
    tar::Archive::new(&output.stdout[..]).unpack(dest)?;
    Ok(())
}

/// a git remote for the tests, with no network.
#[cfg(test)]
pub(crate) mod fixture {
    use super::clone_dir;
    use std::fs;
    use std::path::PathBuf;

    /** a bare repository in a temp folder whose default branch is `main`,
    the work tree that pushes to it, and its clone in the git cache. all
    three go on drop. */
    pub struct Remote {
        pub url: String,
        work: PathBuf,
        bare: PathBuf,
    }

    /// git with a fixed author and no signing or hooks, whatever the user's config says.
    fn git(args: &[&str]) {
        let mut full = vec![
            "-c",
            "user.name=embr-test",
            "-c",
            "user.email=embr-test@localhost",
            "-c",
            "commit.gpgsign=false",
            "-c",
            "tag.gpgsign=false",
        ];
        full.extend_from_slice(args);
        crate::sys::git::run(&full).unwrap();
    }

    impl Remote {
        pub fn new(name: &str) -> Self {
            let base = std::env::temp_dir().join(format!("embr-test-git-{name}"));
            let _ = fs::remove_dir_all(&base);
            let (work, bare) = (base.join("work"), base.join("remote.git"));
            fs::create_dir_all(&work).unwrap();
            let (work_text, bare_text) = (
                work.to_string_lossy().into_owned(),
                bare.to_string_lossy().into_owned(),
            );
            git(&["init", "--quiet", &work_text]);
            git(&["-C", &work_text, "symbolic-ref", "HEAD", "refs/heads/main"]);
            git(&["init", "--quiet", "--bare", &bare_text]);
            git(&["-C", &bare_text, "symbolic-ref", "HEAD", "refs/heads/main"]);
            let url = bare_text.replace('\\', "/");
            let _ = fs::remove_dir_all(clone_dir(&url).unwrap());
            Remote { url, work, bare }
        }

        /// commits `files` on `main`, pushes, and returns the commit.
        pub fn commit(&self, files: &[(&str, &str)]) -> String {
            for (path, text) in files {
                let path = self.work.join(path);
                fs::create_dir_all(path.parent().unwrap()).unwrap();
                fs::write(path, text).unwrap();
            }
            let work = self.work.to_string_lossy().into_owned();
            git(&["-C", &work, "add", "."]);
            git(&[
                "-C",
                &work,
                "commit",
                "--quiet",
                "--no-verify",
                "-m",
                "fixture",
            ]);
            git(&[
                "-C",
                &work,
                "push",
                "--quiet",
                "--no-verify",
                &self.url,
                "main",
            ]);
            crate::sys::git::output(&["-C", &work, "rev-parse", "HEAD"]).unwrap()
        }

        /// tags the last commit and pushes the tag.
        pub fn tag(&self, tag: &str) {
            let work = self.work.to_string_lossy().into_owned();
            git(&["-C", &work, "tag", tag]);
            git(&[
                "-C",
                &work,
                "push",
                "--quiet",
                "--no-verify",
                &self.url,
                tag,
            ]);
        }
    }

    impl Drop for Remote {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(self.work.parent().unwrap());
            let _ = fs::remove_dir_all(&self.bare);
            if let Ok(dir) = clone_dir(&self.url) {
                let _ = fs::remove_dir_all(&dir);
                // the cache folder goes too when no other clone is in it
                let _ = fs::remove_dir(dir.parent().unwrap());
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_https_and_ssh_urls_pass() {
        for good in [
            "https://github.com/acme/lib",
            "https://github.com/acme/lib.git",
            "ssh://git@github.com/acme/lib.git",
            "git@github.com:acme/lib.git",
        ] {
            assert!(check_url(good).is_ok(), "{good}");
        }
        for bad in [
            "http://github.com/acme/lib",
            "git://github.com/acme/lib",
            "file:///srv/lib",
            "ext::sh -c touch% /tmp/x",
            "../lib",
            "-uhttps://github.com/acme/lib",
            "https://",
            "github.com:acme/lib",
        ] {
            let error = check_url(bad).unwrap_err();
            assert!(error.contains("not an https or ssh URL"), "{bad}: {error}");
        }
    }

    /** a branch resolves to the commit that the lock pins while the clone
    has it, and moves with a refresh. a rev and a tag stay. ember.toml comes
    from the commit, with no checkout. */
    #[test]
    fn a_pinned_branch_stays_until_a_refresh() {
        let remote = fixture::Remote::new("locate");
        let first = remote.commit(&[(
            "ember.toml",
            "[package]\nname = \"acme/lib\"\nversion = \"1.0.0\"\n",
        )]);
        remote.tag("v1.0.0");
        let main = Reference::Branch("main".to_string());
        let (found, text) = locate(&remote.url, &main, None, false).unwrap();
        assert_eq!(found, first);
        assert!(text.contains("1.0.0"), "{text}");

        let second = remote.commit(&[(
            "ember.toml",
            "[package]\nname = \"acme/lib\"\nversion = \"1.1.0\"\n",
        )]);
        assert_eq!(
            locate(&remote.url, &main, Some(&first), false).unwrap().0,
            first
        );
        let (moved, text) = locate(&remote.url, &main, Some(&first), true).unwrap();
        assert_eq!(moved, second);
        assert!(text.contains("1.1.0"), "{text}");
        // with no pin a branch takes its newest commit, and the default branch is main
        assert_eq!(
            locate(&remote.url, &Reference::DefaultBranch, None, false)
                .unwrap()
                .0,
            second
        );
        assert_eq!(
            locate(&remote.url, &Reference::Rev(first.clone()), None, true)
                .unwrap()
                .0,
            first
        );
        assert_eq!(
            locate(
                &remote.url,
                &Reference::Tag("v1.0.0".to_string()),
                None,
                true
            )
            .unwrap()
            .0,
            first
        );
        let error = locate(
            &remote.url,
            &Reference::Branch("gone".to_string()),
            None,
            false,
        )
        .unwrap_err();
        assert!(
            error.to_string().ends_with("has no branch 'gone'"),
            "{error}"
        );

        // the files of a commit, with no .git
        let dest = std::env::temp_dir().join("embr-test-git-export");
        let _ = fs::remove_dir_all(&dest);
        export(&remote.url, &first, &dest).unwrap();
        assert!(
            fs::read_to_string(dest.join("ember.toml"))
                .unwrap()
                .contains("1.0.0")
        );
        assert!(!dest.join(".git").exists());
        let _ = fs::remove_dir_all(&dest);
    }

    #[test]
    fn references_name_what_git_resolves() {
        assert_eq!(Reference::of(None, None, None), Reference::DefaultBranch);
        assert_eq!(Reference::of(None, Some("v1"), None).spec(), "refs/tags/v1");
        assert_eq!(
            Reference::of(None, None, Some("main")).locked(),
            (Some("main".to_string()), None)
        );
        assert_eq!(Reference::of(Some("abc"), None, None).spec(), "abc");
    }
}
