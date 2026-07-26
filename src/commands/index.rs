use crate::error::Error;
use crate::project::manifest::Manifest;
use crate::project::manifest::edit::{ManifestDoc, Scope};
use crate::ui;
use clap::Subcommand;
use inquire::CustomUserError;
use inquire::autocompletion::{Autocomplete, Replacement};

#[derive(Subcommand, Debug)]
pub enum IndexCommand {
    /// Add an index under [indices] in lpm.toml
    Add {
        /// Name dependencies reference the index by (e.g. wally)
        name: Option<String>,

        /// Git URL of the index; well-known names fill this in automatically
        url: Option<String>,
    },

    /// Remove an index from [indices] in lpm.toml
    Remove {
        /// Name of the index; picked from a list when omitted
        name: Option<String>,
    },
}

/// Indices offered as autocomplete suggestions. Suggestions only — any name
/// and URL the user types is accepted as-is.
const KNOWN_INDICES: &[(&str, &str)] = &[
    ("pesde", "https://github.com/pesde-pkg/index"),
    ("wally", "https://github.com/UpliftGames/wally-index"),
];

fn known_url(name: &str) -> Option<&'static str> {
    KNOWN_INDICES
        .iter()
        .find(|(key, _)| *key == name)
        .map(|(_, url)| *url)
}

/// Case-insensitive contains filter over a fixed suggestion list. Tab
/// completes the highlighted suggestion, or the only match when just one
/// remains; free-form input is never blocked.
#[derive(Clone)]
struct Suggestions(Vec<String>);

impl Autocomplete for Suggestions {
    fn get_suggestions(&mut self, input: &str) -> Result<Vec<String>, CustomUserError> {
        let needle = input.to_ascii_lowercase();
        Ok(self
            .0
            .iter()
            .filter(|option| option.to_ascii_lowercase().contains(&needle))
            .cloned()
            .collect())
    }

    fn get_completion(
        &mut self,
        input: &str,
        highlighted: Option<String>,
    ) -> Result<Replacement, CustomUserError> {
        if highlighted.is_some() {
            return Ok(highlighted);
        }
        let mut matches = self.get_suggestions(input)?;
        Ok((matches.len() == 1).then(|| matches.remove(0)))
    }
}

pub fn run(command: IndexCommand) -> Result<(), Error> {
    match command {
        IndexCommand::Add { name, url } => add(name, url),
        IndexCommand::Remove { name } => remove(name),
    }
}

fn add(name: Option<String>, url: Option<String>) -> Result<(), Error> {
    // Open first so a missing or unparsable manifest fails before prompting.
    let mut document = ManifestDoc::open(Scope::Project)?;
    inquire::set_global_render_config(ui::render_config());

    let name = match name {
        Some(name) => name,
        None => inquire::Text::new("Index name:")
            .with_autocomplete(Suggestions(
                KNOWN_INDICES
                    .iter()
                    .map(|(key, _)| key.to_string())
                    .collect(),
            ))
            .with_help_message("pesde and wally are suggested; any name works")
            .prompt()?,
    };
    let name = name.trim().to_string();
    if name.is_empty() {
        return Err(Error::ManifestInvalid(
            "[indices] keys cannot be empty".to_string(),
        ));
    }

    let url = match url {
        Some(url) => url,
        // A well-known name already tells us the URL; only ask otherwise.
        None => match known_url(&name) {
            Some(url) => url.to_string(),
            None => inquire::Text::new("Index URL:")
                .with_autocomplete(Suggestions(
                    KNOWN_INDICES
                        .iter()
                        .map(|(_, url)| url.to_string())
                        .collect(),
                ))
                .with_help_message("git repository of the index")
                .prompt()?,
        },
    };
    let url = url.trim().to_string();
    if url.is_empty() {
        return Err(Error::ManifestInvalid("an index needs a URL".to_string()));
    }

    let table = document.table_or_create("indices")?;
    let replaced = table.contains_key(&name);
    table[&name] = toml_edit::value(&url);
    document.save()?;

    let verb = if replaced { "Updated" } else { "Added" };
    ui::print_success(&format!("{verb} index {name} → {url}"));
    if name == "default" {
        println!("The default key overrides lpm's built-in index for bare dependencies");
    } else {
        println!("Use it with `lpm add <scope>/<name> --index {name}`");
    }

    Ok(())
}

fn remove(name: Option<String>) -> Result<(), Error> {
    // Read before editing so the dependents warning below sees the same file.
    let manifest = Manifest::load()?;

    let mut document = ManifestDoc::open(Scope::Project)?;
    let Some(table) = document.table("indices")? else {
        println!("No indices defined in lpm.toml");
        return Ok(());
    };

    let name = match name {
        Some(name) => name,
        None => {
            let keys: Vec<String> = table.iter().map(|(key, _)| key.to_string()).collect();
            if keys.is_empty() {
                println!("No indices defined in lpm.toml");
                return Ok(());
            }
            inquire::set_global_render_config(ui::render_config());
            inquire::Select::new("Remove which index?", keys).prompt()?
        }
    };

    if table.remove(&name).is_none() {
        return Err(Error::UnknownIndex(name));
    }
    document.drop_if_empty("indices");
    document.save()?;
    ui::print_success(&format!("Removed index {name} from lpm.toml"));

    // Dependencies keyed to the removed index break at the next resolve;
    // point at them now rather than letting install fail mysteriously later.
    let dependents = manifest
        .dependencies
        .values()
        .filter(|dependency| {
            matches!(
                dependency,
                crate::project::manifest::Dependency::Registry { index: Some(index), .. }
                    if *index == name
            )
        })
        .count();
    if dependents > 0 {
        let (count, verb) = match dependents {
            1 => ("1 dependency".to_string(), "references"),
            n => (format!("{n} dependencies"), "reference"),
        };
        eprintln!("warning: {count} still {verb} index '{name}'; update them before `lpm install`");
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_names_resolve_to_urls() {
        assert_eq!(
            known_url("wally"),
            Some("https://github.com/UpliftGames/wally-index")
        );
        assert_eq!(
            known_url("pesde"),
            Some("https://github.com/pesde-pkg/index")
        );
        assert_eq!(known_url("custom"), None);
    }

    #[test]
    fn suggestions_filter_case_insensitively() {
        let mut suggestions = Suggestions(vec!["pesde".to_string(), "wally".to_string()]);
        assert_eq!(suggestions.get_suggestions("PES").unwrap(), vec!["pesde"]);
        assert_eq!(suggestions.get_suggestions("").unwrap().len(), 2);
        assert!(suggestions.get_suggestions("nope").unwrap().is_empty());
    }

    #[test]
    fn completion_prefers_highlight_then_single_match() {
        let mut suggestions = Suggestions(vec!["pesde".to_string(), "wally".to_string()]);
        assert_eq!(
            suggestions
                .get_completion("p", Some("wally".to_string()))
                .unwrap(),
            Some("wally".to_string())
        );
        assert_eq!(
            suggestions.get_completion("wal", None).unwrap(),
            Some("wally".to_string())
        );
        // Two candidates left: nothing to complete to yet.
        assert_eq!(suggestions.get_completion("", None).unwrap(), None);
    }
}
