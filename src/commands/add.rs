use crate::error::Error;
use crate::project::hooks::{self, Lifecycle};
use crate::project::manifest::edit::{ManifestDoc, Scope};
use crate::project::manifest::{Manifest, parse_version_req, split_package_name};
use crate::registry::index::{Index, Refresh};
use crate::registry::local;
use crate::ui;
use clap::Args;
use inquire::validator::Validation;
use std::io::IsTerminal;

#[derive(Args, Debug)]
pub struct AddArgs {
    /// Package to add, as scope/name, or a bare name for a package published with --local
    pub package: String,
    /// Version requirement, defaults to "^" meaning latest
    #[arg(long)]
    pub version: Option<String>,
    /// Index key from [indices], skips the prompt
    #[arg(long)]
    pub index: Option<String>,
    /// Key written to [dependencies], defaults to the package short name
    #[arg(long)]
    pub alias: Option<String>,
    /// Key the dependency by its full scope/name; its link file goes in a folder named after the scope
    #[arg(long, conflicts_with = "alias")]
    pub scoped: bool,
    /// Re-fetch the index even if it was refreshed recently
    #[arg(long)]
    pub refresh: bool,
}

pub fn run(args: AddArgs) -> Result<(), Error> {
    let manifest = Manifest::load()?;

    /* `preadd` runs before anything else, the index prompt included -- it is
    the hook for setting up whatever adding a package needs, so it has to be
    able to run before embr goes near an index */
    let lifecycle = Lifecycle::of(&manifest, hooks::ADD);
    lifecycle.before()?;

    /* ember names always carry a scope, so a bare name can only mean a
    package that `embr publish --local` wrote */
    let is_local =
        !args.package.contains('/') && args.index.as_deref().is_none_or(|key| key == local::NAME);
    let name = if is_local {
        local::find(&args.package)?
    } else {
        args.package.to_lowercase()
    };
    let (_, short_name) = split_package_name(&name)?;
    let alias = match args.alias {
        Some(alias) => alias,
        // `mayari/core` and `chief/core` can then both be dependencies (#45)
        None if args.scoped => name.clone(),
        None => short_name.to_string(),
    };

    let index_key = match args.index {
        Some(key) => Some(key),
        None if is_local => Some(local::NAME.to_string()),
        /* no terminal (CI, a script) can't answer, so it gets the default
        index, as an empty answer would */
        None if crate::project::settings::index_prompt() && std::io::stdin().is_terminal() => {
            prompt_index_key(&manifest)?
        }
        None => None,
    };
    let index_url = manifest.index_url(index_key.as_deref())?.to_string();

    let req_text = args.version.unwrap_or_else(|| "^".to_string());
    let req = parse_version_req(&req_text)?;

    // resolve first so a typo'd package or version fails before we touch the manifest
    let mode = if args.refresh {
        Refresh::Force
    } else {
        Refresh::Ttl
    };
    let prefer = manifest
        .target()
        .map(|target| (target.environment, target.language));
    let index = Index::open(&index_url, mode)?;
    let package = match index.resolve(&name, &req, prefer, &[]) {
        Ok(package) => package,
        /* the TTL may have kept us on a stale index that just hasn't
        seen this package or version yet, so one forced refresh, one retry */
        Err(_) if index.ttl_skipped() => {
            Index::open(&index_url, Refresh::Force)?.resolve(&name, &req, prefer, &[])?
        }
        Err(error) => return Err(error),
    };
    if let Some(allow) = crate::registry::resolver::Allow::for_project(&manifest)
        && !allow.admits(
            package.language,
            package.environment,
            Some(allow.environment),
        )
    {
        return Err(allow.refusal(
            format!("{name}@{}", package.version),
            package.language,
            package.environment,
        ));
    }

    /* edit the raw document instead of re-serializing `manifest`,
    so comments and formatting in ember.toml survive */
    let mut document = ManifestDoc::open(Scope::Project)?;

    let mut entry = toml_edit::InlineTable::new();
    entry.insert("name", name.clone().into());
    entry.insert("version", req_text.into());
    if let Some(key) = &index_key {
        entry.insert("index", key.clone().into());
    }
    let dependencies = document.table_or_create("dependencies")?;
    /* re-adding an alias rewrites its whole entry, which is how a version
    bump works. `target` is the one key nothing here can re-ask for, and
    dropping it would quietly move that dependency's whole subtree back
    into another environment root on the next install, so it is carried
    over instead. see `Dependency::target` */
    if let Some(target) = dependencies
        .get(&alias)
        .and_then(|existing| existing.get("target"))
        .and_then(|target| target.as_str())
    {
        entry.insert("target", target.to_string().into());
    }
    dependencies.insert(&alias, toml_edit::value(entry));
    document.save()?;

    ui::print_success(&format!("Added {name}@{} as '{alias}'", package.version));
    println!("Run `embr install` to install it");

    // `postadd` sees the written manifest, so it can go straight to installing
    lifecycle.after()
}

/** asks which index to search. empty input means the default index,
the `default` key under [indices] if set, else embr's. anything else
has to be a key under [indices], `ember`, or `local`. */
fn prompt_index_key(manifest: &Manifest) -> Result<Option<String>, Error> {
    inquire::set_global_render_config(crate::ui::render_config());

    let mut known_keys: Vec<String> = manifest.indices.keys().cloned().collect();
    known_keys.push(local::NAME.to_string());
    known_keys.push(crate::project::manifest::EMBER_INDEX_NAME.to_string());
    let key = inquire::Text::new("index:")
        .with_help_message(
            "Key from [indices] in ember.toml, ember, or local; press enter to use the default index",
        )
        .with_validator(move |input: &str| {
            let input = input.trim();
            if input.is_empty() || known_keys.iter().any(|key| key == input) {
                Ok(Validation::Valid)
            } else {
                Ok(Validation::Invalid(
                    format!("'{input}' is not defined under [indices] in ember.toml").into(),
                ))
            }
        })
        .prompt()?;

    let key = key.trim();
    Ok((!key.is_empty()).then(|| key.to_string()))
}
