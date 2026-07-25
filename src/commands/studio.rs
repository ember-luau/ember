use crate::error::Error;
use crate::project::manifest::edit::{ManifestDoc, Scope};
use crate::project::manifest::{Manifest, StudioTarget};
use crate::ui;
use clap::Subcommand;
use inquire::{Select, Text, validator::Validation};
use std::ffi::OsStr;
use std::path::Path;

#[derive(Subcommand, Debug)]
pub enum StudioCommand {
    /// Create the [studio] table in lpm.toml
    Init,

    /// Open this project's place in Roblox Studio
    Open,
}

/// Bare `lpm studio` never gets here: clap prints the studio help instead
/// (arg_required_else_help in main.rs).
pub fn run(command: StudioCommand) -> Result<(), Error> {
    match command {
        StudioCommand::Init => init(),
        StudioCommand::Open => open(),
    }
}

const PUBLISHED_PLACE: &str = "Published place (universe and place IDs)";
const LOCAL_FILE: &str = "Local place file (.rbxl / .rbxlx)";

fn init() -> Result<(), Error> {
    // Open the manifest first so a missing or unparsable one fails before
    // any prompting, same as `lpm index add`.
    let mut document = ManifestDoc::open(Scope::Project)?;

    // A [studio] with anything in it — including an inline `studio = {...}`,
    // which table() rejects — is left alone. An empty table from a hand-edit
    // is simply filled in, so `open`'s "run `lpm studio init`" advice always
    // works.
    let occupied = match document.table("studio") {
        Ok(table) => table.is_some_and(|table| !table.is_empty()),
        Err(_) => true,
    };
    if occupied {
        return Err(Error::StudioExists);
    }

    inquire::set_global_render_config(ui::render_config());

    let method = Select::new(
        "How should Studio open this project?",
        vec![PUBLISHED_PLACE, LOCAL_FILE],
    )
    .with_help_message("Saved under [studio] in lpm.toml; `lpm studio open` uses it")
    .prompt()?;

    if method == LOCAL_FILE {
        let mut prompt = Text::new("file:")
            .with_help_message("Path to the place file; it doesn't have to exist yet")
            .with_validator(|input: &str| {
                Ok(match validate_file_input(input) {
                    Ok(()) => Validation::Valid,
                    Err(message) => Validation::Invalid(message.into()),
                })
            });
        let default = rojo_place_default(Path::new("."));
        if let Some(default) = &default {
            prompt = prompt.with_default(default);
        }
        let file = prompt.prompt()?;

        let table = document.table_or_create("studio")?;
        table["file"] = toml_edit::value(file.trim());
    } else {
        let universe = prompt_id(
            "universe:",
            "The experience's Universe ID, from the Creator Dashboard",
        )?;
        let place = prompt_id(
            "place:",
            "The Place ID, from the Creator Dashboard or the place page URL",
        )?;

        let table = document.table_or_create("studio")?;
        table["universe"] = toml_edit::value(universe);
        table["place"] = toml_edit::value(place);
    }

    document.save()?;
    ui::print_success("Added [studio] to lpm.toml");
    println!("Open the place any time with `lpm studio open`");
    Ok(())
}

fn open() -> Result<(), Error> {
    let manifest = Manifest::load()?;
    let studio = manifest.studio.as_ref().ok_or(Error::StudioMissing)?;

    match studio.target()? {
        StudioTarget::Place { universe, place } => {
            ensure_protocol_handler()?;
            os_open(OsStr::new(&place_uri(universe, place)))?;
            ui::print_success(&format!("Opening place {place} in Roblox Studio"));
        }
        StudioTarget::File(file) => {
            let path = Path::new(&file);
            validate_place_file(path)?;
            ensure_file_handler(path)?;
            // Absolute so the handler doesn't depend on inheriting our cwd.
            os_open(std::path::absolute(path)?.as_os_str())?;
            ui::print_success(&format!("Opening {file} in Roblox Studio"));
        }
    }
    Ok(())
}

/// The deep link Roblox's own website uses for its "Edit in Studio" buttons.
/// Going through the protocol handler — instead of hunting for the versioned,
/// auto-updating Studio executable — keeps this working across Studio updates
/// on every platform, and involves no credentials: Studio authenticates the
/// user itself once it opens.
fn place_uri(universe: u64, place: u64) -> String {
    format!("roblox-studio:1+launchmode:edit+task:EditPlace+placeId:{place}+universeId:{universe}")
}

/// Everything that can be checked about `file` before Studio is involved: it
/// names a place file, it exists, and it's a file rather than a folder.
fn validate_place_file(path: &Path) -> Result<(), Error> {
    let name = path.display().to_string();
    if !is_place_path(path) {
        return Err(Error::StudioFileNotAPlace(name));
    }
    if !path.exists() {
        return Err(Error::StudioFileMissing(name));
    }
    if !path.is_file() {
        // A stale `rojo build` output directory of the same name, usually.
        return Err(Error::StudioFileIsFolder(name));
    }
    Ok(())
}

