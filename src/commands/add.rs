use crate::error::Error;
use crate::net::registry;
use crate::project::hooks::{self, Lifecycle};
use crate::project::manifest::edit::{ManifestDoc, Scope};
use crate::project::manifest::{
    DEFAULT_INDEX_URL, EMBER_INDEX_NAME, Manifest, parse_version_req, split_package_name,
};
use crate::registry::index::{Index, Refresh};
use crate::registry::local;
use crate::ui;
use clap::Args;
use inquire::validator::Validation;
use std::io::IsTerminal;

#[derive(Args, Debug)]
pub struct AddArgs {
    /** Package to add, as scope/name, a bare name for a package published
    with --local, or an unlisted package's link */
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

    let (package, key) = parse_link(&args.package)?;
    /* ember names always carry a scope, so a bare name can only mean a
    package that `embr publish --local` wrote */
    let is_local =
        !package.contains('/') && args.index.as_deref().is_none_or(|key| key == local::NAME);
    let name = if is_local {
        local::find(package)?
    } else {
        package.to_lowercase()
    };
    if let Some(key) = &key {
        registry::remember_key(&name, key);
    }
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
        // a link key is for the ember index only, so a link skips the prompt
        None if key.is_some() => {
            (manifest.index_url(None)? != DEFAULT_INDEX_URL).then(|| EMBER_INDEX_NAME.to_string())
        }
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
    if let Some(key) = &key {
        entry.insert("key", key.clone().into());
    }
    let dependencies = document.table_or_create("dependencies")?;
    /* re-adding an alias rewrites its whole entry, which is how a version
    bump works. `target` and `key` are the keys nothing here can re-ask
    for. dropping `target` would quietly move that dependency's whole
    subtree back into another environment root on the next install, and
    dropping `key` would lose the link. so they are carried over instead.
    see `Dependency::target` */
    if let Some(existing) = dependencies.get(&alias) {
        for carried in ["target", "key"] {
            if !entry.contains_key(carried)
                && let Some(value) = existing.get(carried).and_then(|value| value.as_str())
            {
                entry.insert(carried, value.to_string().into());
            }
        }
    }
    dependencies.insert(&alias, toml_edit::value(entry));
    document.save()?;

    ui::print_success(&format!("Added {name}@{} as '{alias}'", package.version));
    if key.is_some() {
        println!(
            "The link key is now in ember.toml, so a clone installs {name} with no setup. \
             Anyone who can read this project can use the link. In a public repo, that is everyone."
        );
    }
    println!("Run `embr install` to install it");

    // `postadd` sees the written manifest, so it can go straight to installing
    lifecycle.after()
}

/** splits an unlisted package's link into its name and its key. the link
is `https://emberpm.dev/package/acme/foo#k=<key>` or `acme/foo#k=<key>`. a
spec with no `#` is a plain name, with no key. */
fn parse_link(spec: &str) -> Result<(&str, Option<String>), Error> {
    let Some((name, fragment)) = spec.trim().split_once('#') else {
        return Ok((spec.trim(), None));
    };
    let name = name
        .strip_prefix("https://emberpm.dev/package/")
        .unwrap_or(name)
        .trim_end_matches('/');
    match fragment.strip_prefix("k=") {
        Some(key) if registry::is_link_key(key) => Ok((name, Some(key.to_ascii_lowercase()))),
        _ => Err(Error::LinkInvalid),
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    const KEY: &str = "0123456789abcdef0123456789abcdef";

    #[test]
    fn links_split_into_a_name_and_a_key() {
        let link = format!("https://emberpm.dev/package/acme/foo#k={KEY}");
        assert_eq!(
            parse_link(&link).unwrap(),
            ("acme/foo", Some(KEY.to_string()))
        );
        let short = format!("acme/foo#k={}", KEY.to_uppercase());
        assert_eq!(
            parse_link(&short).unwrap(),
            ("acme/foo", Some(KEY.to_string()))
        );
        let slash = format!("https://emberpm.dev/package/acme/foo/#k={KEY}");
        assert_eq!(parse_link(&slash).unwrap().0, "acme/foo");

        // a plain name has no key
        assert_eq!(parse_link("acme/foo").unwrap(), ("acme/foo", None));
        assert_eq!(parse_link("my-lib").unwrap(), ("my-lib", None));

        // a fragment that is not a whole key is refused, not dropped
        for bad in [
            "acme/foo#k=",
            "acme/foo#k=0123",
            "acme/foo#key=0123456789abcdef0123456789abcdef",
            "acme/foo#k=0123456789abcdef0123456789abcdeg",
            "acme/foo#k=0123456789abcdef0123456789abcdef0",
        ] {
            assert!(matches!(parse_link(bad), Err(Error::LinkInvalid)), "{bad}");
        }

        // another site's link keeps its host, which no package name has
        let other = format!("https://example.com/package/acme/foo#k={KEY}");
        assert!(split_package_name(parse_link(&other).unwrap().0).is_err());
    }
}
