/*! tools on the Ember registry, written `ember:scope/name@version` under
[tools]. Ember lists each version with its assets. embr turns a version into
the release shape of the GitHub path, so `archive` picks the asset the same
way. an asset url answers with a redirect: to the CDN for an upload, to
GitHub for a tool that Ember pins from GitHub. `embr tool publish` uploads a
version from tool.toml. */

use crate::error::Error;
use crate::net::http::{
    self,
    error::HttpError,
    responses::{Asset, Release},
};
use crate::net::registry;
use crate::project::manifest::{EMBER_PREFIX, Tool, split_package_name};
use crate::ui;
use serde::Deserialize;
use std::fs;
use std::path::{Path, PathBuf};

/** every tag that Ember takes, in the groups of the API contract. the
backend and the site hold the same list. the first five are the `type`
group, and a tool needs one of them. */
pub const TAGS: [&str; 19] = [
    "cli",
    "roblox-plugin",
    "vscode-extension",
    "zed-extension",
    "ide",
    "compiler",
    "transpiler",
    "linter",
    "formatter",
    "language-server",
    "sync",
    "bundler",
    "testing",
    "docs",
    "package-manager",
    "luau",
    "alloy",
    "loom",
    "luwu",
];

/// the most assets a version may have.
const MAX_ASSETS: usize = 20;

/// the largest asset, the body limit of the API Worker.
const MAX_ASSET_BYTES: u64 = 100 * 1024 * 1024;

/// the longest release notes, in characters.
const MAX_NOTES_CHARS: usize = 20_000;

/// what `GET /v1/tools/{scope}/{name}` gives, the parts that embr reads.
#[derive(Deserialize)]
struct Detail {
    #[serde(default)]
    tags: Vec<String>,
    #[serde(default)]
    versions: Vec<Version>,
}

/// one finished version of a tool.
#[derive(Deserialize)]
struct Version {
    version: String,
    #[serde(default)]
    prerelease: bool,
    #[serde(default)]
    assets: Vec<EmberAsset>,
}

#[derive(Deserialize)]
struct EmberAsset {
    name: String,
    url: String,
    sha256: Option<String>,
}

/** the release of `version` of the Ember tool `name` ("scope/name"), or of
its latest version. a 404 is [`Error::EmberToolMissing`], so `tool add` can
ask GitHub next. */
pub fn release(name: &str, version: Option<&str>) -> Result<Release, Error> {
    release_at(&registry::api_url(), name, version)
}

fn release_at(api: &str, name: &str, version: Option<&str>) -> Result<Release, Error> {
    // the name goes into the URL and, later, into a storage folder
    split_package_name(name)?;
    let tool: Detail = match http::get_json(&format!("{api}/v1/tools/{name}"), &[]) {
        Ok(tool) => tool,
        Err(HttpError::NotFound) => return Err(Error::EmberToolMissing(name.to_string())),
        Err(error) => return Err(error.into()),
    };
    if !tool.tags.iter().any(|tag| tag == "cli") {
        return Err(Error::NotCliTool(name.to_string()));
    }
    let picked = match version {
        Some(version) => tool
            .versions
            .iter()
            .find(|found| found.version == version)
            .ok_or_else(|| {
                Error::NoSuchRelease(format!("{EMBER_PREFIX}{name}"), version.to_string())
            })?,
        None => latest(&tool.versions)
            .ok_or_else(|| Error::NoReleases(format!("{EMBER_PREFIX}{name}")))?,
    };
    Ok(Release {
        tag_name: picked.version.clone(),
        assets: picked
            .assets
            .iter()
            .map(|asset| Asset {
                name: asset.name.clone(),
                browser_download_url: asset.url.clone(),
                sha256: asset.sha256.clone(),
            })
            .collect(),
        draft: false,
    })
}

/** the API's `latest`: the newest version by semver that is not a
prerelease, else the newest prerelease. */
fn latest(versions: &[Version]) -> Option<&Version> {
    let newest = |prerelease: bool| {
        versions
            .iter()
            .filter(|found| found.prerelease == prerelease)
            .filter_map(|found| Some((semver::Version::parse(&found.version).ok()?, found)))
            .max_by(|a, b| a.0.cmp(&b.0))
            .map(|(_, found)| found)
    };
    newest(false).or_else(|| newest(true))
}