/// Whether `path` names a Roblox place file (.rbxl or .rbxlx, any case).
fn is_place_path(path: &Path) -> bool {
    path.extension()
        .and_then(OsStr::to_str)
        .is_some_and(|ext| ext.eq_ignore_ascii_case("rbxl") || ext.eq_ignore_ascii_case("rbxlx"))
}

/// Validates the file prompt: non-empty and named like a place file. The file
/// itself doesn't have to exist yet — building it later (e.g. `rojo build`)
/// is normal.
fn validate_file_input(input: &str) -> Result<(), String> {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return Err("Enter a file name".to_string());
    }
    if !is_place_path(Path::new(trimmed)) {
        return Err("Place files end in .rbxl or .rbxlx".to_string());
    }
    Ok(())
}

/// Parses a Roblox ID as typed at a prompt: a positive integer, surrounding
/// whitespace tolerated. TOML integers are i64, so anything larger is
/// rejected too.
fn parse_id(input: &str) -> Result<i64, String> {
    let trimmed = input.trim();
    match trimmed.parse::<i64>() {
        Ok(id) if id > 0 => Ok(id),
        Ok(_) => Err("IDs are positive, non-zero numbers".to_string()),
        Err(_) if !trimmed.is_empty() && trimmed.chars().all(|c| c.is_ascii_digit()) => {
            Err("That ID is too large".to_string())
        }
        Err(_) => Err("Enter a numeric ID, digits only".to_string()),
    }
}

fn prompt_id(label: &str, help: &str) -> Result<i64, Error> {
    let input = Text::new(label)
        .with_help_message(help)
        .with_validator(|input: &str| {
            Ok(match parse_id(input) {
                Ok(_) => Validation::Valid,
                Err(message) => Validation::Invalid(message.into()),
            })
        })
        .prompt()?;
    Ok(parse_id(&input).expect("the validator only lets numeric IDs through"))
}

/// Default answer for the file prompt: "<name>.rbxl" from the `name` field of
/// the project's Rojo manifest — default.project.json when present, otherwise
/// the first *.project.json alphabetically. None when there's no usable one.
fn rojo_place_default(dir: &Path) -> Option<String> {
    Some(format!("{}.rbxl", rojo_project_name(dir)?))
}

fn rojo_project_name(dir: &Path) -> Option<String> {
    let default = dir.join("default.project.json");
    let path = if default.is_file() {
        default
    } else {
        let mut candidates: Vec<_> = std::fs::read_dir(dir)
            .ok()?
            .filter_map(|entry| entry.ok().map(|entry| entry.path()))
            .filter(|path| {
                path.is_file()
                    && path
                        .file_name()
                        .and_then(OsStr::to_str)
                        .is_some_and(|name| name.ends_with(".project.json"))
            })
            .collect();
        candidates.sort();
        candidates.into_iter().next()?
    };

    let project: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(path).ok()?).ok()?;
    let name = project.get("name")?.as_str()?.trim();
    (!name.is_empty()).then(|| name.to_string())
}

/// Refuses to launch when the roblox-studio: protocol has no handler in the
/// registry, i.e. Studio was never installed. HKCR merges the machine-wide
/// and per-user class registrations; a working handler must carry
/// shell\open\command, so a leftover bare key doesn't count.
#[cfg(windows)]
fn ensure_protocol_handler() -> Result<(), Error> {
    use winreg::RegKey;
    use winreg::enums::HKEY_CLASSES_ROOT;

    match RegKey::predef(HKEY_CLASSES_ROOT).open_subkey("roblox-studio\\shell\\open\\command") {
        Ok(_) => Ok(()),
        Err(_) => Err(Error::StudioNotInstalled(
            "nothing on this system handles roblox-studio: links".to_string(),
        )),
    }
}

/// Same idea for place files: no .rbxl/.rbxlx association means no Studio.
/// Presence of the extension key is a heuristic (user-choice associations
/// live elsewhere); ShellExecuteW still reports SE_ERR_NOASSOC precisely if
/// this passes a stale key through.
#[cfg(windows)]
fn ensure_file_handler(path: &Path) -> Result<(), Error> {
    use winreg::RegKey;
    use winreg::enums::HKEY_CLASSES_ROOT;

    // validate_place_file has already pinned the extension to rbxl/rbxlx.
    let extension = path
        .extension()
        .and_then(OsStr::to_str)
        .unwrap_or("rbxl")
        .to_ascii_lowercase();
    match RegKey::predef(HKEY_CLASSES_ROOT).open_subkey(format!(".{extension}")) {
        Ok(_) => Ok(()),
        Err(_) => Err(Error::StudioNotInstalled(format!(
            "nothing on this system is associated with .{extension} files"
        ))),
    }
}

