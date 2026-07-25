use crate::error::Error;
use crate::project::manifest::{Environment, Manifest, Package, Target};
use crate::sys::git;
use inquire::{Select, Text, validator::Validation};
use std::path::Path;

const ENVIRONMENTS: &[&str] = &["shared", "server", "lune", "luau", "lute"];

const LICENSES: &[&str] = &[
    "MIT",
    "Apache-2.0",
    "GPL-3.0-only",
    "GPL-2.0-only",
    "LGPL-3.0-only",
    "AGPL-3.0-only",
    "MPL-2.0",
    "BSD-2-Clause",
    "BSD-3-Clause",
    "BSL-1.0",
    "ISC",
    "Zlib",
    "CC0-1.0",
    "Unlicense",
];

const INTRO: &str = "\
This utility will walk you through creating an lpm.toml file
It only covers the most common items, and tries to guess sensible defaults

See `lpm help init` for documentation on these fields and what they do

Press ^C at any time to quit";

struct Defaults {
    name: Option<String>,
    authors: Option<String>,
    repository: Option<String>,
}

impl Defaults {
    /// Best-effort prompt defaults scraped from git: `name` prefers owner/repo
    /// from the origin remote, falling back to `<git user>/<current dir>`.
    fn guess() -> Self {
        let remote = git::output(&["remote", "get-url", "origin"]);
        let repository = remote.as_deref().and_then(git::remote_https_url);
        let user_name = git::output(&["config", "user.name"]);

        let name = repository
            .as_deref()
            .and_then(owner_repo_from_url)
            .or_else(|| {
                let dir = std::env::current_dir().ok()?;
                let name = sanitize_name_part(dir.file_name()?.to_str()?);
                let scope = sanitize_name_part(user_name.as_deref()?);
                (!scope.is_empty() && !name.is_empty()).then(|| format!("{scope}/{name}"))
            });

        let authors = match (user_name, git::output(&["config", "user.email"])) {
            (Some(name), Some(email)) => Some(format!("{name} <{email}>")),
            (Some(name), None) => Some(name),
            _ => None,
        };

        Defaults {
            name,
            authors,
            repository,
        }
    }
}

fn owner_repo_from_url(url: &str) -> Option<String> {
    let path = url.split("://").nth(1)?;
    let mut segments = path.split('/').filter(|segment| !segment.is_empty());
    let _host = segments.next()?;
    let scope = sanitize_name_part(segments.next()?);
    let name = sanitize_name_part(segments.next()?);

    (!scope.is_empty() && !name.is_empty()).then(|| format!("{scope}/{name}"))
}

fn sanitize_name_part(part: &str) -> String {
    part.chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .collect::<String>()
        .to_lowercase()
}

pub fn run() -> Result<(), Error> {
    let manifest_path = Path::new("lpm.toml");
    if manifest_path.exists() {
        return Err(Error::ManifestExists);
    }

    println!("{INTRO}");

    inquire::set_global_render_config(crate::ui::render_config());

    let defaults = Defaults::guess();

    let mut name_prompt = Text::new("name:")
        .with_help_message("Scoped as 'scope/name'; letters and numbers only")
        .with_validator(|input: &str| match validate_name(input) {
            Ok(()) => Ok(Validation::Valid),
            Err(message) => Ok(Validation::Invalid(message.into())),
        });
    if let Some(default) = &defaults.name {
        name_prompt = name_prompt.with_default(default);
    }
    let name = name_prompt.prompt()?;

    let version = Text::new("version:")
        .with_default("0.1.0")
        .with_validator(
            |input: &str| match input.trim().parse::<semver::Version>() {
                Ok(_) => Ok(Validation::Valid),
                Err(error) => Ok(Validation::Invalid(
                    format!("Invalid version: {error}").into(),
                )),
            },
        )
        .prompt()?;

    let description = Text::new("description:")
        .with_help_message("Short description; press enter to skip")
        .prompt()?;

    let mut authors_prompt = Text::new("authors:")
        .with_help_message("Comma separated list, e.g. 'Jane Doe, John Doe'")
        .with_validator(|input: &str| {
            if parse_authors(input).is_empty() {
                Ok(Validation::Invalid(
                    "At least one author is required".into(),
                ))
            } else {
                Ok(Validation::Valid)
            }
        });
    if let Some(default) = &defaults.authors {
        authors_prompt = authors_prompt.with_default(default);
    }
    let authors_input = authors_prompt.prompt()?;

    let repository = match &defaults.repository {
        Some(default) => Text::new("repository:").with_default(default),
        None => {
            Text::new("repository:").with_help_message("Link to repository; press enter to skip")
        }
    }
    .prompt()?;

    let license = Select::new("license:", LICENSES.to_vec())
        .with_page_size(8)
        .prompt()?;

    let environment = Select::new("environment:", ENVIRONMENTS.to_vec())
        .with_help_message("Where this package's Luau code runs")
        .prompt()?;
    let main = Text::new("main:")
        .with_help_message("Entry point of your package")
        .with_default("src/init.luau")
        .prompt()?;

    let manifest = Manifest {
        package: Package {
            name,
            version: version.trim().to_string(),
            description: non_empty(description),
            authors: parse_authors(&authors_input),
            repository: non_empty(repository),
            license: Some(license.to_string()),
            include: Vec::new(),
            exclude: Vec::new(),
        },
        target: Some(Target {
            environment: Environment::from_lpm(environment)?,
            main: non_empty(main),
        }),
        config: Default::default(),
        indices: Default::default(),
        dependencies: Default::default(),
        tools: Default::default(),
        scripts: Default::default(),
    };

    std::fs::write(manifest_path, toml::to_string(&manifest)?)?;
    crate::ui::print_success("Created lpm.toml");

    if let Some(message) = gitignore_lpm(Path::new(".gitignore"))? {
        crate::ui::print_success(message);
    }

    Ok(())
}

