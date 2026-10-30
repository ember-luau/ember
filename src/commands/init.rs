use crate::error::Error;
use crate::net::auth;
use crate::project::manifest::{
    Environment, Language, Manifest, Package, Target, is_github_username,
};
use crate::sys::git;
use clap::Args;
use inquire::{Select, Text, validator::Validation};
use std::io::IsTerminal;
use std::path::Path;

#[derive(Args, Debug, Default)]
pub struct InitArgs {
    /// Project (code you run) or package (code you publish)
    #[arg(long, value_enum)]
    pub kind: Option<Kind>,
    /// The language: luau, alloy, loom, or luwu
    #[arg(long, value_parser = parse_language)]
    pub language: Option<Language>,
    /// Where the code runs: roblox, roblox_server, lune, lute, seal, zune, or luau for portable Luau
    #[arg(long, value_parser = parse_environment)]
    pub environment: Option<Environment>,
    /// The package name, as scope/name
    #[arg(long)]
    pub name: Option<String>,
    /// Take the default for every question that no flag answers
    #[arg(short, long)]
    pub yes: bool,
}

#[derive(clap::ValueEnum, Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Kind {
    /// code you run; it consumes packages and never publishes
    #[default]
    Project,
    /// code you publish to the registry
    Package,
}

fn parse_language(name: &str) -> Result<Language, String> {
    Language::parse(name).ok_or_else(|| format!("'{name}' is not luau, alloy, loom, or luwu"))
}

fn parse_environment(name: &str) -> Result<Environment, String> {
    Environment::from_embr(name).map_err(|error| error.to_string())
}

const LANGUAGES: [Language; 4] = [
    Language::Luau,
    Language::Alloy,
    Language::Loom,
    Language::Luwu,
];

/** the environments a language can target, the first one the default.
Alloy builds to plain Luau, so it can target what Luau can. Loom is a
Roblox DSL. A Luwu host such as Seal runs Luwu, and embr gives Luwu code
the portable Luau environment. */
fn environment_choices(language: Language) -> &'static [Environment] {
    match language {
        Language::Luau | Language::Alloy => &[
            Environment::Roblox,
            Environment::Server,
            Environment::Lune,
            Environment::Lute,
            Environment::Seal,
            Environment::Zune,
            Environment::Luau,
        ],
        Language::Loom => &[Environment::Roblox, Environment::Server],
        Language::Luwu => &[Environment::Luau],
    }
}

/// the name the site uses for an environment.
fn environment_label(environment: Environment) -> &'static str {
    match environment {
        Environment::Roblox => "Roblox",
        Environment::Server => "Roblox Server",
        Environment::Lune => "Lune",
        Environment::Lute => "Lute",
        Environment::Seal => "Seal",
        Environment::Zune => "Zune",
        Environment::Luau => "Portable Luau (any runtime)",
    }
}

/// the entry point a new package of this language starts from.
fn default_main(language: Language) -> &'static str {
    match language {
        Language::Luau => "src/init.luau",
        // `alloy build` and `loom build` write the Luau that publish ships
        Language::Alloy => "out/init.luau",
        Language::Loom => "dist/init.luau",
        Language::Luwu => "src/init.luwu",
    }
}

/** the whole manifest a `project` answer produces. a project consumes
packages and never publishes, so it needs no [package] table -- see
[`Manifest::package`]. its [target] gives it the install rule for its
environment and language. the empty [dependencies] table is there so
`embr add` has somewhere obvious to write. */
fn project_manifest(environment: Environment, language: Language) -> String {
    let language = if language.is_luau() {
        String::new()
    } else {
        format!("language = \"{language}\"\n", language = language.as_str())
    };
    format!(
        "\
# Manifest for this project. See https://emberpm.dev/docs
# Add dependencies with `embr add <scope/name>`, then run `embr install`.

[target]
environment = \"{environment}\"
{language}
[dependencies]
",
        environment = environment.as_str(),
    )
}

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
This utility will walk you through creating an ember.toml file
It only covers the most common items, and tries to guess sensible defaults

See `embr help init` for documentation on these fields and what they do

Press ^C at any time to quit";

struct Defaults {
    name: Option<String>,
    authors: Option<String>,
    repository: Option<String>,
}

