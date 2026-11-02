use crate::error::Error;
use crate::project::hooks::{self, Lifecycle};
use crate::project::manifest::edit::{ManifestDoc, Scope};
use crate::project::manifest::{Manifest, Override, override_paths, patch_key};
use crate::ui;
use clap::Args;

#[derive(Args, Debug)]
pub struct RemoveArgs {
    /// Dependencies to remove, each an alias from ember.toml or a full scope/name
    #[arg(required = true)]
    pub dependencies: Vec<String>,
}

/// the two tables a dependency can be in, as ember.toml names them.
const TABLES: [&str; 2] = ["dependencies", "dev-dependencies"];

pub fn run(args: RemoveArgs) -> Result<(), Error> {
    let manifest = Manifest::load()?;
    let lifecycle = Lifecycle::of(&manifest, hooks::REMOVE);
    lifecycle.before()?;

    /* every name must match before anything changes, so a typo in the last
    name leaves ember.toml as it was */
    let removed = matches(&manifest, &args.dependencies)?;

    /* edit the raw document instead of re-serializing `manifest`, so
    comments and formatting in ember.toml survive */
    let mut document = ManifestDoc::open(Scope::Project)?;
    for (table, alias, _) in &removed {
        if let Some(entries) = document.table(table)? {
            entries.remove(alias);
        }
    }
    /* `embr add --dev` makes [dev-dependencies], so it goes with its last
    entry. `embr init` writes [dependencies], so that header stays */
    document.drop_if_empty("dev-dependencies");
    document.save()?;

    for (table, alias, name) in &removed {
        ui::print_success(&format!("Removed {name} as '{alias}' from [{table}]"));
    }
    for warning in leftovers(&manifest, &removed) {
        eprintln!("{warning}");
    }
    println!("Run `embr install` to update the installed packages");

    lifecycle.after()
}

/** the (table, alias, package name) of each entry that one of `wanted`
names. a name matches an alias, or the package name of an entry, so
`embr remove chief/core` finds it under any alias. */
fn matches<'manifest>(
    manifest: &'manifest Manifest,
    wanted: &[String],
) -> Result<Vec<(&'static str, &'manifest str, &'manifest str)>, Error> {
    let entries: Vec<_> = TABLES
        .into_iter()
        .zip([&manifest.dependencies, &manifest.dev_dependencies])
        .flat_map(|(table, entries)| {
            entries
                .iter()
                .map(move |(alias, dependency)| (table, alias.as_str(), dependency.package()))
        })
        .collect();

    let mut found = Vec::new();
    for name in wanted {
        let named: Vec<_> = entries
            .iter()
            .filter(|(_, alias, package)| alias == name || package.eq_ignore_ascii_case(name))
            .collect();
        if named.is_empty() {
            let aliases: Vec<&str> = entries.iter().map(|(_, alias, _)| *alias).collect();
            let known = if aliases.is_empty() {
                "it has no dependencies".to_string()
            } else {
                format!("the aliases are {}", aliases.join(", "))
            };
            return Err(Error::DependencyMissing {
                name: name.clone(),
                known,
            });
        }
        // a name given twice, or an alias and its package name, is one entry
        for entry in named {
            if !found.contains(entry) {
                found.push(*entry);
            }
        }
    }
    Ok(found)
}

/** warnings for the [patches] and [overrides] entries that still name a
removed dependency. install fails on a patch that does not land and on an
override that borrows a missing alias, so the user hears it here first. */
fn leftovers(manifest: &Manifest, removed: &[(&str, &str, &str)]) -> Vec<String> {
    let mut warnings = Vec::new();
    for key in manifest.patches.keys() {
        if let Ok((name, _)) = patch_key(key)
            && removed
                .iter()
                .any(|(_, _, package)| package.eq_ignore_ascii_case(&name))
        {
            warnings.push(format!(
                "warning: [patches] still has '{key}'; `embr install` fails on it unless another dependency still brings in {name}"
            ));
        }
    }
    for (key, replacement) in &manifest.overrides {
        let starts = override_paths(key).unwrap_or_default();
        let borrowed = match replacement {
            Override::Alias(alias) => Some(alias.as_str()),
            Override::Specifier(_) => None,
        };
        if let Some((_, alias, _)) = removed.iter().find(|(_, alias, _)| {
            borrowed == Some(*alias) || starts.iter().any(|path| path[0] == *alias)
        }) {
            warnings.push(format!(
                "warning: [overrides] '{key}' still names '{alias}'; edit or remove it before `embr install`"
            ));
        }
    }
    warnings
}

#[cfg(test)]
mod tests {
    use super::*;

    fn manifest(text: &str) -> Manifest {
        toml::from_str(text).unwrap()
    }

    const DEPENDENCIES: &str = "[dependencies]\n\
        core = { name = \"chief/core\", version = \"^\" }\n\
        signal = { name = \"acme/signal\", version = \"^\" }\n\
        [dev-dependencies]\n\
        lest = { name = \"alloy/lest\", version = \"^\" }\n";

    fn names(text: &str, wanted: &[&str]) -> Result<Vec<String>, Error> {
        let manifest = manifest(text);
        let wanted: Vec<String> = wanted.iter().map(ToString::to_string).collect();
        matches(&manifest, &wanted).map(|found| {
            found
                .into_iter()
                .map(|(table, alias, _)| format!("{table}.{alias}"))
                .collect()
        })
    }

    #[test]
    fn a_name_matches_an_alias_or_a_package_name() {
        assert_eq!(
            names(DEPENDENCIES, &["core", "alloy/lest"]).unwrap(),
            ["dependencies.core", "dev-dependencies.lest"]
        );
        // case does not matter in a package name, and a repeat is no error
        assert_eq!(
            names(DEPENDENCIES, &["Acme/Signal", "signal"]).unwrap(),
            ["dependencies.signal"]
        );
    }

    #[test]
    fn a_name_that_matches_nothing_lists_the_aliases() {
        let error = names(DEPENDENCIES, &["core", "promise"]).unwrap_err();
        assert_eq!(
            error.to_string(),
            "No dependency in ember.toml is named 'promise'; the aliases are core, signal, lest"
        );
        let error = names("", &["core"]).unwrap_err();
        assert!(
            error.to_string().ends_with("it has no dependencies"),
            "{error}"
        );
    }

    #[test]
    fn patches_and_overrides_that_name_a_removed_dependency_warn() {
        let manifest = manifest(&format!(
            "{DEPENDENCIES}[patches]\n\"chief/core@0.2.0\" = \"patches/core.patch\"\n\
             \"acme/signal@1.0.0\" = \"patches/signal.patch\"\n\
             [overrides]\n\"core.traits\" = \"signal\"\n\"signal.other\" = \"signal\"\n"
        ));
        let removed = [("dependencies", "core", "chief/core")];
        let warnings = leftovers(&manifest, &removed);
        assert_eq!(warnings.len(), 2, "{warnings:?}");
        assert!(
            warnings[0].contains("'chief/core@0.2.0'"),
            "{}",
            warnings[0]
        );
        assert!(warnings[1].contains("'core.traits'"), "{}", warnings[1]);

        // an override that borrows the removed alias warns too
        let removed = [("dependencies", "signal", "acme/signal")];
        let warnings = leftovers(&manifest, &removed);
        assert_eq!(warnings.len(), 3, "{warnings:?}");
    }
}
