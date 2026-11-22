/*! templates and presets on the Ember registry. the two kinds have one API
shape: a list with search, a detail with its versions, a download, and a
publish in two calls. a template is a gzip tarball, a preset is TOML text.
`embr template` and `embr preset` share the search, info and publish
commands here. */

use crate::error::Error;
use crate::net::http::{self, error::HttpError};
use crate::net::{auth, registry};
use crate::project::manifest::{Tool, split_package_name};
use crate::ui::{self, Stylize};
use clap::Args;
use serde::{Deserialize, Serialize};

/// what a listing holds.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Template,
    Preset,
}

impl Kind {
    pub fn noun(self) -> &'static str {
        match self {
            Kind::Template => "template",
            Kind::Preset => "preset",
        }
    }

    /// the `{kind}` of the API paths, and of the pages of the site.
    fn path(self) -> &'static str {
        match self {
            Kind::Template => "templates",
            Kind::Preset => "presets",
        }
    }

    /// what the registry refusal says it refused.
    fn action(self) -> &'static str {
        match self {
            Kind::Template => "template publish",
            Kind::Preset => "preset publish",
        }
    }

    pub fn error(self, message: String) -> Error {
        match self {
            Kind::Template => Error::Template(message),
            Kind::Preset => Error::Preset(message),
        }
    }

    /// the command that uses the listing `name`.
    fn command(self, name: &str) -> String {
        match self {
            Kind::Template => format!("embr init --template ember:{name}"),
            Kind::Preset => format!("embr preset apply ember:{name}"),
        }
    }
}

/** every tag that Ember takes for a template or a preset: the languages,
then the kinds. the backend and the site hold the same list. */
pub const TAGS: [&str; 10] = [
    "luau", "alloy", "loom", "luwu", "game", "library", "plugin", "cli", "ui", "monorepo",
];

/// the most tags of one listing.
const MAX_TAGS: usize = 8;

/// the longest readme, in characters.
pub const MAX_README_CHARS: usize = 20_000;

/// one listing in a search answer.
#[derive(Serialize, Deserialize, Debug)]
pub struct Summary {
    pub scope: String,
    pub name: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub tags: Vec<String>,
    /// the newest version that is no prerelease, else the newest prerelease.
    pub latest: Option<String>,
    #[serde(default)]
    pub downloads: u64,
    #[serde(default)]
    pub updated_at: i64,
}

/// the answer of `GET /v1/{kind}`.
#[derive(Deserialize)]
struct Page {
    items: Vec<Summary>,
    #[serde(default)]
    total: u64,
}

/// the answer of `GET /v1/{kind}/{scope}/{name}`, the parts that embr reads.
#[derive(Serialize, Deserialize, Debug)]
pub struct Detail {
    #[serde(flatten)]
    pub summary: Summary,
    pub license: Option<String>,
    pub repository: Option<String>,
    #[serde(default)]
    pub created_at: i64,
    /// the first publisher.
    pub owner: Option<Owner>,
    pub readme: Option<String>,
    /// newest first.
    #[serde(default)]
    pub versions: Vec<Version>,
}

#[derive(Serialize, Deserialize, Debug)]
pub struct Owner {
    pub login: String,
}

/// one finished version.
#[derive(Serialize, Deserialize, Debug)]
pub struct Version {
    pub version: String,
    #[serde(default)]
    pub prerelease: bool,
    #[serde(default)]
    pub published_at: i64,
    #[serde(default)]
    pub size: u64,
    pub sha256: String,
    /// the paths of the tree. a template has it, a preset does not.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub files: Option<Vec<String>>,
}

/// `name[@version]` in its two parts.
pub fn split_version(spec: &str) -> (&str, Option<&str>) {
    match spec.split_once('@') {
        Some((name, version)) => (name, Some(version)),
        None => (spec, None),
    }
}

/** the listings that match `words` and hold each of `tags`, most
downloaded first when there are no words. */
fn search(api: &str, kind: Kind, words: &str, tags: &[String]) -> Result<Page, Error> {
    let mut url = format!(
        "{api}/v1/{}?q={}&per_page=50",
        kind.path(),
        crate::commands::mcp::encode(words.trim())
    );
    for tag in tags {
        url.push_str(&format!("&tag={}", crate::commands::mcp::encode(tag)));
    }
    Ok(http::get_json(&url, &[])?)
}

