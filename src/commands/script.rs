/*! `embr script`: adds and removes [scripts] entries, and opens a manifest
in an editor. `--global` picks the global manifest, ~/.ember/tools.toml,
whose scripts run in any directory. every edit goes through `ManifestDoc`,
so comments and formatting survive it. */

use crate::commands::run::append_args;
use crate::error::Error;
use crate::project::manifest::edit::{ManifestDoc, Scope};
use crate::sys::process;
use crate::ui;
use clap::Subcommand;
use std::path::{Path, PathBuf};

#[derive(Subcommand, Debug)]
pub enum ScriptCommand {
    /// Add a script to ember.toml, or replace the script of the same name
    Add {
        /// Name of the script, as `embr run <name>` takes it
        name: String,

        /// The command line; quote it, or put it after `--`
        #[arg(required = true, num_args = 1..)]
        command: Vec<String>,

        /// Add to the global manifest, so the script runs in any directory
        #[arg(short, long)]
        global: bool,
    },

    /// Remove a script from ember.toml
    #[command(visible_alias = "rm")]
    Remove {
        /// Name of the script
        name: String,

        /// Remove from the global manifest instead of this project
        #[arg(short, long)]
        global: bool,
    },

    /// Open ember.toml, or the global manifest with --global, in your editor
    Edit {
        /// Open the global manifest, and create it when it is missing
        #[arg(short, long)]
        global: bool,
    },
}

/** the first lines of a global manifest that `embr script` creates. comments
only: no command writes a [scripts] entry that the user did not ask for. */
const HEADER: &str = "\
# The global manifest of embr.
# `embr run <name>` runs a script under [scripts] here in any directory.
# A script of the same name in the ember.toml of a project wins.
# `embr tool add --global` writes the tools under [tools].
";

pub fn run(command: ScriptCommand) -> Result<(), Error> {
    match command {
        ScriptCommand::Add {
            name,
            command,
            global,
        } => add(&name, &command.join(" "), global),
        ScriptCommand::Remove { name, global } => remove(&name, global),
        ScriptCommand::Edit { global } => edit(global),
    }
}

/// the file that `--global` picks, created with its header when it is missing.
fn path(global: bool) -> Result<PathBuf, Error> {
    let path = Scope::from_global(global).path()?;
    if global && !path.exists() {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&path, HEADER)?;
    }
    Ok(path)
}

fn add(name: &str, command: &str, global: bool) -> Result<(), Error> {
    let path = path(global)?;
    let verb = if set(path.clone(), name, command)? {
        "Replaced"
    } else {
        "Added"
    };
    ui::print_success(&format!("{verb} script {name} in {}", path.display()));
    Ok(())
}

/** writes `name = "command"` under [scripts]. true when it replaced an
entry, which keeps its key, its comments and its spacing. */
fn set(path: PathBuf, name: &str, command: &str) -> Result<bool, Error> {
    let mut manifest = ManifestDoc::open_at(path)?;
    let scripts = manifest.table_or_create("scripts")?;
    let replaced = match scripts
        .get_mut(name)
        .and_then(toml_edit::Item::as_value_mut)
    {
        Some(value) => {
            let decor = value.decor().clone();
            *value = command.into();
            *value.decor_mut() = decor;
            true
        }
        None => {
            scripts.insert(name, toml_edit::value(command));
            false
        }
    };
    manifest.save()?;
    Ok(replaced)
}

fn remove(name: &str, global: bool) -> Result<(), Error> {
    let path = Scope::from_global(global).path()?;
    // no global manifest at all means no global script either
    let removed = match unset(path.clone(), name) {
        Err(Error::ManifestMissing) if global => false,
        removed => removed?,
    };
    match removed {
        true => {
            ui::print_success(&format!("Removed script {name} from {}", path.display()));
            Ok(())
        }
        false if global => Err(Error::GlobalScriptMissing(name.to_string())),
        false => Err(Error::ScriptMissing(name.to_string())),
    }
}

/// drops `name` from [scripts]. false when there was no such entry.
fn unset(path: PathBuf, name: &str) -> Result<bool, Error> {
    let mut manifest = ManifestDoc::open_at(path)?;
    let removed = manifest
        .table("scripts")?
        .is_some_and(|scripts| scripts.remove(name).is_some());
    if removed {
        manifest.drop_if_empty("scripts");
        manifest.save()?;
    }
    Ok(removed)
}

