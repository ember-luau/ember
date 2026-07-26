use crate::error::Error;
use crate::net::github::GithubAPI;
use crate::net::http::responses::Release;
use crate::sys::paths;
use clap::Subcommand;
use semver::Version;
use std::env;
use std::fs;
use std::path::Path;

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

    add_to_path(&bin_dir)
}

fn update() -> Result<(), Error> {
    let current = Version::parse(env!("CARGO_PKG_VERSION"))?;
    println!("Checking for updates (currently v{current})");

    let github = GithubAPI::new();
    let release: Release = github.get_latest_release(REPO)?;

    let latest = Version::parse(release.tag_name.trim_start_matches('v'))?;
    if latest <= current {
        println!("lpm is already up to date");
        return Ok(());
    }

    // must match the asset names release.yml uploads: lpm-{os}-{arch}[.exe]
    let asset_name = format!(
        "lpm-{}-{}{}",
        env::consts::OS,
        env::consts::ARCH,
        env::consts::EXE_SUFFIX
    );
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

    let staged = env::temp_dir().join(&asset_name);
    fs::write(&staged, &bytes)?;
    self_replace::self_replace(&staged)?;
    fs::remove_file(&staged)?;

    println!("Updated lpm v{current} -> v{latest}");
    Ok(())
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

    /* deleting the directory we're running from needs the self-delete dance;
    running from elsewhere (e.g. a dev build), plain removal works */
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
keys its associations by regex over the absolute document URI — which uses
forward slashes on every platform — so this matches lpm.toml after a path
separator, but not e.g. not-lpm.toml. */
const ASSOCIATION_PATTERN: &str = "(^|[/\\\\])lpm\\.toml$";
/// the Even Better TOML setting associations live under
const ASSOCIATIONS_SETTING: &str = "evenBetterToml.schema.associations";
/// the extension that reads the association (Taplo)
const EXTENSION_ID: &str = "tamasfe.even-better-toml";

/// a VS Code flavor: where its user settings and extensions live.
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
keeps comments and existing entries intact (the edited object may be
reflowed to one key per line — that much is the CST's prerogative). */
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
    every other editor is (or already was) set up */
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
            purpose (a pinned or local schema), so say what was there */
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

/** the flavors whose settings can be edited: their product dir has a User
dir, i.e. the editor has actually run on this machine. settings.json itself
may not exist yet — first write creates it. */
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
    /// already points at the schema; nothing written
    Current,
    /// entry (or the whole file) created
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
can't leave a truncated settings.json — unlike lpm's own files, it isn't in
version control. A symlinked settings.json (dotfile setups) is resolved
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

/** the pure edit: adds or repoints the lpm.toml association in JSONC text,
touching nothing else — comments and existing entries survive. */
fn upsert_association(text: &str) -> Result<Association, String> {
    use jsonc_parser::cst::{CstInputValue, CstRootNode};

    /* VS Code tolerates a UTF-8 BOM (some Windows tools write one), the
    JSONC parser does not; carry it around the edit */
    let (bom, body) = match text.strip_prefix('\u{feff}') {
        Some(body) => ("\u{feff}", body),
        None => ("", text),
    };

    let root = CstRootNode::parse(body, &Default::default()).map_err(|error| error.to_string())?;

    /* the _or_create variants still create missing values but refuse to
    overwrite existing non-object ones, where _or_set would silently delete
    them (a user's mistyped associations value, say) */
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
        /* prepended so lpm's tool shims win over other toolchain managers'
        (aftman/rokit) shims for the same tools later in PATH */
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

    // REG_EXPAND_SZ (not REG_SZ) so existing %VAR% entries in PATH keep expanding
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

/** tells running apps (e.g. Explorer) the environment changed, so new
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
            // the written pattern is JSON-escaped: backslashes doubled
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
        // a mistyped associations value (array) must be reported, not
        // silently deleted; same for a non-object root
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

    #[test]
    fn detects_editors_that_have_run() {
        let root = std::env::temp_dir().join("lpm-test-self-code-detect");
        let _ = std::fs::remove_dir_all(&root);
        // Code has run (User dir exists), Cursor is installed but never ran,
        // the rest are absent entirely.
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
