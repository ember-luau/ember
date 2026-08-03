use crate::error::Error;
use crate::net::github::GithubAPI;
use crate::net::http::responses::Release;
use crate::sys::paths;
use clap::Subcommand;
use semver::Version;
use std::env;
use std::fs;
use std::path::{Path, PathBuf};

/// github repo `self update` pulls releases from
const REPO: &str = "luaupm/cli";

#[derive(Subcommand, Debug)]
pub enum SelfCommand {
    /// Install lpm to ~/.lpm/bin and add it to your PATH
    Install,
    /// Update lpm to the latest GitHub release
    Update,
    /// Remove lpm from your system
    Uninstall,
    /// Set up lpm.toml IntelliSense in VS Code
    Code,
}

pub fn run(command: SelfCommand) -> Result<(), Error> {
    match command {
        SelfCommand::Install => install(),
        SelfCommand::Update => update(),
        SelfCommand::Uninstall => uninstall(),
        SelfCommand::Code => code(),
    }
}

fn install() -> Result<(), Error> {
    let bin_dir = paths::bin_dir()?;
    fs::create_dir_all(&bin_dir)?;
    let target = bin_dir.join(format!("lpm{}", env::consts::EXE_SUFFIX));

    let current = env::current_exe()?;
    if target.exists() && paths::same_file(&current, &target) {
        println!("lpm is already installed at {}", target.display());
    } else {
        fs::copy(&current, &target)?;
        println!(
            "Installed lpm v{} to {}",
            env!("CARGO_PKG_VERSION"),
            target.display()
        );
    }

    /* lpx is `lpm execute` under its own name, so `lpx thing` runs things
    npx-style. a copy of the binary, like the tool shims. */
    if write_lpx_launcher(&bin_dir, &current) {
        println!("Installed the lpx launcher beside it");
    }

    add_to_path(&bin_dir)?;
    suggest_code_setup();
    Ok(())
}

/** points a fresh install at `lpm self code` when there is a VS Code to
point it at. it's the one setup step `self install` can't do for you --
it edits an editor's settings, which is not something an install should
decide -- and nothing else ever mentions it. silent when no flavor is
installed, and when every installed one is already set up: a suggestion
that survives being acted on is just noise. */
fn suggest_code_setup() {
    let Some(config_root) = dirs::config_dir() else {
        return;
    };
    let pending: Vec<&str> = detect_editors(&config_root)
        .into_iter()
        .filter(|(_, settings)| !schema_configured(settings))
        .map(|(editor, _)| editor.name)
        .collect();
    if pending.is_empty() {
        return;
    }

    println!(
        "Detected {}; run `lpm self code` for lpm.toml autocomplete and validation",
        pending.join(", ")
    );
}

/// whether a settings.json already points at the lpm.toml schema. unreadable or absent reads as "not yet", the direction that suggests rather than hides.
fn schema_configured(settings: &Path) -> bool {
    fs::read_to_string(settings)
        .ok()
        .and_then(|text| upsert_association(&text).ok())
        .is_some_and(|association| matches!(association, Association::Current))
}

/// "lpx.exe" on windows, "lpx" elsewhere.
fn lpx_name() -> String {
    format!("lpx{}", env::consts::EXE_SUFFIX)
}

/** copies `source` over ~/.lpm/bin/lpx. best-effort with a warning. on
windows the launcher stays locked for as long as anything it started is
still running, and that must never sink the install or update this rides
along with. fs::copy carries the executable bit over on unix. */
fn write_lpx_launcher(bin_dir: &Path, source: &Path) -> bool {
    let lpx = bin_dir.join(lpx_name());
    match fs::copy(source, &lpx) {
        Ok(_) => true,
        Err(error) => {
            eprintln!(
                "warning: could not write the lpx launcher {}: {error}",
                lpx.display()
            );
            false
        }
    }
}

