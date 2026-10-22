use super::{DownloadSource, ResolvedPackage, TransitiveDependency};
use crate::error::Error;
use crate::project::manifest::{Environment, Language, split_package_name};
use serde::Deserialize;
use std::path::Path;
use toml::Value;

/// How a pesde registry serves package archives.
const DOWNLOAD_TEMPLATE: &str =
    "{API_URL}/v1/packages/{PACKAGE}/{PACKAGE_VERSION}/{PACKAGE_TARGET}/archive";

/** Root config.toml of a pesde-format index. embr's own index is this format
too, except its entries each carry a `download` URL, so it needs no `api`. */
#[derive(Deserialize)]
pub struct Config {
    /** Registry the download URLs are built against. Optional so a
    config.toml that predates it still parses. */
    #[serde(default)]
    pub api: Option<String>,
    /** GitHub OAuth app publishes authenticate against, via the device
    flow. Real pesde indices spell this `github_oauth_client_id`. */
    #[serde(default, alias = "github_oauth_client_id")]
    pub github_oauth_id: Option<String>,
}

pub fn load_config(root: &Path) -> Result<Config, Error> {
    Ok(toml::from_str(&std::fs::read_to_string(
        root.join("config.toml"),
    )?)?)
}

/// one entry of a package file: a version, published for one target.
pub struct Candidate {
    pub version: semver::Version,
    /** Raw target ID like "luau", "roblox", or "roblox-alloy", from the
    entry key, else from the entry's own target. Fills {PACKAGE_TARGET} in
    download URLs. */
    pub target: Option<String>,
    pub environment: Option<Environment>,
    pub language: Language,
    pub yanked: bool,
    pub deprecated: Option<String>,
    pub entry: Value,
}

/** the highest version of `name` that `req` matches, and of that version
the target that suits `prefer`, a project's environment and language, see
[`preferred`]. */
pub fn resolve(
    root: &Path,
    index_url: &str,
    config: &Config,
    name: &str,
    req: &semver::VersionReq,
    prefer: Option<(Environment, Language)>,
    pinned: &[semver::Version],
) -> Result<ResolvedPackage, Error> {
    let (candidates, package_deprecated) = read(root, index_url, name)?;
    /* cargo's rule: a yanked version stays out of fresh picks, but a
    lockfile that already pins it keeps installing it. pesde writes the
    same `yanked = true`, so its own yanks count too. */
    let candidates: Vec<Candidate> = candidates
        .into_iter()
        .filter(|c| req.matches(&c.version) && (!c.yanked || pinned.contains(&c.version)))
        .collect();

    let Some(best_version) = candidates.iter().map(|c| &c.version).max().cloned() else {
        return Err(Error::NoMatchingVersion {
            name: name.to_string(),
            req: req.to_string(),
        });
    };

    let mut same_version: Vec<Candidate> = candidates
        .into_iter()
        .filter(|c| c.version == best_version)
        .collect();
    let picked = prefer
        .and_then(|(environment, language)| preferred(&same_version, environment, language))
        .unwrap_or(0);
    let candidate = same_version.swap_remove(picked);

    let dependencies = parse_dependencies(&candidate.entry, index_url)?;
    let source = download_source(config, index_url, name, &candidate)?;

    Ok(ResolvedPackage {
        version: candidate.version,
        environment: candidate.environment,
        language: candidate.language,
        yanked: candidate.yanked,
        deprecated: candidate.deprecated,
        package_deprecated,
        dependencies,
        source,
    })
}

/** which of one version's `targets` a project in `environment` written in
`language` takes. the order of preference is the exact language, then
plain Luau, then any language that `language` accepts, each in
`environment`. None leaves the choice to the caller. */
fn preferred(targets: &[Candidate], environment: Environment, language: Language) -> Option<usize> {
    let rank = |candidate: &Candidate| {
        if candidate.environment != Some(environment) {
            None
        } else if candidate.language == language {
            Some(0)
        } else if candidate.language == Language::Luau {
            Some(1)
        } else if language.accepts(candidate.language) {
            Some(2)
        } else {
            None
        }
    };
    targets
        .iter()
        .enumerate()
        .filter_map(|(index, candidate)| rank(candidate).map(|rank| (rank, index)))
        .min()
        .map(|(_, index)| index)
}