/** the listing `name` ("scope/name"). a 404 is
[`Error::EmberListingMissing`], so a template can ask GitHub next. */
pub fn detail(api: &str, kind: Kind, name: &str) -> Result<Detail, Error> {
    // the name goes into the URL
    split_package_name(name)?;
    match http::get_json(&format!("{api}/v1/{}/{name}", kind.path()), &[]) {
        Ok(detail) => Ok(detail),
        Err(HttpError::NotFound) => Err(Error::EmberListingMissing {
            kind: kind.noun(),
            name: name.to_string(),
        }),
        Err(error) => Err(error.into()),
    }
}

/** downloads `version` of the listing `name`, or its latest version, and
checks the bytes against the SHA-256 that Ember lists. gives the version
too. */
pub fn download(
    api: &str,
    kind: Kind,
    name: &str,
    version: Option<&str>,
) -> Result<(String, Vec<u8>), Error> {
    let noun = kind.noun();
    let detail = detail(api, kind, name)?;
    let wanted = version
        .or(detail.summary.latest.as_deref())
        .ok_or_else(|| kind.error(format!("The {noun} {name} has no version on Ember")))?;
    // the version goes into the URL
    let found = detail
        .versions
        .iter()
        .find(|found| found.version == wanted && semver::Version::parse(wanted).is_ok())
        .ok_or_else(|| kind.error(format!("The {noun} {name} has no version {wanted}")))?;
    let url = format!("{api}/v1/{}/{name}/versions/{wanted}/download", kind.path());
    let bytes = ui::with_spinner(&format!("Downloading {noun} {name}@{wanted}"), || {
        Ok(http::get_bytes(&url, &[])?)
    })?;
    let received = crate::sys::hash::sha256_hex(&bytes);
    if !received.eq_ignore_ascii_case(&found.sha256) {
        return Err(kind.error(format!(
            "The {noun} {name}@{wanted} does not match its sha256\n  expected sha256 {}\n  received sha256 {received}\nembr did not use it",
            ui::printable(&found.sha256)
        )));
    }
    Ok((wanted.to_string(), bytes))
}

/// what a publish says about a version, the body of the first call without its `version`.
#[derive(Serialize, Default, Debug)]
pub struct Meta {
    pub description: Option<String>,
    pub tags: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub license: Option<String>,
    /// a GitHub `owner/repo`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub repository: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub readme: Option<String>,
    /// the sorted paths of a template.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub files: Option<Vec<String>>,
}

/// the rules of the API for the metadata of a version. the reason when one breaks.
fn check(meta: &Meta) -> Result<(), String> {
    let description = meta.description.as_deref().unwrap_or_default();
    if description.trim().is_empty() {
        return Err("no description; pass --description \"one line\"".to_string());
    }
    if description.chars().count() > registry::MAX_DESCRIPTION_CHARS {
        return Err("the description must be 1 to 200 characters".to_string());
    }
    if meta.tags.len() > MAX_TAGS {
        return Err(format!("give at most {MAX_TAGS} tags"));
    }
    for (index, tag) in meta.tags.iter().enumerate() {
        check_tag(tag)?;
        if meta.tags[..index].contains(tag) {
            return Err(format!("tag '{tag}' is there twice"));
        }
    }
    if let Some(license) = &meta.license
        && (license.trim().is_empty() || license.chars().count() > 64)
    {
        return Err("the license must be 1 to 64 characters".to_string());
    }
    Ok(())
}

fn check_tag(tag: &str) -> Result<(), String> {
    match TAGS.contains(&tag) {
        true => Ok(()),
        false => Err(format!(
            "unknown tag '{tag}'; the tags are {}",
            TAGS.join(", ")
        )),
    }
}