fn update() -> Result<(), Error> {
    let current = Version::parse(env!("CARGO_PKG_VERSION"))?;
    println!("Checking for updates (currently v{current})");

    let github = GithubAPI::new();
    let release: Release = github.get_latest_release(REPO)?;

    let latest = Version::parse(release.tag_name.trim_start_matches('v'))?;
    if latest <= current {
        println!("lpm is already up to date");
        /* installs from before lpx existed still gain the launcher here,
        so "run `self update`" is the whole migration story even for
        users already on the newest release */
        let bin_dir = paths::bin_dir()?;
        if bin_dir.is_dir()
            && !bin_dir.join(lpx_name()).exists()
            && write_lpx_launcher(&bin_dir, &env::current_exe()?)
        {
            println!("Installed the lpx launcher (new in this version)");
        }
        return Ok(());
    }

    /* the zipped asset release.yml uploads. releases also carry the bare
    binary, but only so an lpm from before the zip existed can still find
    something it understands and update INTO this one -- from here on the
    zip is the only shape lpm reads, and the bare asset can stop being
    published once nobody is left on a version that needs it */
    let asset_name = format!("lpm-{}-{}.zip", env::consts::OS, env::consts::ARCH);
    let asset = release
        .assets
        .iter()
        .find(|asset| asset.name == asset_name)
        .ok_or_else(|| Error::MissingAsset {
            version: latest.clone(),
            asset: asset_name.clone(),
        })?;

    println!("Downloading lpm v{latest}");

    let bytes = crate::net::http::get_bytes(&asset.browser_download_url, &[])?;

    /* the path is captured before the swap. current_exe() itself can go
    stale after self_replace renames the running file out of the way,
    but the path keeps pointing at the freshly installed binary */
    let exe_path = env::current_exe()?;
    let staging = env::temp_dir().join(format!("lpm-update-{}", std::process::id()));
    // the staging dir goes whether or not the swap worked
    let replaced = unpack_update(&asset_name, &bytes, &staging)
        .and_then(|binary| Ok(self_replace::self_replace(&binary)?));
    let _ = fs::remove_dir_all(&staging);
    replaced?;

    /* keep the lpx launcher, a copy of the binary, in step with what was
    just installed. created on update too, so existing installs gain lpx
    without re-running `self install` */
    let bin_dir = paths::bin_dir()?;
    if bin_dir.is_dir() {
        write_lpx_launcher(&bin_dir, &exe_path);
    }

    println!("Updated lpm v{current} -> v{latest}");
    Ok(())
}

/** the new lpm binary, unpacked out of a release asset into `staging`.

releases ship `lpm-{os}-{arch}.zip` holding a bare `lpm[.exe]`, which is
the shape rokit and mise will install lpm from -- they take archives, not
loose binaries. so lpm now reads its own release the same way it reads
every other tool's, through `tools::archive`.

searched for by name rather than taken from the archive root, so an
archive that nests the binary a folder deep still updates, and the
executable bit is set because a zip round trip is not required to carry
one. */
fn unpack_update(asset_name: &str, bytes: &[u8], staging: &Path) -> Result<PathBuf, Error> {
    let _ = fs::remove_dir_all(staging);
    fs::create_dir_all(staging)?;

    let name = format!("lpm{}", env::consts::EXE_SUFFIX);
    /* `extract` sniffs magic bytes and writes anything that isn't an archive
    to the raw target as-is, which is right for a tool whose release really
    is a bare binary. it is not right here: our asset is an archive by
    contract, so bytes that aren't one are a truncated download or an error
    page, and writing those over the running lpm would brick the install.
    parking them under a name the search below can't match turns that into
    the NoExecutableInAsset error instead. */
    crate::tools::archive::extract(asset_name, bytes, staging, &staging.join(".raw-asset"))?;

    let mut files = Vec::new();
    crate::tools::archive::collect_files(staging, &mut files)?;
    let binary = files
        .into_iter()
        .map(|(path, _)| path)
        .find(|path| path.file_name().and_then(|found| found.to_str()) == Some(name.as_str()))
        .ok_or_else(|| Error::NoExecutableInAsset(asset_name.to_string()))?;

    crate::tools::make_executable(&binary)?;
    Ok(binary)
}