/// LaunchServices has no cheap "who handles this?" query, but Studio only
/// ever installs as RobloxStudio.app in /Applications or ~/Applications;
/// missing from both means not installed.
#[cfg(target_os = "macos")]
fn studio_bundle_installed() -> bool {
    Path::new("/Applications/RobloxStudio.app").exists()
        || dirs::home_dir().is_some_and(|home| home.join("Applications/RobloxStudio.app").exists())
}

#[cfg(target_os = "macos")]
fn ensure_protocol_handler() -> Result<(), Error> {
    if studio_bundle_installed() {
        Ok(())
    } else {
        Err(Error::StudioNotInstalled(
            "RobloxStudio.app is not in /Applications or ~/Applications".to_string(),
        ))
    }
}

#[cfg(target_os = "macos")]
fn ensure_file_handler(_path: &Path) -> Result<(), Error> {
    ensure_protocol_handler()
}

/// Best effort: xdg-mime knows the scheme handler when a desktop environment
/// is around. Failing to ask means "can't tell" and passes through —
/// xdg-open still reports missing handlers at launch time.
#[cfg(all(unix, not(target_os = "macos")))]
fn ensure_protocol_handler() -> Result<(), Error> {
    let output = std::process::Command::new("xdg-mime")
        .args(["query", "default", "x-scheme-handler/roblox-studio"])
        .output();
    if let Ok(output) = output
        && output.status.success()
        && output.stdout.iter().all(u8::is_ascii_whitespace)
    {
        return Err(Error::StudioNotInstalled(
            "no handler is registered for roblox-studio: links; on Linux, Vinegar (vinegarhq.org) provides Studio"
                .to_string(),
        ));
    }
    Ok(())
}

#[cfg(all(unix, not(target_os = "macos")))]
fn ensure_file_handler(_path: &Path) -> Result<(), Error> {
    // File-type defaults are too patchy to query reliably; xdg-open reports
    // a missing association itself, still before Studio is involved.
    Ok(())
}

/// Opens `target` — a URI or file path — with whatever Windows has registered
/// for it. ShellExecuteW rather than a `cmd /C start` dance: it's the API
/// `start` itself wraps, it needs no shell quoting, and it reports "nothing
/// is registered for this" as a distinct code instead of a message to parse.
#[cfg(windows)]
fn os_open(target: &OsStr) -> Result<(), Error> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::UI::Shell::{
        SE_ERR_ACCESSDENIED, SE_ERR_ASSOCINCOMPLETE, SE_ERR_NOASSOC, ShellExecuteW,
    };
    use windows_sys::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;

    let operation: Vec<u16> = "open".encode_utf16().chain(std::iter::once(0)).collect();
    let file: Vec<u16> = target.encode_wide().chain(std::iter::once(0)).collect();

    // Returns an HINSTANCE for historical reasons; values over 32 mean the
    // handler launched, anything else is one of the SE_ERR_* codes.
    let result = unsafe {
        ShellExecuteW(
            std::ptr::null_mut(),
            operation.as_ptr(),
            file.as_ptr(),
            std::ptr::null(),
            std::ptr::null(),
            SW_SHOWNORMAL,
        )
    } as usize as u32;

    if result > 32 {
        return Ok(());
    }
    let target = target.to_string_lossy();
    match result {
        SE_ERR_NOASSOC | SE_ERR_ASSOCINCOMPLETE => Err(Error::StudioNotInstalled(format!(
            "nothing on this system is registered to open {target}"
        ))),
        SE_ERR_ACCESSDENIED => Err(Error::StudioLaunch(format!(
            "Windows denied access while opening {target}"
        ))),
        code => Err(Error::StudioLaunch(format!(
            "ShellExecute failed with code {code} for {target}"
        ))),
    }
}

/// Opens `target` through LaunchServices. `open` exits non-zero for plenty
/// of reasons besides a missing handler (the bundle check above rules that
/// one out), and prints its own diagnosis to stderr, so the error here just
/// carries the code and defers to that message.
#[cfg(target_os = "macos")]
fn os_open(target: &OsStr) -> Result<(), Error> {
    use std::process::Command;

    let mut command = Command::new("open");
    command.arg(target);
    match crate::sys::process::wait(command)? {
        0 => Ok(()),
        code => Err(Error::StudioLaunch(format!(
            "`open` exited with code {code}; its message above says why"
        ))),
    }
}