/// `repository` as a GitHub `owner/repo`, from that form or from a github.com URL.
pub fn github_repository(repository: &str) -> Option<String> {
    let path = repository
        .trim()
        .trim_start_matches("https://github.com/")
        .trim_end_matches('/')
        .trim_end_matches(".git");
    let part = |part: &str| {
        part.chars()
            .all(|c| c.is_ascii_alphanumeric() || "-_.".contains(c))
    };
    Tool::split_repository(path)
        .is_ok_and(|(owner, repo)| part(owner) && part(repo))
        .then(|| path.to_string())
}

/// the version after `latest`: its next patch, or 1.0.0 for a new listing.
fn next_version(latest: Option<&str>) -> String {
    match latest.and_then(|latest| semver::Version::parse(latest).ok()) {
        Some(latest) => format!("{}.{}.{}", latest.major, latest.minor, latest.patch + 1),
        None => "1.0.0".to_string(),
    }
}

/** the two publish calls as the holder of `token`: create the draft, then
send the content with its SHA-256. the second call finishes the version. */
fn publish(
    api: &str,
    token: &str,
    kind: Kind,
    name: &str,
    version: &str,
    meta: &Meta,
    content: &[u8],
) -> Result<(), Error> {
    #[derive(Serialize)]
    struct Draft<'a> {
        version: &'a str,
        #[serde(flatten)]
        meta: &'a Meta,
    }
    let versions = format!("/v1/{}/{name}/versions", kind.path());
    let body = serde_json::to_value(Draft { version, meta }).map_err(std::io::Error::from)?;
    ui::with_spinner(&format!("Creating {name}@{version}"), || {
        registry::owner_call(
            api,
            token,
            kind.action(),
            "POST",
            &versions,
            None,
            Some(body),
        )
    })?;
    ui::with_spinner("Uploading", || {
        let path = format!("{versions}/{version}/content");
        registry::owner_upload(api, token, kind.action(), &path, content)
    })
}

#[derive(Args, Debug)]
pub struct SearchArgs {
    /// Text to find in names and descriptions; with no text, the most downloaded
    pub words: Vec<String>,
    /// Only results with this tag; give the flag again for each tag
    #[arg(long = "tag", value_name = "TAG")]
    pub tags: Vec<String>,
    /// Print the results as one JSON document on stdout
    #[arg(long)]
    pub json: bool,
}

#[derive(Args, Debug)]
pub struct InfoArgs {
    /// Name on Ember, as scope/name
    pub name: String,
    /// Print the details as one JSON document on stdout
    #[arg(long)]
    pub json: bool,
}

#[derive(Args, Debug)]
pub struct PublishArgs {
    /// The saved name
    pub name: String,
    /// Name on Ember, as scope/name; by default your login and the saved name
    #[arg(
        long = "as",
        value_name = "SCOPE/NAME",
        num_args = 0..=1,
        default_missing_value = ""
    )]
    pub as_name: Option<String>,
    /// Version to publish; by default the next patch version, or 1.0.0 the first time
    #[arg(long)]
    pub version: Option<String>,
    /// One line that says what it is, 1 to 200 characters
    #[arg(long)]
    pub description: Option<String>,
    /// A tag; give the flag again for each tag
    #[arg(long = "tag", value_name = "TAG")]
    pub tags: Vec<String>,
    /// Check everything and upload nothing
    #[arg(long)]
    pub dry_run: bool,
}

pub fn run_search(kind: Kind, args: SearchArgs) -> Result<(), Error> {
    for tag in &args.tags {
        check_tag(tag).map_err(|why| kind.error(why))?;
    }
    let words = args.words.join(" ");
    let page = search(&registry::api_url(), kind, &words, &args.tags)?;
    if args.json {
        return ui::print_json(&serde_json::json!({
            "version": ui::JSON_VERSION,
            "total": page.total,
            kind.path(): page.items,
        }));
    }
    if page.items.is_empty() {
        println!("No {} matches", kind.noun());
        return Ok(());
    }
    println!(
        "{}",
        format!("{} of {} {}", page.items.len(), page.total, kind.path()).bold()
    );
    for item in &page.items {
        let name = format!("{}/{}", item.scope, item.name);
        println!(
            "  {} {}  {}",
            ui::printable(&name).with(ui::accent()),
            ui::printable(item.latest.as_deref().unwrap_or_default()),
            ui::printable(&format!(
                "{} download{}  {}",
                item.downloads,
                ui::plural(item.downloads as usize),
                item.tags.join(", ")
            ))
            .dark_grey()
        );
        println!("    {}", ui::strip_controls(&item.description));
    }
    Ok(())
}