impl Defaults {
    /** best-effort prompt defaults scraped from git. `name` prefers owner/repo
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

        /* authors must be GitHub usernames, the registry makes them scope
        co-owners on publish and rejects anything else. so only guess from
        sources that actually hold one, the login a previous `embr publish`
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

/** owner segment of a github.com https url, kept verbatim since usernames
can have dashes, so no sanitizing. other hosts give None, a GitLab owner is
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

pub fn run(args: InitArgs) -> Result<(), Error> {
    let manifest_path = Path::new("ember.toml");
    if manifest_path.exists() {
        return Err(Error::ManifestExists);
    }

    /* a flag answers its question. without one, a terminal gets the
    question, and anything else (CI, a script, --yes) gets the default */
    let interactive = !args.yes && std::io::stdin().is_terminal();
    if interactive {
        println!("{INTRO}");
        inquire::set_global_render_config(crate::ui::render_config());
    }

    /* asked first because the answer decides whether the registry metadata
    below is worth asking. a project is the common case */
    let kind = match args.kind {
        Some(kind) => kind,
        None if interactive => {
            let package = Select::new("initialize:", vec!["project", "package"])
                .with_help_message("A project is code you run; a package is code you publish")
                .prompt()?
                == "package";
            if package {
                Kind::Package
            } else {
                Kind::Project
            }
        }
        None => Kind::Project,
    };

    let language = match args.language {
        Some(language) => language,
        None if interactive => {
            let titles: Vec<&str> = LANGUAGES.iter().map(|language| language.title()).collect();
            let title = Select::new("language:", titles)
                .with_help_message("The language of your own files")
                .prompt()?;
            LANGUAGES
                .into_iter()
                .find(|language| language.title() == title)
                .unwrap_or_default()
        }
        None => Language::Luau,
    };

    let choices = environment_choices(language);
    let environment = match args.environment {
        Some(environment) if choices.contains(&environment) => environment,
        Some(environment) => {
            return Err(Error::InitArgs(format!(
                "{} code does not run in {}; choose one of: {}",
                language.title(),
                environment.as_str(),
                choices
                    .iter()
                    .map(|choice| choice.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            )));
        }
        None if interactive && choices.len() > 1 => {
            let labels: Vec<&str> = choices
                .iter()
                .map(|choice| environment_label(*choice))
                .collect();
            let label = Select::new("runtime:", labels)
                .with_help_message("Where the code runs")
                .prompt()?;
            choices
                .iter()
                .copied()
                .find(|choice| environment_label(*choice) == label)
                .unwrap_or(choices[0])
        }
        None => choices[0],
    };
    /* seal resolves `.luau` only, so a dependency with `.lua` files breaks
    there at require time. better said now than in the first bug report. */
    if environment == Environment::Seal {
        eprintln!("note: seal does not load .lua files, and its @std is not lute's @std");
    }

    if kind == Kind::Project {
        return write_manifest(manifest_path, &project_manifest(environment, language));
    }

    let defaults = Defaults::guess();
    let ask = |prompt: Text<'_>, default: Option<&str>| -> Result<String, Error> {
        if interactive {
            Ok(prompt.prompt()?)
        } else {
            Ok(default.unwrap_or_default().to_string())
        }
    };

    let name = match args.name {
        Some(name) => {
            validate_name(&name).map_err(Error::InitArgs)?;
            name
        }
        None if interactive => {
            let mut name_prompt = Text::new("name:")
                .with_help_message("Scoped as 'scope/name'; letters and numbers only")
                .with_validator(|input: &str| match validate_name(input) {
                    Ok(()) => Ok(Validation::Valid),
                    Err(message) => Ok(Validation::Invalid(message.into())),
                });
            if let Some(default) = &defaults.name {
                name_prompt = name_prompt.with_default(default);
            }
            name_prompt.prompt()?
        }
        None => match &defaults.name {
            Some(name) if validate_name(name).is_ok() => name.clone(),
            _ => {
                return Err(Error::InitArgs(
                    "embr cannot guess a package name here; pass --name scope/name".to_string(),
                ));
            }
        },
    };

    let version = ask(
        Text::new("version:")
            .with_default("0.1.0")
            .with_validator(
                |input: &str| match input.trim().parse::<semver::Version>() {
                    Ok(_) => Ok(Validation::Valid),
                    Err(error) => Ok(Validation::Invalid(
                        format!("Invalid version: {error}").into(),
                    )),
                },
            ),
        Some("0.1.0"),
    )?;

    let description = ask(
        Text::new("description:").with_help_message("Short description; press enter to skip"),
        None,
    )?;

