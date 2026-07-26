use crate::error::Error;
use crate::project::manifest::edit::{ManifestDoc, Scope};
use crate::project::manifest::{Manifest, parse_version_req, split_package_name};
use crate::registry::index::Index;
use crate::ui;
use clap::Args;
use inquire::validator::Validation;

#[derive(Args, Debug)]
pub struct AddArgs {
    /// Package to add, as scope/name
    package: String,
    /// Version requirement (default "^", the latest)
    #[arg(long)]
    version: Option<String>,
    /// Index key from [indices]; skips the prompt
    #[arg(long)]
    index: Option<String>,
    /// Key written to [dependencies] (default: package short name)
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

    // resolve first so a typo'd package or version fails before we touch the manifest
    let index = Index::open(&index_url, true)?;
    let package = index.resolve(
        &name,
        &req,
        manifest.target.as_ref().map(|target| target.environment),
    )?;

    /* edit the raw document instead of re-serializing `manifest`,
    so comments and formatting in lpm.toml survive */
    let mut document = ManifestDoc::open(Scope::Project)?;

    let mut entry = toml_edit::InlineTable::new();
    entry.insert("name", name.clone().into());
    entry.insert("version", req_text.into());
    if let Some(key) = &index_key {
        entry.insert("index", key.clone().into());
    }
    document
        .table_or_create("dependencies")?
        .insert(&alias, toml_edit::value(entry));
    document.save()?;

    ui::print_success(&format!("Added {name}@{} as '{alias}'", package.version));
    println!("Run `lpm install` to install it");
    Ok(())
}

/** asks which index to search. empty input means the default index
(the `default` key under [indices] if set, else lpm's); anything else
has to be a key under [indices]. */
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