pub fn run_info(kind: Kind, args: InfoArgs) -> Result<(), Error> {
    let name = args
        .name
        .strip_prefix(crate::project::manifest::EMBER_PREFIX)
        .unwrap_or(&args.name);
    let detail = detail(&registry::api_url(), kind, name)?;
    if args.json {
        return ui::print_json(&serde_json::json!({
            "version": ui::JSON_VERSION,
            kind.noun(): detail,
        }));
    }
    let latest = detail.summary.latest.as_deref().unwrap_or_default();
    println!("{}", format!("{name} {}", ui::printable(latest)).bold());
    println!("{}", ui::strip_controls(&detail.summary.description));
    let fact = |label: &str, value: Option<String>| {
        if let Some(value) = value.filter(|value| !value.is_empty()) {
            println!(
                "{} {}",
                format!("{label}:").dark_grey(),
                ui::printable(&value)
            );
        }
    };
    fact("Tags", Some(detail.summary.tags.join(", ")));
    fact("License", detail.license.clone());
    fact("Repository", detail.repository.clone());
    fact(
        "Owner",
        detail.owner.as_ref().map(|owner| owner.login.clone()),
    );
    fact("Downloads", Some(detail.summary.downloads.to_string()));
    let versions: Vec<&str> = detail
        .versions
        .iter()
        .map(|version| version.version.as_str())
        .collect();
    fact("Versions", Some(versions.join(", ")));
    let files = detail
        .versions
        .iter()
        .find(|version| version.version == latest)
        .and_then(|version| version.files.as_ref());
    if let Some(files) = files {
        println!("{}", format!("Files ({}):", files.len()).dark_grey());
        for file in files {
            println!("  {}", ui::printable(file));
        }
    }
    println!("Use it with `{}`", kind.command(name));
    Ok(())
}

/** publishes `content` as a version of a listing, with the `embr publish`
login. `meta` holds what the saved template or preset says about itself.
a flag wins over it, and the listing on Ember fills what is still open. */
pub fn run_publish(
    kind: Kind,
    args: &PublishArgs,
    meta: Meta,
    content: &[u8],
) -> Result<(), Error> {
    let api = registry::api_url();
    let meta = std::cell::RefCell::new(meta);
    let (name, version) = crate::commands::yank::as_owner(|token| {
        publish_as(&api, token, kind, args, &mut meta.borrow_mut(), content)
    })?;
    if args.dry_run {
        ui::print_success("Dry run: every check passed, nothing uploaded");
        return Ok(());
    }
    ui::print_success(&format!("Published {} {name}@{version}", kind.noun()));
    println!("https://emberpm.dev/{}/{name}", kind.path());
    println!("Use it with `{}`", kind.command(&name));
    Ok(())
}

