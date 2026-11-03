/*! Azul (azul.ransomwave.games), the sync tool where Studio is the source
and the disk mirrors it. Rojo mounts the packages folders through the
project file. Azul does not read them into Studio by itself, so embr
pushes each packages folder with `azul push`. Azul then mirrors the pushed
copy back under its sync folder, and embr keeps that copy out of git and
out of what `embr publish` packs. */

use crate::error::Error;
use crate::project::manifest::{Environment, Manifest, SyncTool};
use crate::tools;
use std::ffi::OsString;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// the folder Azul mirrors Studio into when [studio] sets no `sync-dir`. Azul's own default.
pub const DEFAULT_SYNC_DIR: &str = "sync";

/// the npm package that Azul ships as.
pub const NPM_PACKAGE: &str = "azul-sync";

/** the services the Azul plugin mirrors by default. a sync folder that
holds one of them is Azul's work. */
const SERVICES: [&str; 10] = [
    "Workspace",
    "Lighting",
    "ReplicatedFirst",
    "ReplicatedStorage",
    "ServerScriptService",
    "ServerStorage",
    "StarterGui",
    "StarterPack",
    "StarterPlayer",
    "SoundService",
];

/** the sync tool of the project in `dir`. `sync` under [studio] always
wins. else a sync folder that holds a Studio service, with no Rojo project
file beside it, marks an Azul project. with both on disk the marker is not
clear, so Rojo stays, as it does for every other project. */
pub fn sync_tool(manifest: &Manifest, dir: &Path) -> SyncTool {
    if let Some(tool) = manifest.studio.as_ref().and_then(|studio| studio.sync) {
        return tool;
    }
    let mirror = dir.join(sync_dir(manifest));
    let mirrors_studio = SERVICES.iter().any(|service| mirror.join(service).is_dir());
    if mirrors_studio && !has_rojo_project(dir) {
        SyncTool::Azul
    } else {
        SyncTool::Rojo
    }
}

fn has_rojo_project(dir: &Path) -> bool {
    std::fs::read_dir(dir).is_ok_and(|entries| {
        entries.flatten().any(|entry| {
            entry
                .file_name()
                .to_str()
                .is_some_and(|name| name.ends_with(".project.json"))
        })
    })
}

/// the folder Azul mirrors Studio into, `sync-dir` under [studio] or Azul's default.
pub fn sync_dir(manifest: &Manifest) -> &str {
    manifest
        .studio
        .as_ref()
        .and_then(|studio| studio.sync_dir.as_deref())
        .unwrap_or(DEFAULT_SYNC_DIR)
}

/// one packages folder and the Studio folder that it goes to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Push {
    pub environment: Environment,
    /// the packages folder on disk, relative to the project.
    pub folder: PathBuf,
    /// the Studio path, dot separated, such as `ReplicatedStorage.Packages`.
    pub destination: String,
}

/** the packages folders that go to Studio. the roblox packages go to
ReplicatedStorage, where the client and the server both see them. the
server packages go to ServerStorage, which never replicates to a client.
[studio] can move either. the other environments never run in Studio. */
pub fn pushes(manifest: &Manifest) -> Result<Vec<Push>, Error> {
    let studio = manifest.studio.as_ref();
    let places = [
        (
            Environment::Roblox,
            "roblox-packages",
            studio.and_then(|studio| studio.roblox_packages.as_deref()),
            "ReplicatedStorage.Packages",
        ),
        (
            Environment::Server,
            "server-packages",
            studio.and_then(|studio| studio.server_packages.as_deref()),
            "ServerStorage.Packages",
        ),
    ];
    places
        .into_iter()
        .map(|(environment, key, configured, default)| {
            let destination = match configured {
                Some(value) => studio_path(value).ok_or_else(|| Error::StudioPathInvalid {
                    key,
                    value: value.to_string(),
                })?,
                None => default.to_string(),
            };
            Ok(Push {
                environment,
                folder: manifest.packages_out(environment),
                destination,
            })
        })
        .collect()
}

/** a Studio path as written in [studio], cleaned: no `game.` in front,
which Azul strips with a warning, and no empty segment. */
fn studio_path(value: &str) -> Option<String> {
    let value = value.trim();
    let value = value.strip_prefix("game.").unwrap_or(value);
    let valid = !value.is_empty() && value.split('.').all(|segment| !segment.trim().is_empty());
    valid.then(|| value.to_string())
}