/// `[tool]` of tool.toml.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ToolFile {
    tool: Meta,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Meta {
    name: String,
    version: Option<String>,
    description: String,
    tags: Vec<String>,
    repository: Option<String>,
    homepage: Option<String>,
    license: Option<String>,
}

/// one file to upload, by the asset name that Ember stores it under.
pub struct Upload {
    pub name: String,
    pub path: PathBuf,
    pub size: u64,
}

/// a version that passed every check, ready for the three publish calls.
pub struct Plan {
    /// "scope/name"
    pub name: String,
    pub version: String,
    pub uploads: Vec<Upload>,
    /// the body of `POST /v1/tools/{scope}/{name}/versions`
    body: serde_json::Value,
}

/** reads `manifest` (tool.toml), the notes file and the sizes of `files`,
and checks them against the API's rules. `version` overrides the version
in tool.toml. */
pub fn plan(
    manifest: &Path,
    files: &[PathBuf],
    version: Option<String>,
    notes: Option<&Path>,
    prerelease: bool,
) -> Result<Plan, Error> {
    let unreadable = |path: &Path, error: std::io::Error| {
        Error::ToolPublishInvalid(format!("could not read {}: {error}", path.display()))
    };
    let text = fs::read_to_string(manifest).map_err(|error| unreadable(manifest, error))?;
    let file: ToolFile = toml::from_str(&text).map_err(|error| {
        Error::ToolPublishInvalid(format!("{}: {}", manifest.display(), error.message()))
    })?;
    let notes = notes
        .map(|path| fs::read_to_string(path).map_err(|error| unreadable(path, error)))
        .transpose()?;
    let mut uploads = Vec::new();
    for path in files {
        let metadata = fs::metadata(path).map_err(|error| unreadable(path, error))?;
        if !metadata.is_file() {
            return Err(Error::ToolPublishInvalid(format!(
                "{} is not a file",
                path.display()
            )));
        }
        uploads.push(Upload {
            name: path
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_default(),
            path: path.clone(),
            size: metadata.len(),
        });
    }
    let mut meta = file.tool;
    if version.is_some() {
        meta.version = version;
    }
    check(&meta, notes.as_deref(), &uploads)
        .map_err(|why| Error::ToolPublishInvalid(format!("{}: {why}", manifest.display())))?;

    let version = meta.version.unwrap_or_default();
    let body = serde_json::json!({
        "version": version,
        "description": meta.description,
        "tags": meta.tags,
        "repository": meta.repository,
        "homepage": meta.homepage,
        "license": meta.license,
        "notes": notes,
        "prerelease": prerelease,
    });
    Ok(Plan {
        name: meta.name,
        version,
        uploads,
        body,
    })
}

