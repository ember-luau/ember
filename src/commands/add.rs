use crate::error::Error;
use crate::index::Index;
use crate::manifest::{MANIFEST_FILE, Manifest, parse_version_req, split_package_name};
use crate::ui;
use clap::Args;
use inquire::validator::Validation;
use std::fs;

#[derive(Args, Debug)]
pub struct AddArgs {
    /// Package to add, as scope/name
    package: String,
    /// Version requirement (defaults to "^", the latest version)
    #[arg(long)]
    version: Option<String>,
    /// Index key from [indices]; skips the interactive prompt
    #[arg(long)]
    index: Option<String>,
    /// Dependency key written to [dependencies] (defaults to the package's short name)
    #[arg(long)]
    alias: Option<String>,
}

pub fn run(args: AddArgs) -> Result<(), Error> {
    let manifest = Manifest::load()?;

    let name = args.package.to_lowercase();
    let (_, short_name) = split_package_name(&name)?;
    let alias = args.alias.unwrap_or_else(|| short_name.to_string());

    let index_key = match args.index {
        Some(key) => Some(key),
        None => prompt_index_key(&manifest)?,
    };
    let index_url = manifest.index_url(index_key.as_deref())?.to_string();

    let req_text = args.version.unwrap_or_else(|| "^".to_string());
    let req = parse_version_req(&req_text)?;

    // Resolve now so a typo'd package or version fails before touching the manifest.
    let index = Index::open(&index_url, true)?;
    let package = index.resolve(
        &name,
        &req,
        manifest.target.as_ref().map(|target| target.environment),
    )?;

    // Edit the raw document instead of re-serializing `manifest` so comments
    // and formatting in lpm.toml survive.
    let mut document: toml_edit::DocumentMut = fs::read_to_string(MANIFEST_FILE)?.parse()?;
    let dependencies = document
        .entry("dependencies")
        .or_insert(toml_edit::Item::Table(toml_edit::Table::new()));
    let Some(table) = dependencies.as_table_mut() else {
        return Err(Error::ManifestInvalid(
            "[dependencies] is not a table".to_string(),
        ));
    };

    let mut entry = toml_edit::InlineTable::new();
    entry.insert("name", name.clone().into());
    entry.insert("version", req_text.into());
    if let Some(key) = &index_key {
        entry.insert("index", key.clone().into());
    }
    table.insert(&alias, toml_edit::value(entry));
    fs::write(MANIFEST_FILE, document.to_string())?;

    ui::print_success(&format!("Added {name}@{} as '{alias}'", package.version));
    println!("Run `lpm install` to install it");
    Ok(())
}

/// Asks which index to search. Empty input means the default index (the
/// `default` key under [indices] if set, otherwise LPM's); any other input
/// must be a key defined under [indices].
fn prompt_index_key(manifest: &Manifest) -> Result<Option<String>, Error> {
    inquire::set_global_render_config(crate::ui::render_config());

    let known_keys: Vec<String> = manifest.indices.keys().cloned().collect();
    let key = inquire::Text::new("index:")
        .with_help_message("Key from [indices] in lpm.toml; press enter to use LPM's index")
        .with_validator(move |input: &str| {
            let input = input.trim();
            if input.is_empty() || known_keys.iter().any(|key| key == input) {
                Ok(Validation::Valid)
            } else {
                Ok(Validation::Invalid(
                    format!("'{input}' is not defined under [indices] in lpm.toml").into(),
                ))
            }
        })
        .prompt()?;

    let key = key.trim();
    Ok((!key.is_empty()).then(|| key.to_string()))
}
