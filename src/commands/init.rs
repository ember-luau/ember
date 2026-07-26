use crate::error::Error;
use crate::net::auth;
use crate::project::manifest::{Environment, Manifest, Package, Target, is_github_username};
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
    /** best-effort prompt defaults scraped from git: `name` prefers owner/repo
    from the origin remote, falls back to `<git user>/<current dir>`. */
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

        /* authors must be GitHub usernames (the registry makes them scope
        co-owners on publish, rejects anything else), so only guess from
        sources that actually hold one: the login a previous `lpm publish`
        stored, then the owner of a github.com origin remote. never git
        user.name or user.email, those are display identities not usernames */
        let authors = auth::load()
            .ok()
            .flatten()
            .map(|credentials| credentials.login)
            .or_else(|| repository.as_deref().and_then(github_owner));

        Defaults {
            name,
            authors,
            repository,
        }
    }
}

/** owner segment of a github.com https url, kept verbatim (usernames can
have dashes, so no sanitizing). other hosts give None; a GitLab owner is
not a GitHub username. */
fn github_owner(url: &str) -> Option<String> {
    let rest = url.strip_prefix("https://github.com/")?;
    let owner = rest.split('/').next()?;
    is_github_username(owner).then(|| owner.to_string())
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
        .with_help_message(
            "GitHub usernames, comma separated; each can publish to your scope after the first publish",
        )
        .with_validator(|input: &str| Ok(validate_authors(input)));
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
            private: false,
            description: non_empty(description),
            authors: parse_authors(&authors_input),
            repository: non_empty(repository),
            license: Some(license.to_string()),
        },
        target: Some(Target {
            environment: Environment::from_lpm(environment)?,
            main: non_empty(main),
            includes: Vec::new(),
            excludes: Vec::new(),
            workspace: Vec::new(),
        }),
        config: Default::default(),
        indices: Default::default(),
        dependencies: Default::default(),
        tools: Default::default(),
        scripts: Default::default(),
        studio: Default::default(),
    };

    std::fs::write(manifest_path, toml::to_string(&manifest)?)?;
    crate::ui::print_success("Created lpm.toml");

    if let Some(message) = gitignore_lpm(Path::new(".gitignore"))? {
        crate::ui::print_success(message);
    }

    Ok(())
}

/** git-ignores the packages folder, creating .gitignore if missing.
returns a message saying what changed, None if already ignored. */
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

/** at least one author, all shaped like GitHub usernames; the registry
rejects the publish otherwise. */
fn validate_authors(input: &str) -> Validation {
    let authors = parse_authors(input);
    if authors.is_empty() {
        return Validation::Invalid("At least one author is required".into());
    }
    match authors.iter().find(|author| !is_github_username(author)) {
        Some(author) => Validation::Invalid(
            format!("'{author}' is not a GitHub username (letters, digits, and inner dashes only)")
                .into(),
        ),
        None => Validation::Valid,
    }
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
    fn authors_must_be_github_usernames() {
        assert!(matches!(
            validate_authors("evaera, sleitnick"),
            Validation::Valid
        ));
        assert!(matches!(validate_authors("Luau-PM"), Validation::Valid));
        /* the old default shape ("Name <email>") must be rejected,
        not just no longer guessed */
        assert!(matches!(
            validate_authors("Jane Doe <jane@example.com>"),
            Validation::Invalid(_)
        ));
        assert!(matches!(validate_authors("jane, "), Validation::Valid));
        assert!(matches!(validate_authors(""), Validation::Invalid(_)));
        assert!(matches!(validate_authors(" , "), Validation::Invalid(_)));
    }

    #[test]
    fn github_owner_comes_only_from_github_remotes() {
        assert_eq!(
            github_owner("https://github.com/Luau-PM/repo").as_deref(),
            Some("Luau-PM")
        );
        assert_eq!(
            github_owner("https://github.com/savruun/lpm-cli").as_deref(),
            Some("savruun")
        );
        assert_eq!(github_owner("https://gitlab.com/owner/repo"), None);
        assert_eq!(github_owner("https://github.com/"), None);
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

        // missing file gets created
        assert_eq!(
            gitignore_lpm(&path).unwrap(),
            Some("Created .gitignore with packages/")
        );
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "packages/\n");

        // entry already there (any common spelling) is left alone
        assert_eq!(gitignore_lpm(&path).unwrap(), None);
        std::fs::write(&path, "/target\npackages\n").unwrap();
        assert_eq!(gitignore_lpm(&path).unwrap(), None);

        /* file without the entry gets it appended, even with
        no trailing newline */
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