fn uninstall() -> Result<(), Error> {
    let dir = paths::lpm_dir()?;
    if !dir.exists() {
        return Err(Error::NotInstalled(dir));
    }

    inquire::set_global_render_config(crate::ui::render_config());
    let confirmed = inquire::Confirm::new(&format!("Remove {}?", dir.display()))
        .with_help_message("Deletes lpm and removes it from your PATH")
        .with_default(false)
        .prompt()?;
    if !confirmed {
        println!("Aborted");
        return Ok(());
    }

    remove_from_path(&dir.join("bin"))?;

    /* deleting the directory we're running from needs the self-delete
    dance. running from elsewhere, like a dev build, plain removal works */
    let running_from_install = env::current_exe()
        .ok()
        .and_then(|exe| exe.canonicalize().ok())
        .zip(dir.canonicalize().ok())
        .is_some_and(|(exe, dir)| exe.starts_with(&dir));
    if running_from_install {
        self_replace::self_delete_outside_path(&dir)?;
    }
    fs::remove_dir_all(&dir)?;

    println!("Uninstalled lpm");
    Ok(())
}

/// canonical schema URL, served from the lpm website
const SCHEMA_URL: &str = "https://luaupm.com/lpm.schema.json";
/** the file-path pattern the schema is associated with. Even Better TOML
keys its associations by regex over the absolute document URI, which uses
forward slashes on every platform, so this matches lpm.toml after a path
separator, but not e.g. not-lpm.toml. */
const ASSOCIATION_PATTERN: &str = "(^|[/\\\\])lpm\\.toml$";
/// the Even Better TOML setting associations live under
const ASSOCIATIONS_SETTING: &str = "evenBetterToml.schema.associations";
/// the extension that reads the association, Taplo
const EXTENSION_ID: &str = "tamasfe.even-better-toml";

/// a VS Code flavor, where its user settings and extensions live.
struct Editor {
    /// product dir under the user config root, e.g. "Code - Insiders"
    name: &'static str,
    /// extensions dir under the home directory
    extensions_dir: &'static str,
    /// binary name, for the `--install-extension` hint
    cli: &'static str,
}

const EDITORS: &[Editor] = &[
    Editor {
        name: "Code",
        extensions_dir: ".vscode",
        cli: "code",
    },
    Editor {
        name: "Code - Insiders",
        extensions_dir: ".vscode-insiders",
        cli: "code-insiders",
    },
    Editor {
        name: "VSCodium",
        extensions_dir: ".vscode-oss",
        cli: "codium",
    },
    Editor {
        name: "Cursor",
        extensions_dir: ".cursor",
        cli: "cursor",
    },
];

/** points every detected VS Code flavor's settings.json at the hosted
lpm.toml schema, so Even Better TOML provides completions and validation.
settings.json is JSONC and hand-edited, so edits go through a CST that
keeps comments and existing entries intact. the edited object may be
reflowed to one key per line, that much is the CST's prerogative. */
fn code() -> Result<(), Error> {
    // %APPDATA% on Windows, ~/Library/Application Support on macOS, ~/.config elsewhere
    let config_root = dirs::config_dir().ok_or(Error::NoHomeDir)?;
    let detected = detect_editors(&config_root);
    if detected.is_empty() {
        println!(
            "If your editor lives somewhere unusual, add this inside the top-level {{ }} of its settings.json:"
        );
        println!("{}", association_snippet());
        return Err(Error::VscodeNotFound(editor_names()));
    }

    /* an editor whose settings can't be edited shouldn't fail the run when
    every other editor is, or already was, set up */
    let mut succeeded = false;
    let mut first_error = None;
    for (editor, settings) in &detected {
        match configure_settings(settings) {
            Ok(Association::Current) => {
                println!("{}: already set up", editor.name);
                succeeded = true;
            }
            Ok(Association::Added(_)) => {
                crate::ui::print_success(&format!(
                    "Pointed {} at the lpm.toml schema ({})",
                    editor.name,
                    settings.display()
                ));
                succeeded = true;
            }
            /* repointing overwrites something the user may have set on
            purpose, a pinned or local schema, so say what was there */
            Ok(Association::Updated { previous, .. }) => {
                crate::ui::print_success(&format!(
                    "Repointed {} at {SCHEMA_URL} (was {previous})",
                    editor.name
                ));
                succeeded = true;
            }
            Err(error) => {
                eprintln!("warning: skipped {}: {error}", editor.name);
                first_error.get_or_insert(error);
            }
        }
    }
    if let Some(error) = first_error
        && !succeeded
    {
        return Err(error);
    }

    // the association is inert until the Taplo extension is around to read it
    if let Some(home) = dirs::home_dir() {
        for (editor, _) in &detected {
            if !extension_installed(&home, editor) {
                println!(
                    "Activate it in {} with: {} --install-extension {EXTENSION_ID}",
                    editor.name, editor.cli
                );
            }
        }
    }

    Ok(())
}