/** every entry of `name` that this embr can read, yanked ones included,
in file order, plus the package-wide `[meta] deprecated` reason. resolve
picks from these, and `embr mcp` lists them. */
pub fn read(
    root: &Path,
    index_url: &str,
    name: &str,
) -> Result<(Vec<Candidate>, Option<String>), Error> {
    let (scope, package) = split_package_name(name)?;
    let path = root.join(scope).join(package);
    if !path.exists() {
        return Err(Error::PackageNotFound {
            name: name.to_string(),
            index: index_url.to_string(),
        });
    }

    let file: Value = toml::from_str(&std::fs::read_to_string(&path)?)?;
    /* Newer pesde files nest entries under an "entries" table with sibling
    metadata like `meta`, older ones put them at the top level. Keys are
    "<version> <target>", the target part optional in embr indices. A target
    is "<environment>" for Luau, else "<environment>-<language>". */
    let entries = match file.get("entries") {
        Some(Value::Table(entries)) => entries,
        _ => file.as_table().ok_or_else(|| Error::PackageNotFound {
            name: name.to_string(),
            index: index_url.to_string(),
        })?,
    };

    /* pesde writes an empty string for "not deprecated", so only a
    non-empty reason counts, on the file's [meta] and on an entry alike */
    let reason = |value: Option<&Value>| {
        value
            .and_then(Value::as_str)
            .filter(|reason| !reason.trim().is_empty())
            .map(str::to_string)
    };
    let package_deprecated = reason(file.get("meta").and_then(|meta| meta.get("deprecated")));

    let mut candidates: Vec<Candidate> = Vec::new();
    for (key, entry) in entries {
        let (version_part, key_target) = match key.split_once(' ') {
            Some((version, target)) => (version, Some(target)),
            None => (key.as_str(), None),
        };
        let Ok(version) = semver::Version::parse(version_part) else {
            continue; // not a version entry, like a metadata table
        };

        /* pesde environments use `_`, never `-`, so the first `-` of a
        key's target ends the environment */
        let (key_environment, key_language) = match key_target {
            Some(target) => match target.split_once('-') {
                Some((environment, language)) => (Some(environment), Some(language)),
                None => (Some(target), None),
            },
            None => (None, None),
        };
        /* The entry's own target is authoritative, a table with an
        `environment` for pesde or a bare string for embr API entries. The
        key's target part covers entries that don't carry one. */
        let entry_target = entry.get("target").and_then(|target| match target {
            Value::String(target) => Some(target.as_str()),
            table => table.get("environment").and_then(Value::as_str),
        });
        let environment = entry_target.or(key_environment);
        let Ok(environment) = environment.map(parse_environment).transpose() else {
            continue; // published for a target embr doesn't support
        };
        let yanked = entry.get("yanked").and_then(Value::as_bool) == Some(true);
        let language = entry
            .get("language")
            .and_then(Value::as_str)
            .or(key_language)
            .map_or(Some(Language::Luau), Language::parse);
        let Some(language) = language else {
            continue; // a language this embr does not know
        };

        candidates.push(Candidate {
            version,
            target: key_target.or(entry_target).map(str::to_string),
            environment,
            language,
            yanked,
            deprecated: reason(entry.get("deprecated")),
            entry: entry.clone(),
        });
    }

    Ok((candidates, package_deprecated))
}

/// Accepts both embr environment names and pesde's, "roblox" -> shared and so on.
pub(crate) fn parse_environment(environment: &str) -> Result<Environment, Error> {
    Environment::from_embr(environment).or_else(|_| Environment::from_pesde(environment))
}

