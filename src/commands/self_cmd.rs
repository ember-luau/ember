use crate::error::Error;
use crate::github::GithubAPI;
use crate::http::responses::Release;
use clap::Subcommand;
use semver::Version;
use std::env;
use std::fs;
use std::path::{Path, PathBuf};

/// GitHub repo `self update` pulls releases from.
const REPO: &str = "luaupm/cli";

#[derive(Subcommand, Debug)]
pub enum SelfCommand {
    /// Install lpm to ~/.lpm/bin and add it to your PATH
    Install,
    /// Update lpm to the latest GitHub release
    Update,
    /// Remove lpm from your system
    Uninstall,
}

pub fn run(command: SelfCommand) -> Result<(), Error> {
    match command {
        SelfCommand::Install => install(),
        SelfCommand::Update => update(),
        SelfCommand::Uninstall => uninstall(),
    }
}

fn install_dir() -> Result<PathBuf, Error> {
    Ok(dirs::home_dir().ok_or(Error::NoHomeDir)?.join(".lpm"))
}

fn same_file(a: &Path, b: &Path) -> bool {
    match (a.canonicalize(), b.canonicalize()) {
        (Ok(a), Ok(b)) => a == b,
        _ => false,
    }
}

fn install() -> Result<(), Error> {
    let bin_dir = install_dir()?.join("bin");
    fs::create_dir_all(&bin_dir)?;
    let target = bin_dir.join(format!("lpm{}", env::consts::EXE_SUFFIX));

    let current = env::current_exe()?;
    if target.exists() && same_file(&current, &target) {
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

    // Must match the asset names release.yml uploads: lpm-{os}-{arch}[.exe].
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

    let bytes = crate::http::get_bytes(&asset.browser_download_url, &[])?;

    let staged = env::temp_dir().join(&asset_name);
    fs::write(&staged, &bytes)?;
    self_replace::self_replace(&staged)?;
    fs::remove_file(&staged)?;

    println!("Updated lpm v{current} -> v{latest}");
    Ok(())
}

fn uninstall() -> Result<(), Error> {
    let dir = install_dir()?;
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

    // Deleting the directory we are running from needs the self-delete dance;
    // when running from elsewhere (e.g. a dev build), plain removal works.
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
        // Prepended so lpm's tool shims win over other toolchain managers'
        // (aftman/rokit) shims for the same tools later in PATH.
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

    // REG_EXPAND_SZ (not REG_SZ) so existing %VAR% entries in PATH keep expanding.
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

/// Tells running applications (e.g. Explorer) that the environment changed, so
/// new terminals pick up the PATH edit without logging out.
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