/** the flavors whose settings can be edited, their product dir has a User
dir, i.e. the editor has actually run on this machine. settings.json itself
may not exist yet, first write creates it. */
fn detect_editors(config_root: &Path) -> Vec<(&'static Editor, std::path::PathBuf)> {
    EDITORS
        .iter()
        .filter_map(|editor| {
            let user_dir = config_root.join(editor.name).join("User");
            user_dir
                .is_dir()
                .then(|| (editor, user_dir.join("settings.json")))
        })
        .collect()
}

/// what upserting the association did to a settings file.
#[derive(Debug)]
enum Association {
    /// already points at the schema, nothing written
    Current,
    /// entry or the whole file created
    Added(String),
    /// entry existed pointing elsewhere and was repointed
    Updated { text: String, previous: String },
}

/// the flavor names, for the not-found error.
fn editor_names() -> String {
    EDITORS
        .iter()
        .map(|editor| editor.name)
        .collect::<Vec<_>>()
        .join(", ")
}

/// reads, upserts, and writes back one settings.json.
fn configure_settings(path: &Path) -> Result<Association, Error> {
    let settings_error = |reason: String| Error::VscodeSettings {
        path: path.to_path_buf(),
        reason,
    };

    let text = if path.exists() {
        fs::read_to_string(path).map_err(|error| settings_error(error.to_string()))?
    } else if cfg!(windows) {
        // seed the newline style VS Code itself would use on this platform
        String::from("{}\r\n")
    } else {
        String::from("{}\n")
    };

    let association = upsert_association(&text).map_err(&settings_error)?;
    match &association {
        Association::Current => {}
        Association::Added(new_text) | Association::Updated { text: new_text, .. } => {
            write_settings(path, new_text).map_err(|error| settings_error(error.to_string()))?;
        }
    }
    Ok(association)
}

/** writes through a sibling temp file + rename, so an interrupted write
can't leave a truncated settings.json. unlike lpm's own files, it isn't in
version control. a symlinked settings.json from dotfile setups is resolved
first so the rename lands on the real file instead of replacing the link. */
fn write_settings(path: &Path, text: &str) -> std::io::Result<()> {
    let target = if path.exists() {
        path.canonicalize()?
    } else {
        path.to_path_buf()
    };

    let mut file_name = target.file_name().unwrap_or_default().to_os_string();
    file_name.push(".lpm-tmp");
    let staging = target.with_file_name(file_name);

    fs::write(&staging, text)?;
    fs::rename(&staging, &target).inspect_err(|_| {
        let _ = fs::remove_file(&staging);
    })
}