/// the API's rules for a tool version, before any upload. the reason when one breaks.
fn check(meta: &Meta, notes: Option<&str>, uploads: &[Upload]) -> Result<(), String> {
    let name_ok = split_package_name(&meta.name)
        .is_ok_and(|(scope, name)| scope.len() <= 64 && name.len() <= 64);
    if !name_ok {
        return Err(format!(
            "name '{}' must be scope/name, each part 1 to 64 lowercase letters, digits, - or _",
            meta.name
        ));
    }
    let Some(version) = &meta.version else {
        return Err("no version; set `version` or pass --version".to_string());
    };
    if semver::Version::parse(version).is_err() {
        return Err(format!(
            "version '{version}' is not semver, such as 1.2.0 with no leading v"
        ));
    }
    if !(1..=200).contains(&meta.description.chars().count()) || meta.description.trim().is_empty()
    {
        return Err("description must be 1 to 200 characters".to_string());
    }
    if !(1..=8).contains(&meta.tags.len()) {
        return Err("give 1 to 8 tags".to_string());
    }
    for (index, tag) in meta.tags.iter().enumerate() {
        if !TAGS.contains(&tag.as_str()) {
            return Err(format!(
                "unknown tag '{tag}'; the tags are {}",
                TAGS.join(", ")
            ));
        }
        if meta.tags[..index].contains(tag) {
            return Err(format!("tag '{tag}' is there twice"));
        }
    }
    if !meta
        .tags
        .iter()
        .any(|tag| TAGS[..5].contains(&tag.as_str()))
    {
        return Err(format!(
            "give at least one type tag: {}",
            TAGS[..5].join(", ")
        ));
    }
    if let Some(repository) = &meta.repository {
        let part = |part: &str| {
            part.chars()
                .all(|c| c.is_ascii_alphanumeric() || "-_.".contains(c))
        };
        let valid =
            Tool::split_repository(repository).is_ok_and(|(owner, repo)| part(owner) && part(repo));
        if !valid {
            return Err(format!(
                "repository '{repository}' must be a GitHub owner/repo"
            ));
        }
    }
    if let Some(homepage) = &meta.homepage {
        let host = homepage.strip_prefix("https://").unwrap_or_default();
        if host.is_empty() || homepage.chars().any(char::is_whitespace) {
            return Err(format!("homepage '{homepage}' must be an https URL"));
        }
    }
    if let Some(license) = &meta.license
        && (license.trim().is_empty() || license.chars().count() > 64)
    {
        return Err("license must be 1 to 64 characters".to_string());
    }
    if notes.is_some_and(|notes| notes.chars().count() > MAX_NOTES_CHARS) {
        return Err(format!(
            "the notes are longer than {MAX_NOTES_CHARS} characters"
        ));
    }
    if uploads.len() > MAX_ASSETS {
        return Err(format!("at most {MAX_ASSETS} files a version"));
    }
    if uploads.is_empty() && meta.tags.iter().any(|tag| tag == "cli") {
        return Err("a `cli` tool needs at least one file".to_string());
    }
    for (index, upload) in uploads.iter().enumerate() {
        let name = &upload.name;
        let valid = (1..=128).contains(&name.len())
            && !name.starts_with('.')
            && name
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || "._+-".contains(c));
        if !valid {
            return Err(format!(
                "file name '{name}' must be 1 to 128 of A-Z a-z 0-9 . _ + -, not starting with ."
            ));
        }
        if uploads[..index].iter().any(|other| other.name == *name) {
            return Err(format!("two files are named '{name}'"));
        }
        if upload.size > MAX_ASSET_BYTES {
            return Err(format!("{name} is larger than 100 MB"));
        }
    }
    Ok(())
}

/// what the registry refusal says it refused.
const ACTION: &str = "tool publish";