/// Opens `target` through the desktop's registered handler. Studio has no
/// official Linux build, so a missing handler gets a pointer at Vinegar
/// rather than a bare failure.
#[cfg(all(unix, not(target_os = "macos")))]
fn os_open(target: &OsStr) -> Result<(), Error> {
    use std::process::Command;

    let mut command = Command::new("xdg-open");
    command.arg(target);
    match crate::sys::process::wait(command) {
        Ok(0) => Ok(()),
        Ok(_) => Err(Error::StudioNotInstalled(format!(
            "no handler is registered for {}; on Linux, Vinegar (vinegarhq.org) provides Studio",
            target.to_string_lossy()
        ))),
        Err(Error::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => Err(
            Error::StudioLaunch("xdg-open is missing; install xdg-utils and retry".to_string()),
        ),
        Err(error) => Err(error),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_ids() {
        assert_eq!(parse_id("1818"), Ok(1818));
        assert_eq!(parse_id("  42  "), Ok(42));
        assert!(parse_id("0").is_err());
        assert!(parse_id("-5").is_err());
        assert!(parse_id("12.5").is_err());
        assert!(parse_id("abc").is_err());
        assert!(parse_id("").is_err());
        // All digits but over i64::MAX gets the "too large" message.
        assert_eq!(
            parse_id("99999999999999999999"),
            Err("That ID is too large".to_string())
        );
    }

    #[test]
    fn builds_the_edit_place_uri() {
        assert_eq!(
            place_uri(13058, 1818),
            "roblox-studio:1+launchmode:edit+task:EditPlace+placeId:1818+universeId:13058"
        );
    }

    #[test]
    fn recognizes_place_paths() {
        assert!(is_place_path(Path::new("game.rbxl")));
        assert!(is_place_path(Path::new("game.RBXL")));
        assert!(is_place_path(Path::new("build/game.rbxlx")));
        assert!(!is_place_path(Path::new("model.rbxm")));
        assert!(!is_place_path(Path::new("game")));
        assert!(!is_place_path(Path::new("default.project.json")));
        assert!(!is_place_path(Path::new(".rbxl")));
    }

    #[test]
    fn validates_file_prompt_input() {
        assert!(validate_file_input("game.rbxl").is_ok());
        assert!(validate_file_input(" build/game.rbxlx ").is_ok());
        assert!(validate_file_input("").is_err());
        assert!(validate_file_input("   ").is_err());
        assert!(validate_file_input("game").is_err());
        assert!(validate_file_input("model.rbxm").is_err());
    }

    #[test]
    fn validates_place_files_on_disk() {
        let dir = std::env::temp_dir().join("lpm-test-studio-place-file");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        // Doesn't exist yet.
        assert!(matches!(
            validate_place_file(&dir.join("missing.rbxl")),
            Err(Error::StudioFileMissing(_))
        ));

        // Exists, but isn't a place file.
        let model = dir.join("model.rbxm");
        std::fs::write(&model, b"x").unwrap();
        assert!(matches!(
            validate_place_file(&model),
            Err(Error::StudioFileNotAPlace(_))
        ));

        // A folder that merely looks like a place file.
        let folder = dir.join("folder.rbxl");
        std::fs::create_dir_all(&folder).unwrap();
        assert!(matches!(
            validate_place_file(&folder),
            Err(Error::StudioFileIsFolder(_))
        ));

        // The real thing.
        let place = dir.join("game.rbxl");
        std::fs::write(&place, b"x").unwrap();
        assert!(validate_place_file(&place).is_ok());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn rojo_default_reads_default_project_json() {
        let dir = std::env::temp_dir().join("lpm-test-studio-rojo-default");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        // No project file at all: no default.
        assert_eq!(rojo_place_default(&dir), None);

        // default.project.json wins even when others exist.
        std::fs::write(dir.join("aaa.project.json"), r#"{"name": "aaa"}"#).unwrap();
        std::fs::write(dir.join("default.project.json"), r#"{"name": "my-game"}"#).unwrap();
        assert_eq!(rojo_place_default(&dir), Some("my-game.rbxl".to_string()));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn rojo_default_falls_back_and_survives_bad_json() {
        let dir = std::env::temp_dir().join("lpm-test-studio-rojo-fallback");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        // Malformed JSON and a blank name both mean "no default".
        std::fs::write(dir.join("default.project.json"), "not json").unwrap();
        assert_eq!(rojo_place_default(&dir), None);
        std::fs::write(dir.join("default.project.json"), r#"{"name": "  "}"#).unwrap();
        assert_eq!(rojo_place_default(&dir), None);

        // Without default.project.json the first *.project.json (sorted) is
        // used.
        std::fs::remove_file(dir.join("default.project.json")).unwrap();
        std::fs::write(dir.join("zzz.project.json"), r#"{"name": "zzz"}"#).unwrap();
        std::fs::write(dir.join("aaa.project.json"), r#"{"name": "aaa"}"#).unwrap();
        assert_eq!(rojo_place_default(&dir), Some("aaa.rbxl".to_string()));

        let _ = std::fs::remove_dir_all(&dir);
    }
}