/** the pure edit. adds or repoints the lpm.toml association in JSONC text,
touching nothing else, comments and existing entries survive. */
fn upsert_association(text: &str) -> Result<Association, String> {
    use jsonc_parser::cst::{CstInputValue, CstRootNode};

    /* VS Code tolerates a UTF-8 BOM, some Windows tools write one, the
    JSONC parser does not. carry it around the edit */
    let (bom, body) = match text.strip_prefix('\u{feff}') {
        Some(body) => ("\u{feff}", body),
        None => ("", text),
    };

    let root = CstRootNode::parse(body, &Default::default()).map_err(|error| error.to_string())?;

    /* the _or_create variants still create missing values but refuse to
    overwrite existing non-object ones, where _or_set would silently delete
    them, a user's mistyped associations value, say */
    let settings = root
        .object_value_or_create()
        .ok_or_else(|| "its top-level value is not an object".to_string())?;
    let associations = settings
        .object_value_or_create(ASSOCIATIONS_SETTING)
        .ok_or_else(|| {
            format!("`{ASSOCIATIONS_SETTING}` is not an object; fix or remove it first")
        })?;

    match associations.get(ASSOCIATION_PATTERN) {
        Some(entry) => {
            let current = entry
                .value()
                .and_then(|value| value.as_string_lit())
                .and_then(|lit| lit.decoded_value().ok());
            if current.as_deref() == Some(SCHEMA_URL) {
                return Ok(Association::Current);
            }
            entry.set_value(CstInputValue::String(SCHEMA_URL.to_string()));
            Ok(Association::Updated {
                text: format!("{bom}{root}"),
                previous: current.unwrap_or_else(|| "a non-string value".to_string()),
            })
        }
        None => {
            associations.append(
                ASSOCIATION_PATTERN,
                CstInputValue::String(SCHEMA_URL.to_string()),
            );
            Ok(Association::Added(format!("{bom}{root}")))
        }
    }
}

/// whether Even Better TOML is installed for an editor flavor.
fn extension_installed(home: &Path, editor: &Editor) -> bool {
    let extensions = home.join(editor.extensions_dir).join("extensions");
    fs::read_dir(extensions).is_ok_and(|entries| {
        entries.filter_map(Result::ok).any(|entry| {
            entry
                .file_name()
                .to_string_lossy()
                .starts_with(EXTENSION_ID)
        })
    })
}

/// the settings fragment printed when no VS Code is found.
fn association_snippet() -> String {
    format!(
        "  \"{ASSOCIATIONS_SETTING}\": {{\n    \"{}\": \"{SCHEMA_URL}\"\n  }}",
        ASSOCIATION_PATTERN.replace('\\', "\\\\")
    )
}

#[cfg(windows)]
fn add_to_path(bin_dir: &Path) -> Result<(), Error> {
    let (env_key, path) = read_user_path()?;
    let dir = bin_dir.to_string_lossy();

    if path_contains(&path, &dir) {
        println!("PATH already contains {dir}");
        return Ok(());
    }

    let new_path = if path.trim().is_empty() {
        dir.to_string()
    } else {
        /* prepended so lpm's tool shims win over aftman/rokit shims for
        the same tools later in PATH */
        format!("{dir};{path}")
    };
    write_user_path(&env_key, &new_path)?;

    println!("Added {dir} to your PATH");
    println!("Restart your terminal, then run `lpm --version` to verify");
    Ok(())
}

#[cfg(windows)]
fn remove_from_path(bin_dir: &Path) -> Result<(), Error> {
    let (env_key, path) = read_user_path()?;
    let dir = bin_dir.to_string_lossy();

    if !path_contains(&path, &dir) {
        return Ok(());
    }

    let new_path = path
        .split(';')
        .filter(|entry| !entry.trim().is_empty() && !entry.trim().eq_ignore_ascii_case(&dir))
        .collect::<Vec<_>>()
        .join(";");
    write_user_path(&env_key, &new_path)?;

    println!("Removed {dir} from your PATH");
    Ok(())
}

#[cfg(windows)]
fn path_contains(path: &str, dir: &str) -> bool {
    path.split(';')
        .any(|entry| entry.trim().eq_ignore_ascii_case(dir))
}