/// [`run_publish`] against `api` as the holder of `token`. gives the name and the version.
fn publish_as(
    api: &str,
    token: &str,
    kind: Kind,
    args: &PublishArgs,
    meta: &mut Meta,
    content: &[u8],
) -> Result<(String, String), Error> {
    let noun = kind.noun();
    let name = match args.as_name.as_deref().filter(|name| !name.is_empty()) {
        Some(name) => name.to_string(),
        None => {
            // the stored login when the token is the stored one, else the API says whose it is
            let login = match auth::load()? {
                Some(stored) if stored.token == token => stored.login,
                _ => auth::check(api, token)?.login,
            };
            format!("{login}/{}", args.name).to_lowercase()
        }
    };
    let (scope, short) = split_package_name(&name).map_err(|_| {
        kind.error(format!(
            "'{name}' is not a name on Ember; pass --as scope/name, each part lowercase letters, digits, - or _"
        ))
    })?;
    if scope.len() > 64 || short.len() > 64 {
        return Err(kind.error(format!("'{name}' is longer than 64 characters a part")));
    }

    let listed = match detail(api, kind, &name) {
        Ok(listed) => Some(listed),
        Err(Error::EmberListingMissing { .. }) => None,
        Err(error) => return Err(error),
    };
    let version = match &args.version {
        Some(version) => version.clone(),
        None => next_version(
            listed
                .as_ref()
                .and_then(|listed| listed.summary.latest.as_deref()),
        ),
    };
    if semver::Version::parse(&version).is_err() {
        return Err(kind.error(format!(
            "version '{version}' is not semver, such as 1.2.0 with no leading v"
        )));
    }

    if args.description.is_some() {
        meta.description = args.description.clone();
    }
    if !args.tags.is_empty() {
        meta.tags = args.tags.clone();
    }
    if let Some(listed) = listed {
        if meta.description.is_none() {
            meta.description = Some(listed.summary.description);
        }
        if args.tags.is_empty() {
            meta.tags = listed.summary.tags;
        }
        meta.license = meta.license.take().or(listed.license);
        meta.repository = meta.repository.take().or(listed.repository);
    }
    check(meta).map_err(|why| kind.error(format!("Cannot publish the {noun}: {why}")))?;

    println!(
        "Publishing {noun} {name}@{version} ({:.1} KiB)",
        content.len() as f64 / 1024.0
    );
    if !args.dry_run {
        publish(api, token, kind, &name, &version, meta, content)?;
    }
    Ok((name, version))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::net::http::mock;

    const GAME: &str = r#"{
        "scope": "ada", "name": "roblox-game", "description": "A Roblox game",
        "tags": ["game", "luau"], "latest": "1.2.0", "downloads": 7,
        "updated_at": 1730000000, "license": "MIT", "repository": "ada/roblox-game",
        "created_at": 1730000000, "owner": {"login": "ada", "avatar": "https://x/a.png"},
        "readme": "A game", "can_manage": false,
        "versions": [
            {"version": "1.3.0-rc.1", "prerelease": true, "published_at": 1730000300,
             "size": 3, "sha256": "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad",
             "url": "https://api.emberpm.dev/v1/templates/ada/roblox-game/versions/1.3.0-rc.1/download",
             "files": ["ember.toml"]},
            {"version": "1.2.0", "prerelease": false, "published_at": 1730000200,
             "size": 3, "sha256": "BA7816BF8F01CFEA414140DE5DAE2223B00361A396177A9CB410FF61F20015AD",
             "url": "https://api.emberpm.dev/v1/templates/ada/roblox-game/versions/1.2.0/download",
             "files": ["ember.toml", "src/init.luau"]}
        ]
    }"#;

    /// a registry with the one template ada/roblox-game, whose content is `abc` behind a redirect.
    fn registry() -> (String, mock::Log) {
        mock::serve(|request| {
            let path = request.split(' ').nth(1).unwrap_or_default();
            if path == "/v1/templates/ada/roblox-game" {
                mock::reply("200 OK", GAME)
            } else if path.ends_with("/download") {
                let host = request
                    .lines()
                    .find_map(|line| line.strip_prefix("host: "))
                    .unwrap_or_default();
                format!(
                    "HTTP/1.1 302 Found\r\nLocation: http://{host}/cdn/object\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                )
            } else if path == "/cdn/object" {
                mock::reply("200 OK", "abc")
            } else {
                mock::reply("404 Not Found", r#"{"error":"not found"}"#)
            }
        })
    }

    #[test]
    fn a_download_follows_the_redirect_and_checks_the_sha256() {
        let (api, log) = registry();
        let (version, bytes) = download(&api, Kind::Template, "ada/roblox-game", None).unwrap();
        assert_eq!((version.as_str(), bytes.as_slice()), ("1.2.0", &b"abc"[..]));
        let log = log.lock().unwrap();
        assert!(log[0].starts_with("get /v1/templates/ada/roblox-game "));
        assert!(
            log[1].starts_with("get /v1/templates/ada/roblox-game/versions/1.2.0/download "),
            "{}",
            log[1]
        );
        assert!(log[2].starts_with("get /cdn/object "));
        drop(log);

        // a named version, and one that Ember does not list
        let named = download(&api, Kind::Template, "ada/roblox-game", Some("1.3.0-rc.1"));
        assert_eq!(named.unwrap().0, "1.3.0-rc.1");
        assert!(matches!(
            download(&api, Kind::Template, "ada/roblox-game", Some("9.9.9")),
            Err(Error::Template(message)) if message.contains("no version 9.9.9")
        ));
        // a 404 is its own error, and a name that is no Ember name never reaches the API
        assert!(matches!(
            download(&api, Kind::Template, "ada/none", None),
            Err(Error::EmberListingMissing {
                kind: "template",
                ..
            })
        ));
        assert!(download(&api, Kind::Template, "Ada/Game", None).is_err());
    }

    #[test]
    fn content_that_does_not_match_its_sha256_is_refused() {
        let wrong = GAME.replace("BA7816BF", "00000000");
        let (api, _) = mock::serve(move |request| match request.contains("/download") {
            true => mock::reply("200 OK", "abc"),
            false => mock::reply("200 OK", &wrong),
        });
        assert!(matches!(
            download(&api, Kind::Template, "ada/roblox-game", None),
            Err(Error::Template(message)) if message.contains("does not match its sha256")
        ));
    }

    #[test]
    fn a_search_sends_the_words_and_each_tag() {
        let (api, log) = mock::serve(|_| {
            mock::reply(
                "200 OK",
                r#"{"items":[{"scope":"ada","name":"strict","description":"Strict mode","tags":["luau"],"latest":"1.0.0","downloads":2,"updated_at":1}],"page":1,"per_page":50,"total":1}"#,
            )
        });
        let tags = ["luau".to_string(), "game".to_string()];
        let page = search(&api, Kind::Preset, " strict mode ", &tags).unwrap();
        assert_eq!((page.total, page.items[0].name.as_str()), (1, "strict"));
        assert!(
            log.lock().unwrap()[0]
                .starts_with("get /v1/presets?q=strict%20mode&per_page=50&tag=luau&tag=game "),
        );
    }

    fn args(as_name: Option<&str>, version: Option<&str>) -> PublishArgs {
        PublishArgs {
            name: "Starter".to_string(),
            as_name: as_name.map(str::to_string),
            version: version.map(str::to_string),
            description: None,
            tags: Vec::new(),
            dry_run: false,
        }
    }

    #[test]
    fn a_publish_makes_the_two_calls_with_the_token_and_the_sha256() {
        let (api, log) = mock::serve(|request| {
            if request.starts_with("get /api/me ") {
                mock::reply("200 OK", r#"{"login":"Ada"}"#)
            } else if request.starts_with("get ") {
                mock::reply("404 Not Found", r#"{"error":"not found"}"#)
            } else if request.starts_with("post ") {
                mock::reply("201 Created", r#"{"version":"1.0.0"}"#)
            } else {
                mock::reply("201 Created", r#"{"scope":"ada","name":"starter"}"#)
            }
        });
        let mut meta = Meta {
            description: Some("A start".to_string()),
            tags: vec!["game".to_string()],
            files: Some(vec!["ember.toml".to_string()]),
            ..Meta::default()
        };
        // no --as: the login and the saved name, in lowercase. no listing yet: 1.0.0
        let published = publish_as(
            &api,
            "ember_abc",
            Kind::Template,
            &args(None, None),
            &mut meta,
            b"abc",
        )
        .unwrap();
        assert_eq!(published, ("ada/starter".to_string(), "1.0.0".to_string()));

        let log = log.lock().unwrap();
        assert_eq!(log.len(), 4, "{log:?}");
        assert!(log[1].starts_with("get /v1/templates/ada/starter "));
        assert!(
            log[2].starts_with("post /v1/templates/ada/starter/versions "),
            "{}",
            log[2]
        );
        let body = log[2].split("\r\n\r\n").nth(1).unwrap();
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(body).unwrap(),
            serde_json::json!({
                "version": "1.0.0", "description": "a start", "tags": ["game"],
                "files": ["ember.toml"],
            })
        );
        assert!(
            log[3].starts_with("put /v1/templates/ada/starter/versions/1.0.0/content "),
            "{}",
            log[3]
        );
        assert!(
            log[3].contains(
                "x-ember-sha256: ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
            ),
            "{}",
            log[3]
        );
        assert!(log[3].ends_with("\r\n\r\nabc"));
        assert!(
            log[2..]
                .iter()
                .all(|sent| sent.contains("authorization: bearer ember_abc"))
        );
    }

    #[test]
    fn a_new_version_is_the_next_patch_and_keeps_what_the_listing_says() {
        let (api, log) = registry();
        let mut meta = Meta::default();
        let mut dry = args(Some("ada/roblox-game"), None);
        dry.dry_run = true;
        let published =
            publish_as(&api, "ember_abc", Kind::Template, &dry, &mut meta, b"abc").unwrap();
        assert_eq!(published.1, "1.2.1");
        assert_eq!(meta.description.as_deref(), Some("A Roblox game"));
        assert_eq!(meta.tags, ["game", "luau"]);
        assert_eq!(meta.license.as_deref(), Some("MIT"));
        // a dry run asks for the listing and sends nothing
        assert_eq!(log.lock().unwrap().len(), 1);

        assert_eq!(next_version(None), "1.0.0");
        assert_eq!(next_version(Some("0.4.9")), "0.4.10");
        assert_eq!(next_version(Some("2.0.0-rc.1")), "2.0.1");
    }

    #[test]
    fn a_publish_checks_the_rules_before_any_upload() {
        let (api, log) = mock::serve(|_| mock::reply("404 Not Found", "{}"));
        let refused = |args: &PublishArgs, meta: &mut Meta| match publish_as(
            &api,
            "ember_abc",
            Kind::Preset,
            args,
            meta,
            b"x",
        ) {
            Err(Error::Preset(message)) => message,
            other => panic!("a broken rule: {other:?}"),
        };
        let described = || Meta {
            description: Some("Strict".to_string()),
            ..Meta::default()
        };
        let named = args(Some("ada/strict"), None);
        assert!(refused(&named, &mut Meta::default()).contains("--description"));
        assert!(refused(&args(Some("Ada/strict"), None), &mut described()).contains("--as"));
        assert!(
            refused(&args(Some("ada/strict"), Some("v1")), &mut described()).contains("semver")
        );
        let mut meta = described();
        meta.tags = vec!["rust".to_string()];
        assert!(refused(&named, &mut meta).contains("unknown tag"));
        meta.tags = vec!["luau".to_string(), "luau".to_string()];
        assert!(refused(&named, &mut meta).contains("twice"));
        meta.tags = TAGS[..9].iter().map(|tag| tag.to_string()).collect();
        assert!(refused(&named, &mut meta).contains("at most 8"));
        let mut meta = described();
        meta.description = Some("x".repeat(201));
        assert!(refused(&named, &mut meta).contains("200"));
        assert!(
            log.lock()
                .unwrap()
                .iter()
                .all(|sent| sent.starts_with("get ")),
        );

        // a refusal of the registry stops the publish with its message
        let (api, log) = mock::serve(|request| match request.starts_with("get ") {
            true => mock::reply("404 Not Found", "{}"),
            false => mock::reply("409 Conflict", r#"{"error":"version 1.0.0 is finished"}"#),
        });
        let refused = publish_as(
            &api,
            "ember_abc",
            Kind::Preset,
            &named,
            &mut described(),
            b"x",
        );
        let Err(error @ Error::OwnerActionFailed { status: 409, .. }) = refused else {
            panic!("a 409 is a refusal: {:?}", refused.err());
        };
        assert!(error.to_string().contains("is finished"), "{error}");
        assert_eq!(log.lock().unwrap().len(), 2);
    }

    /// the site and the backend name the same tags, and a change here must change them too.
    #[test]
    fn the_tag_list_matches_the_contract() {
        assert_eq!(
            TAGS,
            [
                "luau", "alloy", "loom", "luwu", "game", "library", "plugin", "cli", "ui",
                "monorepo"
            ]
        );
    }

    #[test]
    fn a_repository_is_a_github_owner_and_repo() {
        assert_eq!(github_repository("ada/game").as_deref(), Some("ada/game"));
        assert_eq!(
            github_repository("https://github.com/ada/game.git").as_deref(),
            Some("ada/game")
        );
        assert_eq!(github_repository("https://gitlab.com/ada/game"), None);
        assert_eq!(github_repository("game"), None);
    }
}
