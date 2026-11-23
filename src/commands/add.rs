use crate::error::Error;
use crate::net::registry;
use crate::project::hooks::{self, Lifecycle};
use crate::project::manifest::edit::{ManifestDoc, Scope};
use crate::project::manifest::{
    DEFAULT_INDEX_URL, EMBER_INDEX_NAME, Environment, Language, Manifest, check_channel, features,
    parse_version_req, split_package_name, takes_channel,
};
use crate::registry::index::{Index, Refresh, ResolvedPackage};
use crate::registry::local;
use crate::registry::resolver::Allow;
use crate::ui;
use clap::Args;
use inquire::validator::Validation;
use std::collections::{HashMap, HashSet};

#[derive(Args, Debug)]
pub struct AddArgs {
    /** Packages to add. Each one is scope/name, scope/name@requirement, a
    bare name for a package published with --local, an unlisted package's
    link, or scope/ for every package of a scope */
    #[arg(required = true, value_name = "PACKAGE")]
    pub packages: Vec<String>,
    /// Version requirement, defaults to "^" meaning latest; for one package only
    #[arg(long)]
    pub version: Option<String>,
    /// Follow this channel of the package, such as `beta`, in place of a version requirement; for one package only
    #[arg(long, value_name = "NAME")]
    pub channel: Option<String>,
    /// Index key from [indices], skips the prompt
    #[arg(long)]
    pub index: Option<String>,
    /// Key written to [dependencies], defaults to the package short name; for one package only
    #[arg(long)]
    pub alias: Option<String>,
    /// Key the dependency by its full scope/name; its link file goes in a folder named after the scope
    #[arg(long, conflicts_with = "alias")]
    pub scoped: bool,
    /// Re-fetch the index even if it was refreshed recently
    #[arg(long)]
    pub refresh: bool,
    /// Write to [dev-dependencies], for the tests and tools of this project only; any language goes
    #[arg(long)]
    pub dev: bool,
    /// Write `optional = true`, so install skips the package when it does not resolve
    #[arg(long, conflicts_with_all = ["dev", "peer"])]
    pub optional: bool,
    /// Write to [peer-dependencies], for a package the projects that use this one must provide
    #[arg(long, conflicts_with = "dev")]
    pub peer: bool,
    /// Turn on these features of the package, comma-separated
    #[arg(long, value_delimiter = ',', value_name = "FEATURES")]
    pub features: Vec<String>,
    /// Turn off the features that the package's `default` lists
    #[arg(long)]
    pub no_default_features: bool,
    /// Add a whole scope without the question
    #[arg(long, short = 'y')]
    pub yes: bool,
    /// Print what went into ember.toml as one JSON document on stdout
    #[arg(long)]
    pub json: bool,
}

/// one argument of `embr add`.
enum Spec {
    Package(Item),
    /// `scope/` or `scope/*`: every package of the scope
    Scope(String),
}

/// one package of the run, before it resolves.
struct Item {
    /// the package as written: scope/name, or the bare name of a local package
    package: String,
    /// the key of an unlisted package's link
    key: Option<String>,
    /// the requirement written after `@`
    req: Option<String>,
}

/// one package that resolved, ready to go into ember.toml.
struct Planned {
    name: String,
    alias: String,
    version: semver::Version,
    entry: toml_edit::InlineTable,
    index_url: String,
    has_key: bool,
}

/** the indices that one run opens, by URL. several packages of one index
share one open, so the run fetches each index once. */
struct Indices {
    open: HashMap<String, Index>,
    mode: Refresh,
}

impl Indices {
    fn get(&mut self, url: &str) -> Result<&Index, Error> {
        if !self.open.contains_key(url) {
            let index = Index::open(url, self.mode)?;
            self.open.insert(url.to_string(), index);
        }
        Ok(&self.open[url])
    }

    fn force(&mut self, url: &str) -> Result<&Index, Error> {
        let index = Index::open(url, Refresh::Force)?;
        self.open.insert(url.to_string(), index);
        Ok(&self.open[url])
    }