#[cfg(windows)]
fn read_user_path() -> Result<(winreg::RegKey, String), Error> {
    use winreg::RegKey;
    use winreg::enums::{HKEY_CURRENT_USER, KEY_READ, KEY_WRITE};

    let hkcu = RegKey::predef(HKEY_CURRENT_USER);
    let env_key = hkcu.open_subkey_with_flags("Environment", KEY_READ | KEY_WRITE)?;
    let path: String = env_key.get_value("Path").unwrap_or_default();
    Ok((env_key, path))
}

#[cfg(windows)]
fn write_user_path(env_key: &winreg::RegKey, path: &str) -> Result<(), Error> {
    use winreg::RegValue;
    use winreg::enums::RegType;

    // REG_EXPAND_SZ, not REG_SZ, so existing %VAR% entries in PATH keep expanding
    let bytes = path
        .encode_utf16()
        .chain(std::iter::once(0))
        .flat_map(u16::to_le_bytes)
        .collect();
    env_key.set_raw_value(
        "Path",
        &RegValue {
            bytes,
            vtype: RegType::REG_EXPAND_SZ,
        },
    )?;
    broadcast_environment_change();
    Ok(())
}

/** tells running apps like Explorer the environment changed, so new
terminals pick up the PATH edit without logging out. */
#[cfg(windows)]
fn broadcast_environment_change() {
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        HWND_BROADCAST, SMTO_ABORTIFHUNG, SendMessageTimeoutW, WM_SETTINGCHANGE,
    };

    let param: Vec<u16> = "Environment\0".encode_utf16().collect();
    unsafe {
        SendMessageTimeoutW(
            HWND_BROADCAST,
            WM_SETTINGCHANGE,
            0,
            param.as_ptr() as _,
            SMTO_ABORTIFHUNG,
            5000,
            std::ptr::null_mut(),
        );
    }
}

#[cfg(not(windows))]
fn add_to_path(bin_dir: &Path) -> Result<(), Error> {
    println!(
        "Add {} to your PATH by appending this to your shell profile:",
        bin_dir.display()
    );
    println!("  export PATH=\"{}:$PATH\"", bin_dir.display());
    Ok(())
}

#[cfg(not(windows))]
fn remove_from_path(bin_dir: &Path) -> Result<(), Error> {
    println!(
        "Remove the {} entry from your shell profile to finish cleaning up",
        bin_dir.display()
    );
    Ok(())
}

#[cfg(all(test, windows))]
mod tests {
    use super::*;

    #[test]
    fn path_matching_ignores_case_and_padding() {
        let path = r"C:\Windows;c:\users\me\.LPM\BIN ; C:\Tools";
        assert!(path_contains(path, r"C:\Users\Me\.lpm\bin"));
        assert!(path_contains(path, r"C:\Tools"));
        assert!(!path_contains(path, r"C:\Users\Me\.lpm"));
    }
}

/** what `self update` does with a release asset, minus the network and the
swap. the asset shape is a contract with release.yml, and the only other
thing holding it is a comment there. */
#[cfg(test)]
mod update_tests {
    use super::*;
    use std::io::Write as _;

    /// a zip holding `names`, each one a stand-in binary.
    fn zip_of(names: &[&str]) -> Vec<u8> {
        let mut writer = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
        let options: zip::write::FileOptions<'_, ()> =
            zip::write::FileOptions::default().compression_method(zip::CompressionMethod::Stored);
        for name in names {
            writer.start_file(*name, options).unwrap();
            writer.write_all(b"\x7fELF not really").unwrap();
        }
        writer.finish().unwrap().into_inner()
    }

    fn staging(name: &str) -> PathBuf {
        let dir = env::temp_dir().join(format!("lpm-test-update-{name}"));
        let _ = fs::remove_dir_all(&dir);
        dir
    }