/** where Azul mirrors each pushed folder back, under the sync folder.
`ReplicatedStorage.Packages` comes back as `sync/ReplicatedStorage/Packages`.
a path that [studio] gets wrong mirrors nowhere, `pushes` reports it. */
pub fn mirror_dirs(manifest: &Manifest) -> Vec<PathBuf> {
    let root = Path::new(sync_dir(manifest));
    pushes(manifest)
        .unwrap_or_default()
        .iter()
        .map(|push| {
            push.destination
                .split('.')
                .fold(root.to_path_buf(), |path, segment| path.join(segment))
        })
        .collect()
}

/// the .gitignore lines that keep the mirrored copies out of git.
pub fn gitignore_entries(manifest: &Manifest) -> Vec<String> {
    mirror_dirs(manifest)
        .iter()
        .map(|dir| {
            let parts: Vec<_> = dir.iter().map(|part| part.to_string_lossy()).collect();
            format!("{}/", parts.join("/"))
        })
        .collect()
}

/** the `azul push` arguments for one folder. `--rojo` reads the folder the
way Rojo does, which the link files and the project files of vendored
packages need. `--destructive` clears the Studio folder first, so a
package that went away goes from Studio too. `--no-warn` skips the
question that `--destructive` asks, since nobody can answer it here. */
pub fn push_args(push: &Push) -> Vec<OsString> {
    let mut args: Vec<OsString> = vec!["push".into(), "--source".into()];
    args.push(push.folder.clone().into());
    args.push("--destination".into());
    args.push(push.destination.clone().into());
    for flag in ["--destructive", "--rojo", "--no-warn"] {
        args.push(flag.into());
    }
    args
}

/** the command that runs Azul. a pinned `npm:azul-sync` tool, project or
global, runs through node directly. a kill then stops Azul itself and not
a wrapper around it. else `azul` on PATH, which npm makes a .cmd script on
Windows. */
pub fn command(dir: &Path) -> Result<Command, Error> {
    let mut pinned = tools::shim::project_tools(dir)?;
    for (alias, tool) in tools::shim::global_tools()? {
        pinned.entry(alias).or_insert(tool);
    }
    let azul = pinned
        .iter()
        .find(|(_, tool)| tool.npm_package() == Some(NPM_PACKAGE));
    if let Some((alias, tool)) = azul {
        return match tools::npm::command(alias, NPM_PACKAGE, &tool.version) {
            Ok(command) => Ok(command),
            Err(Error::ToolNotInstalled(_)) => Err(Error::AzulMissing),
            Err(error) => Err(error),
        };
    }
    Ok(Command::new(if cfg!(windows) {
        "azul.cmd"
    } else {
        "azul"
    }))
}

/// how one `azul push` ended.
#[derive(Debug, PartialEq, Eq)]
pub enum Outcome {
    /// Studio connected and took the folder.
    Pushed,
    /// Studio did not connect before the wait ran out.
    NoStudio,
    /// the live sync daemon holds the port that `azul push` needs.
    DaemonRunning,
    /// no `azul` to run.
    Missing,
    /// azul stopped with an error, its output attached.
    Failed(String),
}

/** runs one `azul push` and waits for it to end. azul waits for the Studio
plugin to connect, so `wait` limits that, and None waits as long as azul
does. the output is captured: a daemon that holds the port shows up in it
as EADDRINUSE, and on success there is nothing in it for the user. */
pub fn run_push(mut command: Command, wait: Option<Duration>) -> Outcome {
    command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Outcome::Missing,
        Err(error) => return Outcome::Failed(error.to_string()),
    };

    // each pipe drains on its own thread, so a full pipe never blocks azul
    let stdout = drain(child.stdout.take());
    let stderr = drain(child.stderr.take());

    let started = Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            /* the drains are left to finish on their own. a grandchild that
            outlives the kill can hold a pipe open, and the output of a
            push that never started says nothing anyway */
            Ok(None) if wait.is_some_and(|wait| started.elapsed() >= wait) => {
                kill(&mut child);
                return Outcome::NoStudio;
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(100)),
            Err(error) => return Outcome::Failed(error.to_string()),
        }
    };
    let output = format!(
        "{}{}",
        stdout.join().unwrap_or_default(),
        stderr.join().unwrap_or_default()
    );
    if status.success() {
        Outcome::Pushed
    } else if output.contains("EADDRINUSE") {
        Outcome::DaemonRunning
    } else {
        Outcome::Failed(output.trim().to_string())
    }
}

fn drain(pipe: Option<impl Read + Send + 'static>) -> std::thread::JoinHandle<String> {
    std::thread::spawn(move || {
        let mut text = String::new();
        if let Some(mut pipe) = pipe {
            let _ = pipe.read_to_string(&mut text);
        }
        text
    })
}