    /* the TTL may have kept us on a stale index that just hasn't seen this
    package or version yet, so one forced refresh, one retry. the forced
    index replaces the stale one, so a run retries once for each index */
    fn resolve(
        &mut self,
        url: &str,
        name: &str,
        req: &semver::VersionReq,
        prefer: Option<(Environment, Language)>,
    ) -> Result<ResolvedPackage, Error> {
        let index = self.get(url)?;
        match index.resolve(name, req, prefer, &[]) {
            Err(_) if index.ttl_skipped() => self.force(url)?.resolve(name, req, prefer, &[]),
            result => result,
        }
    }

    /// the version that `channel` of `name` names, with the same retry as [`Indices::resolve`].
    fn channel(&mut self, url: &str, name: &str, channel: &str) -> Result<semver::Version, Error> {
        let index = self.get(url)?;
        match index.channel(name, channel) {
            Err(_) if index.ttl_skipped() => self.force(url)?.channel(name, channel),
            result => result,
        }
    }

    /// the packages of `scope`, with the same retry as [`Indices::resolve`].
    fn scope(&mut self, url: &str, scope: &str) -> Result<Vec<String>, Error> {
        let index = self.get(url)?;
        let mut names = index.scope(scope)?;
        if names.is_empty() && index.ttl_skipped() {
            names = self.force(url)?.scope(scope)?;
        }
        if names.is_empty() {
            return Err(Error::ScopeEmpty {
                scope: scope.to_string(),
                index: url.to_string(),
            });
        }
        Ok(names)
    }
}

pub fn run(args: AddArgs) -> Result<(), Error> {
    ui::json_or_text(args.json, || add(&args))
}

/// the `--json` document: each package that went into `table` of ember.toml.
fn added(table: &str, planned: &[Planned]) -> serde_json::Value {
    let added: Vec<serde_json::Value> = planned
        .iter()
        .map(|package| {
            let mut entry = serde_json::json!({
                "name": package.name,
                "alias": package.alias,
                "version": package.version.to_string(),
                "requirement": package.entry.get("version").and_then(|value| value.as_str()),
                "table": table,
            });
            // `requirement` is null beside a channel that has none
            if let Some(channel) = package
                .entry
                .get("channel")
                .and_then(|value| value.as_str())
            {
                entry["channel"] = channel.into();
            }
            entry
        })
        .collect();
    serde_json::json!({ "version": ui::JSON_VERSION, "added": added })
}