fn download_source(
    config: &Config,
    index_url: &str,
    name: &str,
    candidate: &Candidate,
) -> Result<DownloadSource, Error> {
    /* embr-index entries bake in their download URL, which lets
    installs work without a registry server being up. pesde entries
    build theirs from the registry template. */
    if let Some(url) = candidate.entry.get("download").and_then(Value::as_str) {
        return Ok(DownloadSource::TarGz {
            url: url.to_string(),
        });
    }

    let mut url = DOWNLOAD_TEMPLATE
        .replace("{PACKAGE}", &name.replace('/', "%2F"))
        .replace("{PACKAGE_VERSION}", &candidate.version.to_string());

    // Only demand what the template actually references.
    if url.contains("{API_URL}") {
        let Some(api) = config.api.as_deref() else {
            return Err(Error::IndexFetch {
                url: index_url.to_string(),
                reason: format!(
                    "entry for {name} has no download url and the index config has no api"
                ),
            });
        };
        url = url.replace("{API_URL}", api.trim_end_matches('/'));
    }
    if url.contains("{PACKAGE_TARGET}") {
        let Some(target) = candidate.target.as_deref() else {
            return Err(Error::IndexFetch {
                url: index_url.to_string(),
                reason: format!("entry for {name} names no target for the download template"),
            });
        };
        url = url.replace("{PACKAGE_TARGET}", target);
    }

    Ok(DownloadSource::TarGz { url })
}

