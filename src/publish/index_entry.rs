use crate::error::Error;
use crate::manifest::{Environment, Manifest};
use toml_edit::{DocumentMut, InlineTable, Item, Table, value};

/// Adds this release to a pesde-style package file, replacing the entry when
/// its "<version> <environment>" key is already present. Everything else in
/// the file (other entries, comments, formatting) is preserved.
pub fn updated_package_file(
    existing: Option<&str>,
    manifest: &Manifest,
    environment: Environment,
    download_url: &str,
) -> Result<String, Error> {
    let mut document: DocumentMut = existing.unwrap_or_default().parse()?;

    let mut entry = Table::new();
    entry.insert("download", value(download_url));
    if let Some(description) = &manifest.package.description {
        entry.insert("description", value(description.as_str()));
    }

    let mut target = Table::new();
    target.insert("environment", value(environment.dir_name()));
    entry.insert("target", Item::Table(target));

    if !manifest.dependencies.is_empty() {
        let mut dependencies = Table::new();
        for (alias, dependency) in &manifest.dependencies {
            let mut spec = InlineTable::new();
            spec.insert("name", dependency.name.as_str().into());
            spec.insert("version", dependency.version.as_str().into());
            // Published entries name indices by URL; the default index is
            // implied by the file's own location and stays omitted.
            if dependency.index.is_some() {
                let url = manifest.index_url(dependency.index.as_deref())?;
                spec.insert("index", url.into());
            }
            dependencies.insert(alias, value(spec));
        }
        entry.insert("dependencies", Item::Table(dependencies));
    }

    let key = format!("{} {}", manifest.package.version, environment.dir_name());
    // Replacing a table would drop the trivia stored on it (comments above
    // the header live in the table's decor); carry it over to the new entry.
    if let Some(Item::Table(old)) = document.get(&key) {
        *entry.decor_mut() = old.decor().clone();
    }
    document.insert(&key, Item::Table(entry));
    Ok(document.to_string())
}

/// Contents of a scope's owners.toml, written when the scope is first claimed.
pub fn owners_file(login: &str) -> String {
    format!("# GitHub logins allowed to publish packages in this scope.\nowners = [\"{login}\"]\n")
}

/// Logins from an owners.toml; missing or malformed content grants nobody.
pub fn owners(existing: &str) -> Vec<String> {
    let Ok(parsed) = toml::from_str::<toml::Value>(existing) else {
        return Vec::new();
    };
    let Some(logins) = parsed.get("owners").and_then(toml::Value::as_array) else {
        return Vec::new();
    };
    logins
        .iter()
        .filter_map(toml::Value::as_str)
        .map(str::to_string)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::index::DownloadSource;
    use crate::index::pesde;
    use std::fs;

    fn parse_manifest(toml_text: &str) -> Manifest {
        toml::from_str(toml_text).unwrap()
    }

    #[test]
    fn generated_entry_resolves_with_the_pesde_reader() {
        let base = std::env::temp_dir().join("lpm-test-index-entry-resolve");
        let _ = fs::remove_dir_all(&base);
        fs::create_dir_all(base.join("acme")).unwrap();
        // An lpm index: no api, entries carry direct download URLs.
        fs::write(base.join("config.toml"), "").unwrap();

        let manifest = parse_manifest(
            r#"
            [package]
            name = "acme/rocket"
            version = "1.2.3"
            description = "Model rockets"

            [indices]
            wally = "https://github.com/UpliftGames/wally-index"

            [dependencies]
            Core = { name = "acme/core", version = "^1.0.0" }
            Promise = { name = "evaera/promise", version = "^4.0.0", index = "wally" }
            "#,
        );

        let file = updated_package_file(
            None,
            &manifest,
            Environment::Luau,
            "https://example.com/rocket-1.2.3.tar.gz",
        )
        .unwrap();
        assert!(file.contains(r#"description = "Model rockets""#));
        fs::write(base.join("acme").join("rocket"), &file).unwrap();

        let config = pesde::load_config(&base).unwrap();
        let resolved = pesde::resolve(
            &base,
            "https://example.com/index",
            &config,
            "acme/rocket",
            &semver::VersionReq::STAR,
            None,
        )
        .unwrap();

        assert_eq!(resolved.version, semver::Version::new(1, 2, 3));
        assert_eq!(resolved.environment, Some(Environment::Luau));
        let DownloadSource::TarGz { url } = resolved.source else {
            panic!("expected a tarball source");
        };
        assert_eq!(url, "https://example.com/rocket-1.2.3.tar.gz");

        assert_eq!(resolved.dependencies.len(), 2);
        let core = resolved
            .dependencies
            .iter()
            .find(|dep| dep.name == "acme/core")
            .unwrap();
        assert_eq!(core.version_req, "^1.0.0");
        assert_eq!(core.index_url, None);
        let promise = resolved
            .dependencies
            .iter()
            .find(|dep| dep.name == "evaera/promise")
            .unwrap();
        assert_eq!(promise.version_req, "^4.0.0");
        assert_eq!(
            promise.index_url.as_deref(),
            Some("https://github.com/UpliftGames/wally-index")
        );

        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn preserves_other_entries_and_replaces_same_key() {
        let existing = r#"# scope canary
["1.0.0 luau"]
download = "https://example.com/rocket-1.0.0.tar.gz"

["1.0.0 luau".target]
environment = "luau"
"#;

        let manifest = parse_manifest("[package]\nname = \"acme/rocket\"\nversion = \"1.1.0\"");
        let updated = updated_package_file(
            Some(existing),
            &manifest,
            Environment::Luau,
            "https://example.com/rocket-1.1.0.tar.gz",
        )
        .unwrap();
        assert!(updated.contains("# scope canary"));
        assert!(updated.contains("https://example.com/rocket-1.0.0.tar.gz"));
        assert!(updated.contains(r#"["1.1.0 luau"]"#));
        assert!(updated.contains("https://example.com/rocket-1.1.0.tar.gz"));

        let manifest = parse_manifest("[package]\nname = \"acme/rocket\"\nversion = \"1.0.0\"");
        let replaced = updated_package_file(
            Some(existing),
            &manifest,
            Environment::Luau,
            "https://example.com/rocket-1.0.0-fixed.tar.gz",
        )
        .unwrap();
        assert!(replaced.contains("# scope canary"));
        assert!(!replaced.contains("rocket-1.0.0.tar.gz"));

        let parsed: toml::Value = toml::from_str(&replaced).unwrap();
        let table = parsed.as_table().unwrap();
        assert_eq!(table.len(), 1);
        assert_eq!(
            table["1.0.0 luau"]["download"].as_str(),
            Some("https://example.com/rocket-1.0.0-fixed.tar.gz")
        );
    }

    #[test]
    fn owners_round_trip() {
        let file = owners_file("savruun");
        assert!(file.starts_with('#'));
        assert_eq!(owners(&file), vec!["savruun".to_string()]);

        assert!(owners("").is_empty());
        assert!(owners("owners = \"not-a-list\"").is_empty());
        assert!(owners("not toml [").is_empty());
    }
}