/** stops a push that waits for Studio. on Windows the child can be a .cmd
script, and a kill stops cmd.exe but not the node under it. that node
would hold the port and push later, so taskkill takes the whole tree. */
fn kill(child: &mut std::process::Child) {
    #[cfg(windows)]
    {
        let _ = Command::new("taskkill")
            .args(["/T", "/F", "/PID", &child.id().to_string()])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }
    let _ = child.kill();
    let _ = child.wait();
}

/** whether Roblox Studio runs on this machine. None when the check itself
cannot run, which a caller reads as "maybe". on Linux, Studio runs under
Wine through Vinegar, so the Windows process name is what shows. */
pub fn studio_running() -> Option<bool> {
    #[cfg(windows)]
    {
        let output = Command::new("tasklist")
            .args(["/FI", "IMAGENAME eq RobloxStudioBeta.exe", "/NH"])
            .stdin(Stdio::null())
            .output()
            .ok()?;
        let listed = String::from_utf8_lossy(&output.stdout).to_ascii_lowercase();
        Some(listed.contains("robloxstudiobeta.exe"))
    }
    #[cfg(not(windows))]
    {
        let pattern = if cfg!(target_os = "macos") {
            ["-x", "RobloxStudio"]
        } else {
            ["-f", "RobloxStudioBeta"]
        };
        let status = Command::new("pgrep")
            .args(pattern)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .ok()?;
        // pgrep exits 0 on a match, 1 on none, and higher when it fails
        match status.code() {
            Some(0) => Some(true),
            Some(1) => Some(false),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn manifest(studio: &str) -> Manifest {
        toml::from_str(&format!("[target]\nenvironment = \"roblox\"\n\n{studio}")).unwrap()
    }

    fn project(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("embr-test-azul-{name}"));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn detects_azul_by_setting_then_by_the_sync_folder() {
        let dir = project("detect");
        let plain = manifest("");

        // nothing on disk: Rojo, the default
        assert_eq!(sync_tool(&plain, &dir), SyncTool::Rojo);

        // a sync folder with no service in it says nothing
        fs::create_dir_all(dir.join("sync").join("src")).unwrap();
        assert_eq!(sync_tool(&plain, &dir), SyncTool::Rojo);

        // a mirrored service marks Azul
        fs::create_dir_all(dir.join("sync").join("ReplicatedStorage")).unwrap();
        assert_eq!(sync_tool(&plain, &dir), SyncTool::Azul);

        // a Rojo project file beside it makes the disk unclear: Rojo stays
        fs::write(dir.join("default.project.json"), "{}").unwrap();
        assert_eq!(sync_tool(&plain, &dir), SyncTool::Rojo);

        // the setting wins over the disk, both ways
        let azul = manifest("[studio]\nsync = \"azul\"");
        assert_eq!(sync_tool(&azul, &dir), SyncTool::Azul);
        fs::remove_file(dir.join("default.project.json")).unwrap();
        let rojo = manifest("[studio]\nsync = \"rojo\"");
        assert_eq!(sync_tool(&rojo, &dir), SyncTool::Rojo);

        // a moved sync folder is where the marker is looked for
        let moved = manifest("[studio]\nsync-dir = \"place\"");
        assert_eq!(sync_tool(&moved, &dir), SyncTool::Rojo);
        fs::create_dir_all(dir.join("place").join("Workspace")).unwrap();
        assert_eq!(sync_tool(&moved, &dir), SyncTool::Azul);

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn packages_go_where_the_environments_belong() {
        let pushes = pushes(&manifest("")).unwrap();
        assert_eq!(
            pushes,
            [
                Push {
                    environment: Environment::Roblox,
                    folder: Path::new("packages").join("roblox"),
                    destination: "ReplicatedStorage.Packages".to_string(),
                },
                Push {
                    environment: Environment::Server,
                    folder: Path::new("packages").join("server"),
                    destination: "ServerStorage.Packages".to_string(),
                },
            ]
        );
    }

    #[test]
    fn the_manifest_moves_the_studio_folders() {
        let moved = manifest(
            "[config]\nroblox-packages-out = \"deps\"\n\n[studio]\nroblox-packages = \"game.ReplicatedFirst.Deps\"\nserver-packages = \" ServerScriptService.Packages \"",
        );
        let pushes = pushes(&moved).unwrap();
        assert_eq!(pushes[0].folder, PathBuf::from("deps"));
        assert_eq!(pushes[0].destination, "ReplicatedFirst.Deps");
        assert_eq!(pushes[1].destination, "ServerScriptService.Packages");

        for bad in ["", "game.", "ReplicatedStorage..Packages", ".Packages"] {
            let broken = manifest(&format!("[studio]\nroblox-packages = \"{bad}\""));
            assert!(
                matches!(
                    super::pushes(&broken),
                    Err(Error::StudioPathInvalid {
                        key: "roblox-packages",
                        ..
                    })
                ),
                "{bad:?} should be refused"
            );
        }
    }

    #[test]
    fn builds_the_push_arguments() {
        let push = Push {
            environment: Environment::Roblox,
            folder: PathBuf::from("packages/roblox"),
            destination: "ReplicatedStorage.Packages".to_string(),
        };
        let args: Vec<_> = push_args(&push)
            .iter()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect();
        assert_eq!(
            args,
            [
                "push",
                "--source",
                "packages/roblox",
                "--destination",
                "ReplicatedStorage.Packages",
                "--destructive",
                "--rojo",
                "--no-warn",
            ]
        );
    }

    #[test]
    fn the_mirrored_copies_are_what_gets_ignored() {
        assert_eq!(
            gitignore_entries(&manifest("")),
            [
                "sync/ReplicatedStorage/Packages/",
                "sync/ServerStorage/Packages/"
            ]
        );
        assert_eq!(
            gitignore_entries(&manifest(
                "[studio]\nsync-dir = \"mirror\"\nserver-packages = \"ServerScriptService.Deps\""
            )),
            [
                "mirror/ReplicatedStorage/Packages/",
                "mirror/ServerScriptService/Deps/"
            ]
        );
    }

    #[test]
    fn the_azul_keys_are_known_studio_keys() {
        let azul = manifest(
            "[studio]\nsync = \"azul\"\nsync-dir = \"sync\"\nroblox-packages = \"ReplicatedStorage.Packages\"\nserver-packages = \"ServerStorage.Packages\"",
        );
        // nothing to open yet, and no key reads as a typo
        assert!(matches!(
            azul.studio.as_ref().unwrap().target(),
            Err(Error::StudioUnconfigured)
        ));
        // a misspelled tool name is a parse error that names the choices
        let error = toml::from_str::<Manifest>("[studio]\nsync = \"azule\"").unwrap_err();
        assert!(error.to_string().contains("azul"), "{error}");
    }

    #[test]
    fn gitignoring_the_mirror_twice_adds_each_line_once() {
        let dir = project("gitignore");
        let path = dir.join(".gitignore");
        fs::write(&path, "/target\nsync/ServerStorage/Packages\n").unwrap();
        let entries = gitignore_entries(&manifest(""));
        let entries: Vec<&str> = entries.iter().map(String::as_str).collect();

        let gitignore = crate::commands::init::gitignore;
        assert_eq!(
            gitignore(&path, &entries).unwrap().as_deref(),
            Some("Added sync/ReplicatedStorage/Packages/ to .gitignore")
        );
        assert_eq!(gitignore(&path, &entries).unwrap(), None);
        assert_eq!(
            fs::read_to_string(&path).unwrap(),
            "/target\nsync/ServerStorage/Packages\nsync/ReplicatedStorage/Packages/\n"
        );

        let _ = fs::remove_dir_all(&dir);
    }

    /// a stand-in for azul: a shell script with the exit and output the case needs.
    #[cfg(unix)]
    fn fake(script: &str) -> Command {
        let mut command = Command::new("sh");
        command.args(["-c", script]);
        command
    }

    #[cfg(unix)]
    #[test]
    fn reads_how_a_push_ended() {
        assert_eq!(run_push(fake("echo pushed"), None), Outcome::Pushed);
        assert_eq!(
            run_push(
                fake("echo 'listen EADDRINUSE: address already in use :::8080' >&2; exit 13"),
                None
            ),
            Outcome::DaemonRunning
        );
        assert_eq!(
            run_push(fake("echo 'bad source'; exit 1"), None),
            Outcome::Failed("bad source".to_string())
        );
        assert_eq!(
            run_push(Command::new("embr-test-no-such-azul"), None),
            Outcome::Missing
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_push_that_waits_for_studio_stops_at_the_limit() {
        let started = Instant::now();
        let outcome = run_push(fake("exec sleep 30"), Some(Duration::from_millis(300)));
        assert_eq!(outcome, Outcome::NoStudio);
        assert!(started.elapsed() < Duration::from_secs(10));
    }
}