fn add(args: &AddArgs) -> Result<serde_json::Value, Error> {
    let manifest = Manifest::load()?;

    /* `preadd` runs before anything else, the index prompt included -- it is
    the hook for setting up whatever adding a package needs, so it has to be
    able to run before embr goes near an index */
    let lifecycle = Lifecycle::of(&manifest, hooks::ADD);
    lifecycle.before()?;

    /* every package resolves before the manifest changes. a failure does
    not stop the run: the user gets each failure of the batch in one go */
    let mut failures: Vec<Error> = Vec::new();
    let mut items: Vec<Item> = Vec::new();
    let mut scopes: Vec<String> = Vec::new();
    for spec in &args.packages {
        match parse_spec(spec) {
            Ok(Spec::Package(item)) => items.push(item),
            Ok(Spec::Scope(scope)) => scopes.push(scope),
            Err(error) => failures.push(error),
        }
    }
    if args.packages.len() > 1 || !scopes.is_empty() {
        if args.alias.is_some() {
            return Err(Error::AddOneOnly { flag: "--alias" });
        }
        if args.version.is_some() {
            return Err(Error::AddOneOnly { flag: "--version" });
        }
        if args.channel.is_some() {
            return Err(Error::AddOneOnly { flag: "--channel" });
        }
    }
    if let Some(channel) = &args.channel {
        check_channel(channel)?;
    }
    if args.version.is_some() && items.iter().any(|item| item.req.is_some()) {
        return Err(Error::AddVersionTwice(args.packages[0].clone()));
    }

    /* ember names always carry a scope, so a bare name can only mean a
    package that `embr publish --local` wrote */
    let is_local = |package: &str| {
        !package.contains('/') && args.index.as_deref().is_none_or(|key| key == local::NAME)
    };
    /* the prompt asks once for the whole run. a local package and a link
    name their index themselves, so a run of only those does not ask. no
    terminal (CI, a script) can't answer, so it gets the default index, as
    an empty answer would */
    let asks = args.index.is_none()
        && (!scopes.is_empty()
            || items
                .iter()
                .any(|item| item.key.is_none() && !is_local(&item.package)));
    let prompted = if asks && crate::project::settings::index_prompt() && ui::can_prompt() {
        prompt_index_key(&manifest)?
    } else {
        None
    };
    let index_key = |package: &str, has_key: bool| -> Result<Option<String>, Error> {
        Ok(match &args.index {
            Some(key) => Some(key.clone()),
            None if is_local(package) => Some(local::NAME.to_string()),
            // a link key is for the ember index only, so a link skips the prompt
            None if has_key => (manifest.index_url(None)? != DEFAULT_INDEX_URL)
                .then(|| EMBER_INDEX_NAME.to_string()),
            None => prompted.clone(),
        })
    };

    let table = match (args.dev, args.peer) {
        (true, _) => "dev-dependencies",
        (_, true) => "peer-dependencies",
        _ => "dependencies",
    };
    /* edit the raw document instead of re-serializing `manifest`,
    so comments and formatting in ember.toml survive */
    let mut document = ManifestDoc::open(Scope::Project)?;

    let mut indices = Indices {
        open: HashMap::new(),
        mode: if args.refresh {
            Refresh::Force
        } else {
            Refresh::Ttl
        },
    };

    /* a scope adds the packages that the table does not have yet. a package
    that the run also names alone keeps the requirement written there */
    let present: HashSet<String> = document
        .table(table)?
        .map(|entries| {
            entries
                .iter()
                .filter_map(|(_, entry)| entry.get("name")?.as_str().map(str::to_lowercase))
                .collect()
        })
        .unwrap_or_default();
    let mut listed: HashSet<String> = items
        .iter()
        .map(|item| item.package.to_lowercase())
        .collect();
    for scope in &scopes {
        let key = index_key(&format!("{scope}/"), false)?;
        let url = manifest.index_url(key.as_deref())?.to_string();
        let names = match indices.scope(&url, scope) {
            Ok(names) => names,
            Err(error) => {
                failures.push(error);
                continue;
            }
        };
        let new: Vec<String> = names
            .into_iter()
            .filter(|name| !present.contains(name) && !listed.contains(name))
            .collect();
        if new.is_empty() {
            println!("Every package of scope '{scope}' is already in [{table}]");
            continue;
        }
        println!(
            "Scope '{scope}' has {} to add:",
            match new.len() {
                1 => "1 package".to_string(),
                count => format!("{count} packages"),
            }
        );
        for name in &new {
            println!("  {name}");
        }
        if !args.yes {
            if !ui::can_prompt() {
                return Err(Error::AddNeedsYes(scope.clone()));
            }
            inquire::set_global_render_config(crate::ui::render_config());
            let question = if new.len() == 1 {
                "Add it?"
            } else {
                "Add them?"
            };
            if !inquire::Confirm::new(question)
                .with_default(true)
                .prompt()?
            {
                println!("Nothing added");
                return Ok(added(table, &[]));
            }
        }
        for name in new {
            listed.insert(name.clone());
            items.push(Item {
                package: name,
                key: None,
                req: None,
            });
        }
    }

    let prefer = manifest
        .target()
        .map(|target| (target.environment, target.language));
    let allow = Allow::for_project(&manifest).map(|allow| Allow {
        any_language: args.dev,
        ..allow
    });
    let mut plan = |item: &Item| -> Result<Planned, Error> {
        let name = if is_local(&item.package) {
            local::find(&item.package)?
        } else {
            item.package.to_lowercase()
        };
        if let Some(key) = &item.key {
            registry::remember_key(&name, key);
        }
        let (_, short_name) = split_package_name(&name)?;
        let alias = match &args.alias {
            Some(alias) => alias.clone(),
            // `mayari/core` and `chief/core` can then both be dependencies (#45)
            None if args.scoped => name.clone(),
            None => short_name.to_string(),
        };
        let index_key = index_key(&item.package, item.key.is_some())?;
        let index_url = manifest.index_url(index_key.as_deref())?.to_string();
        let stated = item.req.clone().or_else(|| args.version.clone());
        let req_text = stated.clone().unwrap_or_else(|| "^".to_string());
        let req = parse_version_req(&req_text)?;
        /* a channel picks the one version that it names now. the entry
        keeps the channel, and a requirement only when the user gave one */
        let req = match &args.channel {
            Some(channel) => {
                let version = indices.channel(&index_url, &name, channel)?;
                if !takes_channel(&req, &version) {
                    return Err(Error::ChannelMismatch {
                        name,
                        channel: channel.clone(),
                        version: version.to_string(),
                        req: req_text,
                    });
                }
                parse_version_req(&format!("={version}"))?
            }
            None => req,
        };

        let package = indices.resolve(&index_url, &name, &req, prefer)?;
        if let Some(allow) = allow
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

        let mut entry = toml_edit::InlineTable::new();
        entry.insert("name", name.clone().into());
        match &args.channel {
            Some(channel) => {
                if let Some(stated) = stated {
                    entry.insert("version", stated.into());
                }
                entry.insert("channel", channel.clone().into());
            }
            None => {
                entry.insert("version", req_text.into());
            }
        }
        if let Some(key) = &index_key {
            entry.insert("index", key.clone().into());
        }
        if let Some(key) = &item.key {
            entry.insert("key", key.clone().into());
        }
        if args.optional {
            entry.insert("optional", true.into());
        }
        // a feature that the version does not have would fail the next install
        let known =
            |feature: &str| feature == features::DEFAULT || package.features.contains_key(feature);
        if let Some(feature) = args.features.iter().find(|feature| !known(feature)) {
            let names: Vec<&str> = package
                .features
                .keys()
                .map(String::as_str)
                .filter(|name| *name != features::DEFAULT)
                .collect();
            return Err(Error::FeatureUnknown {
                package: format!("{name}@{}", package.version),
                feature: feature.clone(),
                by: "--features".to_string(),
                known: match names.as_slice() {
                    [] => "it has no features".to_string(),
                    names => format!("its features are {}", names.join(", ")),
                },
            });
        }
        if !args.features.is_empty() {
            entry.insert(
                "features",
                toml_edit::Value::Array(args.features.iter().collect()),
            );
        }
        if args.no_default_features {
            entry.insert("default-features", false.into());
        }
        Ok(Planned {
            name,
            alias,
            version: package.version,
            entry,
            index_url,
            has_key: item.key.is_some(),
        })
    };
    let mut planned: Vec<Planned> = Vec::new();
    for item in &items {
        match plan(item) {
            // the same package named twice is one package
            Ok(package) if planned.iter().any(|other| other.name == package.name) => {}
            Ok(package) => planned.push(package),
            Err(error) => failures.push(error),
        }
    }

    let mut aliases: HashMap<&str, &str> = HashMap::new();
    for package in &planned {
        /* the manifest refuses an alias in [dependencies] and in one of the
        other two tables, so the edit refuses it first */
        for other in ["dependencies", "dev-dependencies", "peer-dependencies"] {
            let clash = (table == "dependencies") != (other == "dependencies");
            if clash
                && document
                    .table(other)?
                    .is_some_and(|entries| entries.contains_key(&package.alias))
            {
                failures.push(Error::AliasInBothTables {
                    alias: package.alias.clone(),
                    table: if table == "dependencies" {
                        other
                    } else {
                        table
                    },
                });
            }
        }
        // `a/signal` and `b/signal` would write one key twice
        if let Some(first) = aliases.insert(&package.alias, &package.name) {
            failures.push(Error::AddAliasTwice {
                alias: package.alias.clone(),
                first: first.to_string(),
                second: package.name.clone(),
            });
        }
    }
    if failures.len() == 1 {
        return Err(failures.remove(0));
    }
    if !failures.is_empty() {
        return Err(Error::AddFailed(failures));
    }
    if planned.is_empty() {
        return Ok(added(table, &[]));
    }

    let dependencies = document.table_or_create(table)?;
    for package in &mut planned {
        let entry = &mut package.entry;
        /* re-adding an alias rewrites its whole entry, which is how a version
        bump works. `target`, `key`, `optional`, `features` and
        `default-features` are carried over when this run does not set them.
        dropping `target` would quietly move that dependency's whole subtree
        back into another environment root on the next install, dropping `key`
        would lose the link, and a version bump must not make an optional
        dependency required or turn its features off. see `Dependency::target` */
        if let Some(existing) = dependencies.get(&package.alias) {
            for carried in ["target", "key"] {
                if !entry.contains_key(carried)
                    && let Some(value) = existing.get(carried).and_then(|value| value.as_str())
                {
                    entry.insert(carried, value.to_string().into());
                }
            }
            if existing.get("optional").and_then(|value| value.as_bool()) == Some(true) {
                entry.insert("optional", true.into());
            }
            for carried in ["features", "default-features"] {
                if !entry.contains_key(carried)
                    && let Some(value) = existing.get(carried).and_then(|value| value.as_value())
                {
                    entry.insert(carried, value.clone());
                }
            }
        }
        dependencies.insert(&package.alias, toml_edit::value(entry.clone()));
    }
    // one write for the whole run
    document.save()?;

    for package in &planned {
        let Planned {
            name,
            alias,
            version,
            ..
        } = package;
        ui::print_success(&format!("Added {name}@{version} as '{alias}'"));
        if package.has_key {
            println!(
                "The link key is now in ember.toml, so a clone installs {name} with no setup. \
                 Anyone who can read this project can use the link. In a public repo, that is everyone."
            );
        }
    }
    println!(
        "Run `embr install` to install {}",
        if planned.len() == 1 { "it" } else { "them" }
    );
    for package in &planned {
        crate::registry::notices::report_added(&package.index_url, &package.name, &package.version);
    }

    // `postadd` sees the written manifest, so it can go straight to installing
    lifecycle.after()?;
    Ok(added(table, &planned))
}