pub fn parse_dependencies(
    entry: &Value,
    index_url: &str,
) -> Result<Vec<TransitiveDependency>, Error> {
    let Some(Value::Table(dependencies)) = entry.get("dependencies") else {
        return Ok(Vec::new());
    };

    let mut parsed = Vec::new();
    for (alias, spec) in dependencies {
        /* Specs are either the specifier table itself or a [specifier, kind]
        pair, kind being "standard", "peer", or "dev". */
        let (spec, kind) = match spec {
            Value::Array(pair) => match pair.first() {
                Some(first) => (first, pair.get(1).and_then(Value::as_str)),
                None => continue,
            },
            other => (other, None),
        };

        // Dev dependencies of upstream packages are not ours to install.
        if kind == Some("dev") {
            continue;
        }
        // Workspace/git specifiers can't be resolved from an index clone.
        if spec.get("workspace").is_some() || spec.get("repo").is_some() {
            continue;
        }

        let is_wally = spec.get("wally").is_some();
        let Some(name) = spec
            .get("name")
            .or_else(|| spec.get("wally"))
            .and_then(Value::as_str)
        else {
            continue;
        };
        // pesde serializes wally package names with a "wally#" prefix.
        let name = name.strip_prefix("wally#").unwrap_or(name);
        let version_req = spec
            .get("version")
            .and_then(Value::as_str)
            .unwrap_or("*")
            .to_string();
        let index = spec.get("index").and_then(Value::as_str);

        /* Published entries name a dependency's index by URL. "default" or
        nothing means the index the entry itself came from. */
        let dep_index_url = match index {
            Some(url) if url.starts_with("http://") || url.starts_with("https://") => {
                Some(url.to_string())
            }
            None | Some("default") => {
                if is_wally {
                    // A wally dep can't live in this pesde index.
                    return Err(Error::UnknownIndex(format!(
                        "wally dependency {name} (from an entry in {index_url}) names no index url"
                    )));
                }
                None
            }
            Some(alias) => return Err(Error::UnknownIndex(alias.to_string())),
        };

        parsed.push(TransitiveDependency {
            // Old wally entries may carry uppercase names, normalize.
            name: name.to_lowercase(),
            version_req,
            index_url: dep_index_url,
            alias: alias.clone(),
        });
    }

    Ok(parsed)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse_entries(toml_text: &str) -> Value {
        toml::from_str(toml_text).unwrap()
    }

    /// On-disk index rooted in the system temp dir, removed on drop.
    struct TempIndex {
        root: std::path::PathBuf,
    }

    impl TempIndex {
        fn new(name: &str, config: &str) -> Self {
            let dir = format!("embr-pesde-test-{name}-{}", std::process::id());
            let root = std::env::temp_dir().join(dir);
            let _ = std::fs::remove_dir_all(&root);
            std::fs::create_dir_all(&root).unwrap();
            std::fs::write(root.join("config.toml"), config).unwrap();
            TempIndex { root }
        }

        fn write_package(&self, name: &str, contents: &str) {
            let path = self.root.join(name);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, contents).unwrap();
        }
    }

    impl Drop for TempIndex {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.root);
        }
    }

    #[test]
    fn parses_environments_from_both_namings() {
        assert_eq!(parse_environment("shared").unwrap(), Environment::Roblox);
        assert_eq!(parse_environment("roblox").unwrap(), Environment::Roblox);
        assert_eq!(
            parse_environment("roblox_server").unwrap(),
            Environment::Server
        );
        assert_eq!(parse_environment("lute").unwrap(), Environment::Lute);
        assert!(parse_environment("cobol").is_err());
    }

    #[test]
    fn config_parses_both_oauth_spellings_and_tolerates_extras() {
        /* Real pesde configs spell the oauth id the long way and carry other
        registry settings embr ignores. */
        let config: Config = toml::from_str(
            r#"
            api = "https://registry.example.com"
            github_oauth_client_id = "Iv1.abc123"
            other_registry_setting = true
            "#,
        )
        .unwrap();
        assert_eq!(config.api.as_deref(), Some("https://registry.example.com"));
        assert_eq!(config.github_oauth_id.as_deref(), Some("Iv1.abc123"));

        // embr's own index spells it github_oauth_id and has no api.
        let config: Config = toml::from_str(r#"github_oauth_id = "Ov23abc""#).unwrap();
        assert_eq!(config.api, None);
        assert_eq!(config.github_oauth_id.as_deref(), Some("Ov23abc"));

        let config: Config = toml::from_str("").unwrap();
        assert_eq!(config.api, None);
        assert_eq!(config.github_oauth_id, None);
    }

    #[test]
    fn parses_pesde_style_dependencies() {
        let entry = parse_entries(
            r#"
            [dependencies]
            foo = { name = "acme/foo", version = "^1.0.0" }
            bar = [{ name = "acme/bar", version = "2.0.0", index = "https://github.com/acme/index" }, "standard"]
            dev_only = [{ name = "acme/testkit", version = "^1.0.0" }, "dev"]
            "#,
        );

        let deps = parse_dependencies(&entry, "https://example.com/index").unwrap();
        assert_eq!(deps.len(), 2);
        let foo = deps.iter().find(|d| d.name == "acme/foo").unwrap();
        assert_eq!(foo.version_req, "^1.0.0");
        assert_eq!(foo.index_url, None);
        let bar = deps.iter().find(|d| d.name == "acme/bar").unwrap();
        assert_eq!(
            bar.index_url.as_deref(),
            Some("https://github.com/acme/index")
        );
    }

    #[test]
    fn wally_dependency_requires_index_url() {
        let entry = parse_entries(
            r#"
            [dependencies]
            promise = { wally = "wally#evaera/Promise", version = "^4.0.0", index = "https://github.com/UpliftGames/wally-index" }
            "#,
        );
        let deps = parse_dependencies(&entry, "https://example.com/index").unwrap();
        assert_eq!(deps[0].name, "evaera/promise");
        assert_eq!(
            deps[0].index_url.as_deref(),
            Some("https://github.com/UpliftGames/wally-index")
        );

        let bad = parse_entries(
            r#"
            [dependencies]
            promise = { wally = "evaera/promise", version = "^4.0.0" }
            "#,
        );
        assert!(parse_dependencies(&bad, "https://example.com/index").is_err());
    }

    #[test]
    fn builds_download_url_from_the_registry_template() {
        let config = Config {
            api: Some("https://registry.example.com/".to_string()),
            github_oauth_id: None,
        };
        let candidate = Candidate {
            version: semver::Version::new(1, 0, 2),
            target: Some("luau".to_string()),
            environment: Some(Environment::Luau),
            language: Language::Luau,
            yanked: false,
            deprecated: None,
            entry: parse_entries(""),
        };

        let source = download_source(
            &config,
            "https://example.com/index",
            "pesde/hello",
            &candidate,
        )
        .unwrap();
        let DownloadSource::TarGz { url } = source else {
            panic!("expected tarball source");
        };
        assert_eq!(
            url,
            "https://registry.example.com/v1/packages/pesde%2Fhello/1.0.2/luau/archive"
        );
    }

    #[test]
    fn entry_download_url_wins_over_the_template() {
        /* embr's own index. entries carry their download URL, so no `api` is
        needed, and one being set wouldn't override the entry. */
        let config = Config {
            api: Some("https://registry.example.com".to_string()),
            github_oauth_id: None,
        };
        let candidate = Candidate {
            version: semver::Version::new(1, 2, 3),
            target: Some("shared".to_string()),
            environment: Some(Environment::Roblox),
            language: Language::Luau,
            yanked: false,
            deprecated: None,
            entry: parse_entries(r#"download = "https://cdn.luaupm.com/scope/pkg/1.2.3.tar.gz""#),
        };

        let source = download_source(
            &config,
            "https://github.com/luaupm/index",
            "scope/pkg",
            &candidate,
        )
        .unwrap();
        let DownloadSource::TarGz { url } = source else {
            panic!("expected tarball source");
        };
        assert_eq!(url, "https://cdn.luaupm.com/scope/pkg/1.2.3.tar.gz");
    }

    #[test]
    fn entries_without_a_download_or_api_cannot_be_downloaded() {
        let config = Config {
            api: None,
            github_oauth_id: None,
        };
        let candidate = Candidate {
            version: semver::Version::new(1, 2, 3),
            target: Some("luau".to_string()),
            environment: Some(Environment::Luau),
            language: Language::Luau,
            yanked: false,
            deprecated: None,
            entry: parse_entries(""),
        };

        assert!(matches!(
            download_source(
                &config,
                "https://example.com/index",
                "scope/pkg",
                &candidate
            ),
            Err(Error::IndexFetch { .. })
        ));
    }

    #[test]
    fn resolves_embr_index_entries_with_baked_downloads() {
        /* The shape the embr API writes. no root `api`, per-entry `download`,
        target as a table with just `environment`. */
        let index = TempIndex::new("embr-format", r#"github_oauth_id = "Ov23abc""#);
        index.write_package(
            "chief/core",
            r#"
            ["0.1.0 shared"]
            download = "https://cdn.luaupm.com/chief/core/0.1.0.tar.gz"
            description = "Module framework core"

            ["0.1.0 shared".target]
            environment = "shared"

            ["0.1.0 shared".dependencies]
            bin = { name = "chief/bin", version = "^0.1.0" }
            "#,
        );

        let config = load_config(&index.root).unwrap();
        let resolved = resolve(
            &index.root,
            "https://github.com/luaupm/index",
            &config,
            "chief/core",
            &semver::VersionReq::STAR,
            None,
            &[],
        )
        .unwrap();

        assert_eq!(resolved.version, semver::Version::new(0, 1, 0));
        assert_eq!(resolved.environment, Some(Environment::Roblox));
        assert_eq!(resolved.dependencies.len(), 1);
        assert_eq!(resolved.dependencies[0].name, "chief/bin");
        // No index named on the dependency, resolve it in the same index.
        assert_eq!(resolved.dependencies[0].index_url, None);
        let DownloadSource::TarGz { url } = resolved.source else {
            panic!("expected tarball source");
        };
        assert_eq!(url, "https://cdn.luaupm.com/chief/core/0.1.0.tar.gz");
    }

    #[test]
    fn resolves_top_level_entries_from_disk() {
        /* Entry keys carry the target after the version. older files put
        entries at the top level instead of under [entries]. */
        let index = TempIndex::new("top-level", r#"api = "https://registry.example.com""#);
        index.write_package(
            "scope/pkg",
            r#"
            ["0.1.0 luau".target]
            environment = "luau"
            lib = "init.luau"

            ["0.2.0 luau".target]
            environment = "luau"
            lib = "init.luau"
            "#,
        );

        let config = load_config(&index.root).unwrap();
        let resolved = resolve(
            &index.root,
            "https://example.com/index",
            &config,
            "scope/pkg",
            &semver::VersionReq::STAR,
            None,
            &[],
        )
        .unwrap();

        assert_eq!(resolved.version, semver::Version::new(0, 2, 0));
        assert_eq!(resolved.environment, Some(Environment::Luau));
        let DownloadSource::TarGz { url } = resolved.source else {
            panic!("expected tarball source");
        };
        assert_eq!(
            url,
            "https://registry.example.com/v1/packages/scope%2Fpkg/0.2.0/luau/archive"
        );
    }

    #[test]
    fn resolves_nested_entries_and_prefers_environment() {
        let index = TempIndex::new("nested", r#"api = "https://registry.example.com""#);
        index.write_package(
            "scope/multi",
            r#"
            [meta]

            [entries."1.0.0 luau"]
            name = "scope/multi"

            [entries."1.0.0 luau".target]
            environment = "luau"
            lib = "init.luau"

            [entries."1.0.0 lune"]
            name = "scope/multi"

            [entries."1.0.0 lune".target]
            environment = "lune"
            lib = "init.luau"

            [entries."1.0.0 zune"]
            name = "scope/multi"

            [entries."1.0.0 zune".target]
            environment = "zune"
            lib = "init.luau"
            "#,
        );

        let config = load_config(&index.root).unwrap();
        let resolved = resolve(
            &index.root,
            "https://example.com/index",
            &config,
            "scope/multi",
            &semver::VersionReq::STAR,
            Some((Environment::Lune, Language::Luau)),
            &[],
        )
        .unwrap();

        assert_eq!(resolved.version, semver::Version::new(1, 0, 0));
        assert_eq!(resolved.environment, Some(Environment::Lune));
        let DownloadSource::TarGz { url } = resolved.source else {
            panic!("expected tarball source");
        };
        assert_eq!(
            url,
            "https://registry.example.com/v1/packages/scope%2Fmulti/1.0.0/lune/archive"
        );
    }

    #[test]
    fn yanked_versions_resolve_only_when_pinned() {
        let index = TempIndex::new("yanked", r#"github_oauth_id = "Ov23abc""#);
        index.write_package(
            "acme/thing",
            r#"
            ["1.0.0 luau"]
            target = "luau"
            download = "https://cdn.example.com/acme/thing/1.0.0.tar.gz"

            ["1.1.0 luau"]
            target = "luau"
            yanked = true
            download = "https://cdn.example.com/acme/thing/1.1.0.tar.gz"
            "#,
        );
        let config = load_config(&index.root).unwrap();
        let pick = |pinned: &[semver::Version]| {
            resolve(
                &index.root,
                "https://example.com/index",
                &config,
                "acme/thing",
                &semver::VersionReq::STAR,
                None,
                pinned,
            )
            .unwrap()
        };

        // a fresh pick steps over the yank
        let fresh = pick(&[]);
        assert_eq!(fresh.version, semver::Version::new(1, 0, 0));
        assert!(!fresh.yanked);

        // a lockfile that pins the yanked version keeps it, flagged
        let pinned = pick(&[semver::Version::new(1, 1, 0)]);
        assert_eq!(pinned.version, semver::Version::new(1, 1, 0));
        assert!(pinned.yanked);
    }

    #[test]
    fn entries_carry_their_language() {
        let index = TempIndex::new("language", r#"github_oauth_id = "Ov23abc""#);
        index.write_package(
            "acme/signal",
            r#"
            ["0.1.0 roblox"]
            target = "roblox"
            language = "alloy"
            download = "https://cdn.example.com/acme/signal/0.1.0.tar.gz"

            ["0.2.0 roblox"]
            target = "roblox"
            language = "cobol"
            download = "https://cdn.example.com/acme/signal/0.2.0.tar.gz"
            "#,
        );
        let config = load_config(&index.root).unwrap();
        let resolved = resolve(
            &index.root,
            "https://example.com/index",
            &config,
            "acme/signal",
            &semver::VersionReq::STAR,
            None,
            &[],
        )
        .unwrap();

        // a language this embr does not know is skipped like an unknown target
        assert_eq!(resolved.version, semver::Version::new(0, 1, 0));
        assert_eq!(resolved.language, Language::Alloy);
    }

    /** a package file with one entry for each target ID, the shape the
    registry writes: `target` holds the environment, `language` appears
    only when it is not luau, and each download is named after its ID. */
    fn targets_file(version: &str, ids: &[&str]) -> String {
        let mut text = String::new();
        for id in ids {
            let (environment, language) = id.split_once('-').unwrap_or((id, "luau"));
            text.push_str(&format!(
                "[\"{version} {id}\"]\ntarget = \"{environment}\"\n"
            ));
            if language != "luau" {
                text.push_str(&format!("language = \"{language}\"\n"));
            }
            text.push_str(&format!(
                "download = \"https://cdn.example.com/{version}/{id}.tar.gz\"\n\n"
            ));
        }
        text
    }

    /// the target ID that a project in `environment` written in `language` installs.
    fn picked(
        index: &TempIndex,
        name: &str,
        environment: Environment,
        language: Language,
    ) -> String {
        let config = load_config(&index.root).unwrap();
        let resolved = resolve(
            &index.root,
            "https://example.com/index",
            &config,
            name,
            &semver::VersionReq::STAR,
            Some((environment, language)),
            &[],
        )
        .unwrap();
        let DownloadSource::TarGz { url } = resolved.source else {
            panic!("expected tarball source");
        };
        url.rsplit('/')
            .next()
            .unwrap()
            .trim_end_matches(".tar.gz")
            .to_string()
    }

    #[test]
    fn keys_carry_the_language_after_the_environment() {
        let index = TempIndex::new("key-language", r#"github_oauth_id = "Ov23abc""#);
        index.write_package(
            "acme/multi",
            &(targets_file("1.0.0", &["roblox", "roblox-alloy", "roblox_server-loom"])
                // the key alone can say the language
                + "[\"1.0.0 luau-luwu\"]\ntarget = \"luau\"\n\n"
                // targets this embr does not know are skipped
                + "[\"1.0.0 roblox-cobol\"]\ntarget = \"roblox\"\n\n"
                + "[\"1.0.0 mars-alloy\"]\ntarget = \"mars\"\n\n"),
        );

        let (candidates, _) = read(&index.root, "https://example.com/index", "acme/multi").unwrap();
        let read: Vec<(&str, Option<Environment>, Language)> = candidates
            .iter()
            .map(|c| (c.target.as_deref().unwrap(), c.environment, c.language))
            .collect();
        assert_eq!(
            read,
            [
                ("luau-luwu", Some(Environment::Luau), Language::Luwu),
                ("roblox", Some(Environment::Roblox), Language::Luau),
                ("roblox-alloy", Some(Environment::Roblox), Language::Alloy),
                (
                    "roblox_server-loom",
                    Some(Environment::Server),
                    Language::Loom
                ),
            ]
        );
    }

    #[test]
    fn a_version_picks_the_target_that_fits_the_project() {
        use Environment::{Lune, Roblox};
        use Language::{Alloy, Luau, Luwu};
        let index = TempIndex::new("preference", r#"github_oauth_id = "Ov23abc""#);
        let ids = ["luau", "luau-luwu", "roblox-loom", "roblox", "roblox-alloy"];
        index.write_package("acme/every", &targets_file("1.0.0", &ids));
        index.write_package(
            "acme/no-alloy",
            &targets_file("1.0.0", &["roblox-loom", "roblox"]),
        );
        index.write_package(
            "acme/loom-only",
            &targets_file("1.0.0", &["luau", "roblox-loom"]),
        );
        index.write_package(
            "acme/luwu-only",
            &targets_file("1.0.0", &["lune", "roblox-luwu"]),
        );

        // (a) the exact environment and language
        assert_eq!(picked(&index, "acme/every", Roblox, Alloy), "roblox-alloy");
        assert_eq!(
            picked(&index, "acme/every", Environment::Luau, Luwu),
            "luau-luwu"
        );
        assert_eq!(picked(&index, "acme/every", Roblox, Luau), "roblox");
        // (b) the environment in plain Luau, before another language
        assert_eq!(picked(&index, "acme/no-alloy", Roblox, Alloy), "roblox");
        /* (c) the environment in a language the project accepts. an Alloy
        project accepts only Alloy and Luau, so Loom is never a pick, and
        (d) applies */
        assert_eq!(picked(&index, "acme/loom-only", Roblox, Alloy), "luau");
        // (d) nothing in the environment fits, so the first entry, as before
        assert_eq!(picked(&index, "acme/luwu-only", Roblox, Luau), "lune");
        assert_eq!(picked(&index, "acme/loom-only", Lune, Luau), "luau");
    }

    #[test]
    fn old_keys_still_resolve() {
        let index = TempIndex::new("old-keys", r#"github_oauth_id = "Ov23abc""#);
        // what the registry wrote before targets had IDs
        index.write_package(
            "acme/old",
            r#"
            ["1.0.0 shared"]
            target = "shared"
            download = "https://cdn.example.com/1.0.0/shared.tar.gz"

            ["1.0.0 lune"]
            target = "lune"
            download = "https://cdn.example.com/1.0.0/lune.tar.gz"
            "#,
        );
        use Environment::*;
        assert_eq!(
            picked(&index, "acme/old", Roblox, Language::Alloy),
            "shared"
        );
        assert_eq!(picked(&index, "acme/old", Roblox, Language::Luau), "shared");
        assert_eq!(picked(&index, "acme/old", Lune, Language::Luau), "lune");
    }

    #[test]
    fn a_yanked_target_steps_aside_for_the_others() {
        let index = TempIndex::new("yanked-target", r#"github_oauth_id = "Ov23abc""#);
        index.write_package(
            "acme/thing",
            &targets_file("1.0.0", &["roblox", "roblox-alloy"]).replace(
                "language = \"alloy\"\n",
                "language = \"alloy\"\nyanked = true\n",
            ),
        );
        let config = load_config(&index.root).unwrap();
        let pick = |pinned: &[semver::Version]| {
            let resolved = resolve(
                &index.root,
                "https://example.com/index",
                &config,
                "acme/thing",
                &semver::VersionReq::STAR,
                Some((Environment::Roblox, Language::Alloy)),
                pinned,
            )
            .unwrap();
            (resolved.language, resolved.yanked)
        };
        // a fresh Alloy pick takes the Luau target of the same version
        assert_eq!(pick(&[]), (Language::Luau, false));
        // a lockfile that pins the version keeps the yanked target, flagged
        assert_eq!(
            pick(&[semver::Version::new(1, 0, 0)]),
            (Language::Alloy, true)
        );
    }

    #[test]
    fn deprecations_come_from_meta_and_from_entries() {
        let index = TempIndex::new("deprecated", r#"github_oauth_id = "Ov23abc""#);
        index.write_package(
            "acme/old",
            r#"
            [meta]
            deprecated = "use acme/new"

            ["1.0.0 luau"]
            target = "luau"
            deprecated = "1.0.0 leaks connections"
            download = "https://cdn.example.com/acme/old/1.0.0.tar.gz"
            "#,
        );
        // pesde's empty string means "not deprecated"
        index.write_package(
            "acme/fine",
            r#"
            [meta]
            deprecated = ""

            ["1.0.0 luau"]
            target = "luau"
            download = "https://cdn.example.com/acme/fine/1.0.0.tar.gz"
            "#,
        );
        let config = load_config(&index.root).unwrap();
        let pick = |name: &str| {
            resolve(
                &index.root,
                "https://example.com/index",
                &config,
                name,
                &semver::VersionReq::STAR,
                None,
                &[],
            )
            .unwrap()
        };

        let old = pick("acme/old");
        assert_eq!(old.package_deprecated.as_deref(), Some("use acme/new"));
        assert_eq!(old.deprecated.as_deref(), Some("1.0.0 leaks connections"));
        let fine = pick("acme/fine");
        assert_eq!(fine.package_deprecated, None);
        assert_eq!(fine.deprecated, None);
    }
}