    #[test]
    fn takes_the_binary_out_of_a_release_zip() {
        let dir = staging("zip");
        let binary = format!("lpm{}", env::consts::EXE_SUFFIX);

        // the shape release.yml uploads: the binary alone, at the root
        let found = unpack_update("lpm-linux-x86_64.zip", &zip_of(&[&binary]), &dir).unwrap();
        assert_eq!(found.file_name().unwrap().to_str().unwrap(), binary);
        assert!(found.starts_with(&dir));
        assert!(found.is_file());

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = fs::metadata(&found).unwrap().permissions().mode();
            assert_eq!(mode & 0o111, 0o111, "a zip need not carry the exec bit");
        }

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn finds_the_binary_even_when_the_archive_nests_it() {
        let dir = staging("nested");
        let binary = format!("lpm{}", env::consts::EXE_SUFFIX);
        let nested = format!("lpm-linux-x86_64/{binary}");

        let found = unpack_update("lpm-linux-x86_64.zip", &zip_of(&[&nested]), &dir).unwrap();
        assert_eq!(found.file_name().unwrap().to_str().unwrap(), binary);

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn an_archive_without_lpm_in_it_is_an_error_not_a_swap() {
        /* the failure that matters: replacing the running binary with
        whatever happened to be in the archive would brick the install */
        let dir = staging("empty");
        assert!(matches!(
            unpack_update("lpm-linux-x86_64.zip", &zip_of(&["README.md"]), &dir),
            Err(Error::NoExecutableInAsset(_))
        ));

        /* and bytes that are no archive at all -- a truncated download, an
        error page -- must not be written over the running binary either */
        let raw = staging("raw");
        assert!(matches!(
            unpack_update("lpm-linux-x86_64.zip", b"not an archive", &raw),
            Err(Error::NoExecutableInAsset(_))
        ));

        let _ = fs::remove_dir_all(&dir);
        let _ = fs::remove_dir_all(&raw);
    }
}

#[cfg(test)]
mod code_tests {
    use super::*;

    fn added(text: &str) -> String {
        match upsert_association(text).unwrap() {
            Association::Added(new_text) => new_text,
            Association::Current => panic!("expected Added, got Current"),
            Association::Updated { .. } => panic!("expected Added, got Updated"),
        }
    }