    let mut authors_prompt = Text::new("authors:")
        .with_help_message(
            "GitHub usernames, comma separated; each can publish to your scope after the first publish",
        )
        .with_validator(|input: &str| Ok(validate_authors(input)));
    if let Some(default) = &defaults.authors {
        authors_prompt = authors_prompt.with_default(default);
    }
    let authors_input = ask(authors_prompt, defaults.authors.as_deref())?;

    let repository = ask(
        match &defaults.repository {
            Some(default) => Text::new("repository:").with_default(default),
            None => Text::new("repository:")
                .with_help_message("Link to repository; press enter to skip"),
        },
        defaults.repository.as_deref(),
    )?;

    let license = if interactive {
        Select::new("license:", LICENSES.to_vec())
            .with_page_size(8)
            .prompt()?
    } else {
        LICENSES[0]
    };

    let main = ask(
        Text::new("main:")
            .with_help_message("Entry point of your package")
            .with_default(default_main(language)),
        Some(default_main(language)),
    )?;

    let package = Package {
        name,
        version: version.trim().to_string(),
        private: false,
        description: non_empty(description),
        authors: parse_authors(&authors_input),
        repository: non_empty(repository),
        license: Some(license.to_string()),
        logo: None,
        visibility: None,
    };
    let manifest = package_manifest(package, environment, language, non_empty(main));
    write_manifest(manifest_path, &toml::to_string(&manifest)?)
}

/// the manifest a `package` answer produces: the package and its one target.
fn package_manifest(
    package: Package,
    environment: Environment,
    language: Language,
    main: Option<String>,
) -> Manifest {
    Manifest {
        package: Some(package),
        targets: vec![Target {
            environment,
            main,
            includes: Vec::new(),
            excludes: Vec::new(),
            workspace: Vec::new(),
            language,
            definitions: Vec::new(),
        }],
        config: Default::default(),
        indices: Default::default(),
        dependencies: Default::default(),
        overrides: Default::default(),
        patches: Default::default(),
        tools: Default::default(),
        scripts: Default::default(),
        studio: Default::default(),
    }
}

/// writes the new manifest and git-ignores what installs generate, the tail both answers share.
fn write_manifest(path: &Path, contents: &str) -> Result<(), Error> {
    std::fs::write(path, contents)?;
    crate::ui::print_success("Created ember.toml");

    if let Some(message) = gitignore_embr(Path::new(".gitignore"))? {
        crate::ui::print_success(&message);
    }

    Ok(())
}