/// reads one argument. `scope/` and `scope/*` name a whole scope. a
/// requirement goes after `@`: `acme/foo@^1.2`. a link has no `@` part.
fn parse_spec(spec: &str) -> Result<Spec, Error> {
    let (package, key) = parse_link(spec)?;
    if key.is_none()
        && let Some(scope) = package
            .strip_suffix("/*")
            .or_else(|| package.strip_suffix('/'))
    {
        return Ok(Spec::Scope(scope.to_lowercase()));
    }
    let (package, req) = match package.split_once('@') {
        Some((package, req)) if key.is_none() => (package, Some(req.to_string())),
        _ => (package, None),
    };
    Ok(Spec::Package(Item {
        package: package.to_string(),
        key,
        req,
    }))
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

    #[test]
    fn an_argument_is_a_package_a_requirement_or_a_scope() {
        let package = |spec: &str| match parse_spec(spec).unwrap() {
            Spec::Package(item) => (item.package, item.req),
            Spec::Scope(scope) => panic!("{spec} read as scope {scope}"),
        };
        assert_eq!(package("acme/foo"), ("acme/foo".to_string(), None));
        assert_eq!(
            package("acme/foo@^1.2"),
            ("acme/foo".to_string(), Some("^1.2".to_string()))
        );
        assert_eq!(package("my-lib"), ("my-lib".to_string(), None));

        for spec in ["acme/", "acme/*", "Acme/"] {
            assert!(
                matches!(parse_spec(spec), Ok(Spec::Scope(scope)) if scope == "acme"),
                "{spec}"
            );
        }

        // a link ends in a key and never names a scope
        let link = format!("https://emberpm.dev/package/acme/foo/#k={KEY}");
        assert!(matches!(parse_spec(&link), Ok(Spec::Package(item)) if item.key.is_some()));
    }
}