    #[test]
    fn adds_association_to_fresh_settings() {
        for text in ["", "{}", "{}\n"] {
            let written = added(text);
            assert!(written.contains(ASSOCIATIONS_SETTING), "in {written:?}");
            assert!(written.contains(SCHEMA_URL), "in {written:?}");
            // the written pattern is JSON-escaped, backslashes doubled
            assert!(
                written.contains(r#"(^|[/\\\\])lpm\\.toml$"#),
                "in {written:?}"
            );
        }
    }

    #[test]
    fn preserves_comments_formatting_and_other_settings() {
        let text = "{\n  // my carefully curated settings\n  \"editor.fontSize\": 14,\n  \"evenBetterToml.schema.associations\": {\n    \"other\\\\.toml$\": \"https://example.com/other.json\",\n  },\n}\n";
        let written = match upsert_association(text).unwrap() {
            Association::Added(new_text) => new_text,
            _ => panic!("expected Added"),
        };
        assert!(written.contains("// my carefully curated settings"));
        assert!(written.contains("\"editor.fontSize\": 14"));
        assert!(written.contains("https://example.com/other.json"));
        assert!(written.contains(SCHEMA_URL));
        // the output must itself still be parseable, correct JSONC
        assert!(matches!(
            upsert_association(&written).unwrap(),
            Association::Current
        ));
    }

    #[test]
    fn refuses_to_clobber_non_object_values() {
        // a mistyped associations value like an array must be reported,
        // not silently deleted. same for a non-object root
        let text = r#"{"evenBetterToml.schema.associations": ["oops"]}"#;
        assert!(
            upsert_association(text)
                .unwrap_err()
                .contains("not an object")
        );
        assert!(
            upsert_association("[1, 2, 3]")
                .unwrap_err()
                .contains("not an object")
        );
    }

    #[test]
    fn carries_a_bom_through_the_edit() {
        let written = added("\u{feff}{}");
        assert!(written.starts_with('\u{feff}'));
        assert!(written.contains(SCHEMA_URL));
    }

    #[test]
    fn keeps_crlf_line_endings() {
        let written = added("{\r\n  \"editor.fontSize\": 14\r\n}\r\n");
        assert!(!written.replace("\r\n", "").contains('\n'));
    }

    #[test]
    fn is_idempotent_once_configured() {
        let written = added("{}");
        assert!(matches!(
            upsert_association(&written).unwrap(),
            Association::Current
        ));
    }

    #[test]
    fn repoints_an_outdated_url_and_reports_what_it_replaced() {
        let outdated = added("{}").replace(SCHEMA_URL, "https://old.example.com/lpm.json");
        let (written, previous) = match upsert_association(&outdated).unwrap() {
            Association::Updated { text, previous } => (text, previous),
            _ => panic!("expected Updated"),
        };
        assert!(written.contains(SCHEMA_URL));
        assert!(!written.contains("old.example.com"));
        assert_eq!(previous, "https://old.example.com/lpm.json");
    }

    #[test]
    fn configures_a_settings_file_on_disk() {
        let dir = std::env::temp_dir().join("lpm-test-self-code-configure");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let settings = dir.join("settings.json");

        // missing file is created...
        assert!(matches!(
            configure_settings(&settings).unwrap(),
            Association::Added(_)
        ));
        assert!(
            std::fs::read_to_string(&settings)
                .unwrap()
                .contains(SCHEMA_URL)
        );
        // ...a second run changes nothing...
        assert!(matches!(
            configure_settings(&settings).unwrap(),
            Association::Current
        ));
        // ...and a malformed file errors with its path instead of writing
        std::fs::write(&settings, "{ not json").unwrap();
        assert!(matches!(
            configure_settings(&settings),
            Err(Error::VscodeSettings { .. })
        ));
        assert_eq!(std::fs::read_to_string(&settings).unwrap(), "{ not json");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn rejects_malformed_settings() {
        assert!(upsert_association("{ not json").is_err());
    }

    /** what `self install`'s suggestion keys off: an editor that has run and
    is not already pointed at the schema. */
    #[test]
    fn schema_setup_is_only_pending_until_it_is_done() {
        let dir = std::env::temp_dir().join("lpm-test-self-code-pending");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let settings = dir.join("settings.json");

        // never opened, mistyped, or pointed elsewhere: all still pending
        assert!(!schema_configured(&settings));
        std::fs::write(&settings, "{ not json").unwrap();
        assert!(!schema_configured(&settings));
        std::fs::write(
            &settings,
            added("{}").replace(SCHEMA_URL, "https://old.example.com"),
        )
        .unwrap();
        assert!(!schema_configured(&settings));

        // and done once `self code` has run
        configure_settings(&settings).unwrap();
        assert!(schema_configured(&settings));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn detects_editors_that_have_run() {
        let root = std::env::temp_dir().join("lpm-test-self-code-detect");
        let _ = std::fs::remove_dir_all(&root);
        // Code has run, its User dir exists. Cursor is installed but never
        // ran, the rest are absent entirely.
        std::fs::create_dir_all(root.join("Code").join("User")).unwrap();
        std::fs::create_dir_all(root.join("Cursor")).unwrap();

        let detected = detect_editors(&root);
        assert_eq!(detected.len(), 1);
        assert_eq!(detected[0].0.name, "Code");
        assert_eq!(
            detected[0].1,
            root.join("Code").join("User").join("settings.json")
        );

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn snippet_is_valid_json_fragment() {
        let wrapped = format!("{{\n{}\n}}", association_snippet());
        let parsed: serde_json::Value = serde_json::from_str(&wrapped).unwrap();
        assert_eq!(
            parsed[ASSOCIATIONS_SETTING][ASSOCIATION_PATTERN],
            SCHEMA_URL
        );
    }
}