/** git-ignores embr's generated folders, the packages output and patch
working copies, creating .gitignore if missing. returns a message saying
what changed, None when everything was already covered. */
fn gitignore_embr(path: &Path) -> Result<Option<String>, Error> {
    const ENTRIES: [&str; 2] = ["packages/", ".ember-patch/"];

    // any common spelling counts, bare, trailing slash, leading slash
    let ignored = |contents: &str, entry: &str| {
        let bare = entry.trim_end_matches('/');
        contents.lines().map(str::trim).any(|line| {
            line == bare
                || line == entry
                || line.strip_prefix('/') == Some(bare)
                || line.strip_prefix('/') == Some(entry)
        })
    };

    if !path.exists() {
        std::fs::write(path, format!("{}\n", ENTRIES.join("\n")))?;
        return Ok(Some(format!(
            "Created .gitignore with {}",
            ENTRIES.join(" and ")
        )));
    }

    let contents = std::fs::read_to_string(path)?;
    let missing: Vec<&str> = ENTRIES
        .into_iter()
        .filter(|entry| !ignored(&contents, entry))
        .collect();
    if missing.is_empty() {
        return Ok(None);
    }

    let mut updated = contents;
    if !updated.is_empty() && !updated.ends_with('\n') {
        updated.push('\n');
    }
    for entry in &missing {
        updated.push_str(entry);
        updated.push('\n');
    }
    std::fs::write(path, updated)?;
    Ok(Some(format!(
        "Added {} to .gitignore",
        missing.join(" and ")
    )))
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

/** at least one author, all shaped like GitHub usernames. the registry
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

    /** the `project` answer writes this verbatim and asks nothing else, so
    nothing else validates it. it has to parse, declare no package, and be
    somewhere `embr add` can write. */
    #[test]
    fn a_project_gets_a_target_and_no_package() {
        for language in LANGUAGES {
            for &environment in environment_choices(language) {
                let text = project_manifest(environment, language);
                let manifest: Manifest = toml::from_str(&text).unwrap();
                assert!(manifest.package.is_none(), "{text}");
                assert!(manifest.dependencies.is_empty());
                let target = manifest.target().expect("a [target]");
                assert_eq!(target.environment, environment);
                assert_eq!(target.language, language);
                assert!(target.main.is_none());
                // plain Luau is the default, so the file leaves it out
                assert_eq!(text.contains("language ="), !language.is_luau(), "{text}");

                // `embr add` edits the raw document, so [dependencies] must be there
                let document: toml_edit::DocumentMut = text.parse().unwrap();
                assert!(
                    document
                        .get("dependencies")
                        .is_some_and(|table| table.is_table())
                );
            }
        }
    }

    #[test]
    fn a_package_gets_its_language_runtime_and_entry_point() {
        let package = || Package {
            name: "acme/rocket".to_string(),
            version: "0.1.0".to_string(),
            private: false,
            description: None,
            authors: Vec::new(),
            repository: None,
            license: Some("MIT".to_string()),
            logo: None,
            visibility: None,
        };
        for language in LANGUAGES {
            let environment = environment_choices(language)[0];
            let main = Some(default_main(language).to_string());
            let text =
                toml::to_string(&package_manifest(package(), environment, language, main)).unwrap();
            let manifest: Manifest = toml::from_str(&text).unwrap();
            assert_eq!(manifest.package.as_ref().unwrap().name, "acme/rocket");
            let target = manifest.target().unwrap();
            assert_eq!(
                (target.environment, target.language),
                (environment, language),
                "{text}"
            );
            assert_eq!(target.main.as_deref(), Some(default_main(language)));
        }
        // Alloy and Loom publish the Luau that their build writes
        assert!(default_main(Language::Alloy).ends_with(".luau"));
        assert!(default_main(Language::Loom).ends_with(".luau"));
    }

    #[test]
    fn each_language_offers_the_runtimes_it_can_target() {
        assert_eq!(
            environment_choices(Language::Luau).len(),
            Environment::ALL.len()
        );
        assert_eq!(
            environment_choices(Language::Alloy),
            environment_choices(Language::Luau)
        );
        assert_eq!(
            environment_choices(Language::Loom),
            [Environment::Roblox, Environment::Server]
        );
        assert_eq!(environment_choices(Language::Luwu), [Environment::Luau]);
        // every choice round-trips through the --environment spelling
        for language in LANGUAGES {
            for &environment in environment_choices(language) {
                assert_eq!(parse_environment(environment.as_str()), Ok(environment));
            }
            assert_eq!(parse_language(language.as_str()), Ok(language));
        }
        assert!(parse_language("lua").is_err());
    }

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
        /* the old default shape "Name <email>" must be rejected,
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
            github_owner("https://github.com/savruun/embr-cli").as_deref(),
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
            owner_repo_from_url("https://github.com/luaupm/embr").as_deref(),
            Some("luaupm/embr")
        );
        assert_eq!(
            owner_repo_from_url("https://github.com/Luau-PM/My_Repo").as_deref(),
            Some("luaupm/myrepo")
        );
        assert_eq!(owner_repo_from_url("https://github.com/onlyowner"), None);
    }

    #[test]
    fn sanitizes_name_parts() {
        assert_eq!(sanitize_name_part("Luau-EMBR_2"), "luauembr2");
        assert_eq!(sanitize_name_part("---"), "");
    }

    #[test]
    fn gitignores_ember_directory() {
        let dir = std::env::temp_dir().join("embr-test-gitignore");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(".gitignore");

        // missing file gets created with both entries
        assert_eq!(
            gitignore_embr(&path).unwrap().as_deref(),
            Some("Created .gitignore with packages/ and .ember-patch/")
        );
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "packages/\n.ember-patch/\n"
        );

        // both there in any common spelling leaves the file alone
        assert_eq!(gitignore_embr(&path).unwrap(), None);
        std::fs::write(&path, "/target\npackages\n/.ember-patch\n").unwrap();
        assert_eq!(gitignore_embr(&path).unwrap(), None);

        /* file missing entries gets exactly the missing ones appended,
        even with no trailing newline */
        std::fs::write(&path, "/target").unwrap();
        assert_eq!(
            gitignore_embr(&path).unwrap().as_deref(),
            Some("Added packages/ and .ember-patch/ to .gitignore")
        );
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "/target\npackages/\n.ember-patch/\n"
        );
        std::fs::write(&path, "packages/\n").unwrap();
        assert_eq!(
            gitignore_embr(&path).unwrap().as_deref(),
            Some("Added .ember-patch/ to .gitignore")
        );

        let _ = std::fs::remove_dir_all(&dir);
    }
}