/// Makes sure the packages folder is git-ignored, creating .gitignore when
/// missing. Returns a message describing what changed, or `None` if the
/// folder was already ignored.
fn gitignore_lpm(path: &Path) -> Result<Option<&'static str>, Error> {
    const ENTRY: &str = "packages/";

    if !path.exists() {
        std::fs::write(path, format!("{ENTRY}\n"))?;
        return Ok(Some("Created .gitignore with packages/"));
    }

    let contents = std::fs::read_to_string(path)?;
    let already_ignored = contents
        .lines()
        .map(str::trim)
        .any(|line| matches!(line, "packages" | "packages/" | "/packages" | "/packages/"));
    if already_ignored {
        return Ok(None);
    }

    let mut updated = contents;
    if !updated.is_empty() && !updated.ends_with('\n') {
        updated.push('\n');
    }
    updated.push_str(ENTRY);
    updated.push('\n');
    std::fs::write(path, updated)?;
    Ok(Some("Added packages/ to .gitignore"))
}

fn validate_name(input: &str) -> Result<(), String> {
    let is_valid_part =
        |part: &str| !part.is_empty() && part.chars().all(|c| c.is_ascii_alphanumeric());

    match input.split('/').collect::<Vec<_>>().as_slice() {
        [author, name] if is_valid_part(author) && is_valid_part(name) => Ok(()),
        _ => Err("Name must be scoped as 'scope/name' (letters and numbers only)".to_string()),
    }
}

fn parse_authors(input: &str) -> Vec<String> {
    input
        .split(',')
        .map(str::trim)
        .filter(|author| !author.is_empty())
        .map(str::to_string)
        .collect()
}

fn non_empty(value: String) -> Option<String> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_scoped_alphanumeric_names() {
        assert!(validate_name("scope/name").is_ok());
        assert!(validate_name("Scope123/Name456").is_ok());
    }

    #[test]
    fn rejects_malformed_names() {
        assert!(validate_name("name").is_err());
        assert!(validate_name("scope/").is_err());
        assert!(validate_name("/name").is_err());
        assert!(validate_name("scope/name/extra").is_err());
        assert!(validate_name("scope/na-me").is_err());
        assert!(validate_name("sc_ope/name").is_err());
        assert!(validate_name("scope/na me").is_err());
    }

    #[test]
    fn parses_comma_separated_authors() {
        assert_eq!(
            parse_authors("Jane Doe, John Doe"),
            vec!["Jane Doe", "John Doe"]
        );
        assert_eq!(parse_authors("Solo"), vec!["Solo"]);
        assert_eq!(parse_authors(" , ,"), Vec::<String>::new());
        assert_eq!(parse_authors(""), Vec::<String>::new());
    }

    #[test]
    fn non_empty_trims_and_drops_blank_values() {
        assert_eq!(non_empty("   ".to_string()), None);
        assert_eq!(non_empty(" hi ".to_string()), Some("hi".to_string()));
    }

    #[test]
    fn derives_scoped_name_from_remote_url() {
        assert_eq!(
            owner_repo_from_url("https://github.com/luaupm/cli").as_deref(),
            Some("luaupm/cli")
        );
        assert_eq!(
            owner_repo_from_url("https://github.com/Luau-PM/My_Repo").as_deref(),
            Some("luaupm/myrepo")
        );
        assert_eq!(owner_repo_from_url("https://github.com/onlyowner"), None);
    }

    #[test]
    fn sanitizes_name_parts() {
        assert_eq!(sanitize_name_part("Luau-LPM_2"), "luaulpm2");
        assert_eq!(sanitize_name_part("---"), "");
    }

    #[test]
    fn gitignores_lpm_directory() {
        let dir = std::env::temp_dir().join("lpm-test-gitignore");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(".gitignore");

        // Missing file is created.
        assert_eq!(
            gitignore_lpm(&path).unwrap(),
            Some("Created .gitignore with packages/")
        );
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "packages/\n");

        // Entry already present (in any common spelling) is left alone.
        assert_eq!(gitignore_lpm(&path).unwrap(), None);
        std::fs::write(&path, "/target\npackages\n").unwrap();
        assert_eq!(gitignore_lpm(&path).unwrap(), None);

        // Existing file without the entry gets it appended, even when the
        // file lacks a trailing newline.
        std::fs::write(&path, "/target").unwrap();
        assert_eq!(
            gitignore_lpm(&path).unwrap(),
            Some("Added packages/ to .gitignore")
        );
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "/target\npackages/\n"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }
}