/** the three publish calls as the holder of `token`: create the draft,
upload each file with its SHA-256, finish. the API deletes a draft that
never finishes. */
pub fn publish(api: &str, token: &str, plan: &Plan) -> Result<(), Error> {
    let versions = format!("/v1/tools/{}/versions", plan.name);
    let draft = format!("{versions}/{}", plan.version);
    ui::with_spinner(&format!("Creating {}@{}", plan.name, plan.version), || {
        registry::owner_call(
            api,
            token,
            ACTION,
            "POST",
            &versions,
            None,
            Some(plan.body.clone()),
        )
    })?;
    for upload in &plan.uploads {
        let bytes = fs::read(&upload.path)?;
        ui::with_spinner(
            &format!(
                "Uploading {} ({:.1} MiB)",
                upload.name,
                upload.size as f64 / (1024.0 * 1024.0)
            ),
            || {
                let path = format!("{draft}/assets/{}", upload.name);
                registry::owner_upload(api, token, ACTION, &path, &bytes)
            },
        )?;
        ui::print_success(&format!("Uploaded {}", upload.name));
    }
    ui::with_spinner("Finishing", || {
        registry::owner_call(
            api,
            token,
            ACTION,
            "POST",
            &format!("{draft}/finish"),
            None,
            None,
        )
    })?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::net::http::mock;

    const STYLUA: &str = r#"{
        "scope": "johnnymorganz", "name": "stylua",
        "description": "An opinionated Lua code formatter",
        "tags": ["cli", "formatter", "luau"], "source": "github",
        "repository": "johnnymorganz/stylua", "homepage": null,
        "latest": "2.0.2", "downloads": 0, "claimed": false,
        "updated_at": 1730000000, "license": "MPL-2.0", "created_at": 1730000000,
        "owner": null,
        "versions": [
            {"version": "2.1.0-rc.1", "prerelease": true, "published_at": 1730000300,
             "notes": null, "assets": []},
            {"version": "2.0.2", "prerelease": false, "published_at": 1730000200,
             "notes": "fixes",
             "assets": [
                {"name": "stylua-linux-x86_64.zip", "size": 4, "sha256": null,
                 "url": "https://api.emberpm.dev/v1/tools/johnnymorganz/stylua/versions/2.0.2/assets/stylua-linux-x86_64.zip"},
                {"name": "stylua-windows-x86_64.zip", "size": 4,
                 "sha256": "88d4266fd4e6338d13b845fcf289579d209c897823b9217da3e161936f031589",
                 "url": "https://api.emberpm.dev/v1/tools/johnnymorganz/stylua/versions/2.0.2/assets/stylua-windows-x86_64.zip"}
             ]},
            {"version": "2.0.10", "prerelease": false, "published_at": 1730000100,
             "notes": null, "assets": []}
        ]
    }"#;

    fn version(version: &str, prerelease: bool) -> Version {
        Version {
            version: version.to_string(),
            prerelease,
            assets: Vec::new(),
        }
    }

    #[test]
    fn latest_is_the_newest_stable_by_semver_else_the_newest_prerelease() {
        let picked = |versions: &[Version]| latest(versions).map(|found| found.version.clone());
        let versions = [
            version("2.0.2", false),
            version("2.0.10", false),
            version("3.0.0-rc.1", true),
            version("1.9.0", false),
        ];
        // semver order, so 2.0.10 beats 2.0.2, and a prerelease never wins over a stable
        assert_eq!(picked(&versions).as_deref(), Some("2.0.10"));
        let versions = [version("1.0.0-rc.1", true), version("1.0.0-rc.2", true)];
        assert_eq!(picked(&versions).as_deref(), Some("1.0.0-rc.2"));
        assert_eq!(picked(&[]), None);
    }

    #[test]
    fn a_version_becomes_the_release_shape_of_the_github_path() {
        let (api, log) = mock::serve(|_| mock::reply("200 OK", STYLUA));

        let release = release_at(&api, "johnnymorganz/stylua", Some("2.0.2")).unwrap();
        assert_eq!(release.tag_name, "2.0.2");
        assert!(!release.draft);
        assert_eq!(release.assets.len(), 2);
        assert_eq!(release.assets[0].name, "stylua-linux-x86_64.zip");
        assert!(
            release.assets[0]
                .browser_download_url
                .ends_with("/versions/2.0.2/assets/stylua-linux-x86_64.zip")
        );
        assert_eq!(release.assets[0].sha256, None);
        assert_eq!(
            release.assets[1].sha256.as_deref(),
            Some("88d4266fd4e6338d13b845fcf289579d209c897823b9217da3e161936f031589")
        );
        assert!(log.lock().unwrap()[0].starts_with("get /v1/tools/johnnymorganz/stylua "));

        // no version: the latest
        let release = release_at(&api, "johnnymorganz/stylua", None).unwrap();
        assert_eq!(release.tag_name, "2.0.10");

        assert!(matches!(
            release_at(&api, "johnnymorganz/stylua", Some("9.9.9")),
            Err(Error::NoSuchRelease(..))
        ));
        // a name that is no package name never reaches the API
        assert!(release_at(&api, "JohnnyMorganz/StyLua", None).is_err());
        assert_eq!(log.lock().unwrap().len(), 3);
    }

    #[test]
    fn a_tool_without_the_cli_tag_is_refused() {
        let plugin = r#"{"tags": ["roblox-plugin", "sync"], "versions": []}"#;
        let (api, _) = mock::serve(move |_| mock::reply("200 OK", plugin));
        let refused = release_at(&api, "acme/plugin", None);
        let Err(error @ Error::NotCliTool(_)) = refused else {
            panic!("a plugin is no cli tool: {:?}", refused.err());
        };
        assert_eq!(
            error.to_string(),
            "'acme/plugin' is not a command-line tool; see https://emberpm.dev/tool/acme/plugin"
        );
    }

    #[test]
    fn a_missing_tool_is_its_own_error() {
        let (api, _) = mock::serve(|_| mock::reply("404 Not Found", r#"{"error":"not found"}"#));
        assert!(matches!(
            release_at(&api, "acme/none", None),
            Err(Error::EmberToolMissing(_))
        ));
    }

    /// the site and the backend name the same tags, and a change here must change them too.
    #[test]
    fn the_tag_list_matches_the_contract() {
        assert_eq!(
            TAGS,
            [
                "cli",
                "roblox-plugin",
                "vscode-extension",
                "zed-extension",
                "ide",
                "compiler",
                "transpiler",
                "linter",
                "formatter",
                "language-server",
                "sync",
                "bundler",
                "testing",
                "docs",
                "package-manager",
                "luau",
                "alloy",
                "loom",
                "luwu",
            ]
        );
    }

    fn meta(extra: &str) -> Meta {
        let text = format!(
            "name = \"acme/lint\"\nversion = \"1.2.0\"\ndescription = \"One line\"\n{extra}"
        );
        toml::from_str(&text).unwrap()
    }

    fn upload(name: &str, size: u64) -> Upload {
        Upload {
            name: name.to_string(),
            path: PathBuf::from(name),
            size,
        }
    }

    #[test]
    fn tool_toml_checks_follow_the_api_rules() {
        let ok = meta(
            "tags = [\"cli\", \"linter\", \"luau\"]\nrepository = \"acme/lint\"\nhomepage = \"https://example.com\"\nlicense = \"MIT OR Apache-2.0\"",
        );
        let one = [upload("lint-linux-x86_64.zip", 10)];
        assert_eq!(check(&ok, Some("notes"), &one), Ok(()));

        let refused = |meta: &Meta, notes: Option<&str>, uploads: &[Upload]| {
            check(meta, notes, uploads).expect_err("a broken rule")
        };
        // a cli tool needs a file, a plugin may list none
        assert!(refused(&ok, None, &[]).contains("cli"));
        assert_eq!(
            check(&meta("tags = [\"roblox-plugin\"]"), None, &[]),
            Ok(())
        );

        for (extra, why) in [
            ("tags = []", "1 to 8"),
            ("tags = [\"linter\"]", "type tag"),
            ("tags = [\"cli\", \"rust\"]", "unknown tag"),
            ("tags = [\"cli\", \"cli\"]", "twice"),
            (
                "tags = [\"cli\",\"ide\",\"luau\",\"alloy\",\"loom\",\"luwu\",\"docs\",\"sync\",\"bundler\"]",
                "1 to 8",
            ),
            (
                "tags = [\"cli\"]\nrepository = \"https://github.com/a/b\"",
                "repository",
            ),
            (
                "tags = [\"cli\"]\nhomepage = \"http://example.com\"",
                "https",
            ),
            ("tags = [\"cli\"]\nlicense = \"\"", "license"),
        ] {
            assert!(refused(&meta(extra), None, &one).contains(why), "{extra}");
        }

        let cli = meta("tags = [\"cli\"]");
        let mut long = meta("tags = [\"cli\"]");
        long.description = "x".repeat(201);
        assert!(refused(&long, None, &one).contains("description"));
        let mut bad = meta("tags = [\"cli\"]");
        bad.name = "Acme/lint".to_string();
        assert!(refused(&bad, None, &one).contains("name"));
        bad.name = format!("acme/{}", "a".repeat(65));
        assert!(refused(&bad, None, &one).contains("name"));
        let mut bad = meta("tags = [\"cli\"]");
        bad.version = Some("v1.2.0".to_string());
        assert!(refused(&bad, None, &one).contains("semver"));
        bad.version = None;
        assert!(refused(&bad, None, &one).contains("--version"));

        assert!(refused(&cli, Some(&"x".repeat(20_001)), &one).contains("notes"));
        for name in [".hidden", "a b.zip", "", "tool/x"] {
            assert!(
                refused(&cli, None, &[upload(name, 1)]).contains("file name"),
                "{name:?}"
            );
        }
        assert!(
            refused(&cli, None, &[upload("a.zip", 1), upload("a.zip", 2)]).contains("two files")
        );
        assert!(refused(&cli, None, &[upload("a.zip", MAX_ASSET_BYTES + 1)]).contains("100 MB"));
        let many: Vec<Upload> = (0..21).map(|i| upload(&format!("a{i}.zip"), 1)).collect();
        assert!(refused(&cli, None, &many).contains("at most 20"));
    }

    #[test]
    fn plan_reads_tool_toml_and_lets_flags_win() {
        let dir = std::env::temp_dir().join("embr-test-tool-plan");
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let manifest = dir.join("tool.toml");
        fs::write(
            &manifest,
            "[tool]\nname = \"acme/lint\"\nversion = \"1.0.0\"\ndescription = \"Lints\"\ntags = [\"cli\"]\n",
        )
        .unwrap();
        let asset = dir.join("lint-linux.zip");
        fs::write(&asset, b"zip").unwrap();
        let notes = dir.join("NOTES.md");
        fs::write(&notes, "# 1.1.0").unwrap();

        let plan = plan(
            &manifest,
            std::slice::from_ref(&asset),
            Some("1.1.0".to_string()),
            Some(&notes),
            true,
        )
        .unwrap();
        assert_eq!(
            (plan.name.as_str(), plan.version.as_str()),
            ("acme/lint", "1.1.0")
        );
        assert_eq!(plan.uploads[0].name, "lint-linux.zip");
        assert_eq!(plan.uploads[0].size, 3);
        assert_eq!(plan.body["version"], "1.1.0");
        assert_eq!(plan.body["notes"], "# 1.1.0");
        assert_eq!(plan.body["prerelease"], true);

        // an unknown key is a typo, not something to drop
        fs::write(
            &manifest,
            "[tool]\nname = \"acme/lint\"\nverison = \"1.0.0\"\ndescription = \"Lints\"\ntags = [\"cli\"]\n",
        )
        .unwrap();
        assert!(matches!(
            plan_of(&manifest, &asset),
            Err(Error::ToolPublishInvalid(_))
        ));
        let _ = fs::remove_dir_all(&dir);
    }

    fn plan_of(manifest: &Path, asset: &Path) -> Result<Plan, Error> {
        plan(manifest, &[asset.to_path_buf()], None, None, false)
    }

    #[test]
    fn publish_makes_the_three_calls_with_the_token() {
        let dir = std::env::temp_dir().join("embr-test-tool-publish");
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let asset = dir.join("lint-linux.zip");
        fs::write(&asset, b"abc").unwrap();

        let (api, log) = mock::serve(|request| {
            if request.starts_with("post /v1/tools/acme/lint/versions ") {
                mock::reply("201 Created", r#"{"version":"1.0.0"}"#)
            } else if request.starts_with("put ") {
                mock::reply("201 Created", "{}")
            } else {
                mock::reply("200 OK", r#"{"scope":"acme","name":"lint"}"#)
            }
        });
        let plan = Plan {
            name: "acme/lint".to_string(),
            version: "1.0.0".to_string(),
            uploads: vec![Upload {
                name: "lint-linux.zip".to_string(),
                path: asset,
                size: 3,
            }],
            body: serde_json::json!({"version": "1.0.0", "tags": ["cli"]}),
        };
        publish(&api, "ember_abc", &plan).unwrap();

        let log = log.lock().unwrap();
        assert_eq!(log.len(), 3);
        assert!(
            log[0].starts_with("post /v1/tools/acme/lint/versions "),
            "{}",
            log[0]
        );
        assert!(log[0].contains(r#""tags":["cli"]"#), "{}", log[0]);
        assert!(
            log[1].starts_with("put /v1/tools/acme/lint/versions/1.0.0/assets/lint-linux.zip "),
            "{}",
            log[1]
        );
        assert!(
            log[1].contains(
                "x-ember-sha256: ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
            ),
            "{}",
            log[1]
        );
        assert!(log[1].ends_with("\r\n\r\nabc"), "{}", log[1]);
        assert!(
            log[2].starts_with("post /v1/tools/acme/lint/versions/1.0.0/finish "),
            "{}",
            log[2]
        );
        assert!(
            log.iter()
                .all(|sent| sent.contains("authorization: bearer ember_abc"))
        );
        drop(log);

        // a refusal stops the publish with the API's own message
        let (api, log) = mock::serve(|_| {
            mock::reply(
                "409 Conflict",
                r#"{"error":"this tool is pinned from GitHub; claim it first"}"#,
            )
        });
        let refused = publish(&api, "ember_abc", &plan);
        let Err(error @ Error::OwnerActionFailed { status: 409, .. }) = refused else {
            panic!("a 409 is a refusal: {:?}", refused.err());
        };
        assert!(error.to_string().contains("claim it first"), "{error}");
        assert_eq!(log.lock().unwrap().len(), 1);
        let _ = fs::remove_dir_all(&dir);
    }
}