fn edit(global: bool) -> Result<(), Error> {
    let path = path(global)?;
    if !path.exists() {
        return Err(Error::ManifestMissing);
    }
    let command = editor_command(&path, |name| std::env::var(name).ok());
    match process::wait(process::shell(&command))? {
        0 => Ok(()),
        code => Err(Error::EditorFailed { command, code }),
    }
}

/// the program that opens a file in its default app.
#[cfg(windows)]
const OPENER: &str = "start \"\"";
#[cfg(target_os = "macos")]
const OPENER: &str = "open";
#[cfg(all(unix, not(target_os = "macos")))]
const OPENER: &str = "xdg-open";

/** the command line that opens `path`: $VISUAL, then $EDITOR, then the
default app. git reads the two variables in this order. the value is a
command line, such as `code --wait`, so it runs through the shell, and the
path is quoted as `embr run` quotes an argument. */
fn editor_command(path: &Path, var: impl Fn(&str) -> Option<String>) -> String {
    let editor = ["VISUAL", "EDITOR"]
        .into_iter()
        .filter_map(var)
        .find(|editor| !editor.trim().is_empty())
        .unwrap_or_else(|| OPENER.to_string());
    append_args(&editor, &[path.to_string_lossy().into_owned()])
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn env(vars: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> {
        let vars: HashMap<String, String> = vars
            .iter()
            .map(|(name, value)| (name.to_string(), value.to_string()))
            .collect();
        move |name| vars.get(name).cloned()
    }

    #[test]
    fn the_editor_is_visual_then_editor_then_the_default_app() {
        let path = Path::new("tools.toml");
        let both = env(&[("VISUAL", "code --wait"), ("EDITOR", "vim")]);
        assert_eq!(editor_command(path, both), "code --wait tools.toml");
        assert_eq!(
            editor_command(path, env(&[("EDITOR", "vim")])),
            "vim tools.toml"
        );
        // an empty value counts as unset
        let empty = env(&[("VISUAL", " "), ("EDITOR", "nano")]);
        assert_eq!(editor_command(path, empty), "nano tools.toml");
        assert_eq!(
            editor_command(path, env(&[])),
            format!("{OPENER} tools.toml")
        );
    }

    #[cfg(not(windows))]
    #[test]
    fn the_path_reaches_the_editor_as_one_argument() {
        let path = Path::new("/home/a b/.ember/tools.toml");
        assert_eq!(
            editor_command(path, env(&[("EDITOR", "vim")])),
            "vim '/home/a b/.ember/tools.toml'"
        );
    }

    /// a manifest file in its own temp folder, cleaned first so reruns are honest.
    fn file(name: &str, text: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("embr-test-script-{name}"));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("tools.toml");
        std::fs::write(&path, text).unwrap();
        path
    }

    #[test]
    fn adding_a_script_keeps_comments_and_formatting() {
        let path = file("add", HEADER);
        assert!(!set(path.clone(), "lint", "selene src").unwrap());
        let written = std::fs::read_to_string(&path).unwrap();
        assert!(written.starts_with(HEADER), "{written}");
        assert!(
            written.ends_with("[scripts]\nlint = \"selene src\"\n"),
            "{written}"
        );

        // a second add with the same name replaces the command in place
        let text = "[tools]\nrojo = \"rojo-rbx/rojo@7.4.4\" # pinned\n\n\
                    [scripts]\n# checks\nlint   = \"selene\"\nfmt = \"stylua .\"\n";
        let path = file("replace", text);
        assert!(set(path.clone(), "lint", "selene src").unwrap());
        let written = std::fs::read_to_string(&path).unwrap();
        assert!(written.contains("rojo = \"rojo-rbx/rojo@7.4.4\" # pinned"));
        assert!(written.contains("# checks\nlint   = \"selene src\"\nfmt = \"stylua .\""));
    }

    #[test]
    fn removing_the_last_script_drops_the_table_and_keeps_the_rest() {
        let text =
            "# mine\n[tools]\nrojo = \"rojo-rbx/rojo@7.4.4\"\n\n[scripts]\nlint = \"selene\"\n";
        let path = file("remove", text);
        assert!(!unset(path.clone(), "fmt").unwrap());
        assert!(unset(path.clone(), "lint").unwrap());
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "# mine\n[tools]\nrojo = \"rojo-rbx/rojo@7.4.4\"\n"
        );
        // a file with no [scripts] has nothing to remove
        assert!(!unset(path, "lint").unwrap());
    }
}
