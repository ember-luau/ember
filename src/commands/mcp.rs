/*!
`embr mcp`, a Model Context Protocol server on stdio. A coding agent starts
it and calls three tools: a search of the ember registry, a search of every
registry embr installs from, and a package lookup. The server only reads.
It writes no files, installs nothing, and publishes nothing.

The protocol is JSON-RPC 2.0, one JSON object per line. Only protocol
messages go to stdout, so any log goes to stderr.
*/

use crate::error::Error;
use crate::net::http::{self, error::HttpError};
use crate::net::registry::API_URL;
use crate::project::manifest::{DEFAULT_INDEX_URL, Environment, Language, split_package_name};
use crate::registry::index::forest;
use crate::registry::index::pesde::{self, Candidate};
use crate::registry::index::{Index, Refresh};
use serde::Deserialize;
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::io::{BufRead, Write};
use std::sync::mpsc;
use std::time::{Duration, Instant};

/// the APIs behind the Wally and pesde search pages.
const WALLY_API: &str = "https://api.wally.run";
const PESDE_API: &str = "https://registry.pesde.dev";

/** the protocol versions this server accepts. its messages use nothing that
changed between them, so it answers each one the same way. */
const PROTOCOL_VERSIONS: [&str; 4] = ["2024-11-05", "2025-03-26", "2025-06-18", "2025-11-25"];
/// the answer to a client that asks for a version the list does not hold.
const DEFAULT_PROTOCOL_VERSION: &str = "2025-06-18";

const PARSE_ERROR: i64 = -32700;
const INVALID_REQUEST: i64 = -32600;
const METHOD_NOT_FOUND: i64 = -32601;
const INVALID_PARAMS: i64 = -32602;

const INSTRUCTIONS: &str = "ember is a package manager for Luau and Roblox. \
It installs packages from the ember registry, and also from Wally, pesde, and Forest. \
search_packages finds packages in the ember registry by name or description, and filters them by environment or language. \
search_all searches the ember, Wally, pesde, and Forest registries at the same time. \
Use it when the ember registry has no good match, or to compare packages from all registries. \
package_info shows the versions, deprecations, and dependencies of one ember package. \
The tools only read. Each answer gives the `embr add` command that installs a package.";

/// the search answer holds this many packages when the call sets no limit.
const DEFAULT_LIMIT: usize = 20;
/** also the largest page of the registry's `GET /v1/search`, so one page
answers every call. */
const MAX_LIMIT: usize = 50;
/// search_all lists this many packages from each registry when the call sets no limit.
const DEFAULT_ALL_LIMIT: usize = 10;

/** search_all asks the registries at the same time and waits this long for
the answers. a registry that does not answer in time is listed as failed,
and the other registries still answer. */
const REGISTRY_TIMEOUT: Duration = Duration::from_secs(10);
/// Forest refuses a shorter query, and Wally finds nothing for one.
const MIN_QUERY_LENGTH: usize = 2;
/** a Forest search row needs one more request when its scope is claimed,
to read the version metadata. search_all makes this many at most. */
// ponytail: a fixed cap, 1 claimed row in 104 for "signal" today; page the checks if originals grow
const FOREST_CHECKS: usize = 10;

pub fn run() -> Result<(), Error> {
    match serve(std::io::stdin().lock(), std::io::stdout().lock()) {
        // the client closed its end, which ends the session like EOF does
        Err(error) if error.kind() == std::io::ErrorKind::BrokenPipe => Ok(()),
        result => Ok(result?),
    }
}

/** reads one message per line from `input`, and writes each answer as one
line to `output`. it returns at the end of the input. lines are bytes, so
invalid UTF-8 gets a parse error and does not stop the server. */
fn serve(input: impl BufRead, mut output: impl Write) -> std::io::Result<()> {
    for line in input.split(b'\n') {
        let line = line?;
        if line.trim_ascii().is_empty() {
            continue;
        }
        if let Some(answer) = answer(&line) {
            serde_json::to_writer(&mut output, &answer)?;
            output.write_all(b"\n")?;
            output.flush()?;
        }
    }
    Ok(())
}

/** the answer to one message. None when the message needs no answer: a
notification, or a response to a request this server never sends. */
fn answer(line: &[u8]) -> Option<Value> {
    let Ok(message) = serde_json::from_slice::<Value>(line) else {
        return Some(error(Value::Null, PARSE_ERROR, "Parse error"));
    };
    let Some(method) = message.get("method").and_then(Value::as_str) else {
        if message.get("result").is_some() || message.get("error").is_some() {
            return None;
        }
        let id = message.get("id").cloned().unwrap_or(Value::Null);
        return Some(error(id, INVALID_REQUEST, "Invalid Request"));
    };
    // a notification has no id, and gets no answer whatever its method
    let id = message.get("id")?.clone();
    let params = message.get("params").unwrap_or(&Value::Null);

    let result = match method {
        "initialize" => initialize(params),
        "ping" => json!({}),
        "tools/list" => json!({ "tools": tools() }),
        "tools/call" => match call(params) {
            Ok(result) => result,
            Err(message) => return Some(error(id, INVALID_PARAMS, &message)),
        },
        _ => {
            let message = format!("Method not found: {method}");
            return Some(error(id, METHOD_NOT_FOUND, &message));
        }
    };
    Some(json!({ "jsonrpc": "2.0", "id": id, "result": result }))
}

fn error(id: Value, code: i64, message: &str) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "error": { "code": code, "message": message } })
}

fn initialize(params: &Value) -> Value {
    let version = params
        .get("protocolVersion")
        .and_then(Value::as_str)
        .filter(|version| PROTOCOL_VERSIONS.contains(version))
        .unwrap_or(DEFAULT_PROTOCOL_VERSION);
    json!({
        "protocolVersion": version,
        "capabilities": { "tools": {} },
        "serverInfo": { "name": "ember", "version": env!("CARGO_PKG_VERSION") },
        "instructions": INSTRUCTIONS,
    })
}

fn tools() -> Value {
    let environments: Vec<&str> = Environment::ALL.iter().map(|env| env.as_str()).collect();
    let languages = [
        Language::Luau,
        Language::Alloy,
        Language::Loom,
        Language::Luwu,
    ]
    .map(Language::as_str);
    json!([
        {
            "name": "search_packages",
            "description": "Searches the ember registry of Luau and Roblox packages. \
                The query matches package names and descriptions, and case does not matter. \
                Each word of the query matches the start of a word, and a package must match every word. \
                An empty query matches every package. \
                The answer lists the most downloaded packages first, one on each line. \
                Each line gives the name, the latest version, the environments, and the download count. \
                It also gives the language when it is not luau, the description, a deprecation reason, and the install command. \
                To also search Wally, pesde, and Forest, use search_all.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "query": {
                        "type": "string",
                        "description": "The text to find in a package name or description, for example \"signal\" or \"chief/\"."
                    },
                    "environment": {
                        "type": "string",
                        "enum": environments,
                        "description": "Return only the packages for this environment. \
                            roblox is Roblox code for the client or the server, and roblox_server is Roblox server code. \
                            luau is portable Luau, which also runs in every other environment."
                    },
                    "language": {
                        "type": "string",
                        "enum": languages,
                        "description": "Return only the packages in this language. \
                            alloy and loom compile to Luau. luwu needs a luwu host, such as seal."
                    },
                    "limit": {
                        "type": "integer",
                        "minimum": 1,
                        "maximum": MAX_LIMIT,
                        "default": DEFAULT_LIMIT,
                        "description": "The maximum number of packages to return."
                    }
                },
                "required": ["query"],
                "additionalProperties": false
            }
        },
        {
            "name": "search_all",
            "description": "Searches the ember, Wally, pesde, and Forest registries at the same time. \
                The query matches package names and descriptions, and case does not matter. \
                Wally, pesde, and Forest need a query of 2 or more characters. \
                Use search_packages to filter ember packages by environment or language. \
                The answer lists the packages of each registry in the order that the registry gives, one on each line. \
                Each line gives the registry, the name, the latest version, the environments, the description, and the install command. \
                For Wally, pesde, and Forest, the install command adds --index with the registry name. \
                The project needs that index in ember.toml first, and `embr index add <name>` adds it once. \
                An ember line also gives the language when it is not luau, the download count, and a deprecation reason. \
                Forest copies every open-source Wally package, and the answer leaves out those copies. \
                A registry that fails or does not answer in 10 seconds is listed at the end with the reason, \
                and the other registries still answer.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "query": {
                        "type": "string",
                        "description": "The text to find in a package name or description, for example \"signal\"."
                    },
                    "registries": {
                        "type": "array",
                        "items": { "type": "string", "enum": REGISTRIES.map(Registry::name) },
                        "uniqueItems": true,
                        "description": "Search only these registries. Without it, the tool searches all four."
                    },
                    "limit": {
                        "type": "integer",
                        "minimum": 1,
                        "maximum": MAX_LIMIT,
                        "default": DEFAULT_ALL_LIMIT,
                        "description": "The maximum number of packages to return from each registry."
                    }
                },
                "required": ["query"],
                "additionalProperties": false
            }
        },
        {
            "name": "package_info",
            "description": "Shows one package from the ember index. \
                The answer lists every version, newest first, with its environment, its language, and yanked or deprecated marks. \
                It also gives the description, the package deprecation, and the install command. \
                It gives the dependencies of the newest version that is not yanked.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "name": {
                        "type": "string",
                        "description": "The package as scope/name, for example \"chief/core\"."
                    }
                },
                "required": ["name"],
                "additionalProperties": false
            }
        }
    ])
}

/** runs a tools/call. Err is a protocol error: no tool name, or a tool that
does not exist. a tool that fails still answers Ok, with isError set, so the
agent reads the reason. */
fn call(params: &Value) -> Result<Value, String> {
    let name = params
        .get("name")
        .and_then(Value::as_str)
        .ok_or("tools/call needs the name of a tool")?;
    let arguments = params
        .get("arguments")
        .cloned()
        .unwrap_or_else(|| json!({}));
    let outcome = match name {
        "search_packages" => search_packages(arguments),
        "search_all" => search_all(arguments),
        "package_info" => package_info(arguments),
        _ => return Err(format!("Unknown tool: {name}")),
    };
    let (text, is_error) = match outcome {
        Ok(text) => (text, false),
        Err(error) => (error.to_string(), true),
    };
    Ok(json!({ "content": [{ "type": "text", "text": text }], "isError": is_error }))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SearchArgs {
    query: String,
    environment: Option<Environment>,
    language: Option<Language>,
    limit: Option<usize>,
}

/** one package in a `GET /v1/search` answer. older rows can lack
`targets`, `language`, and `deprecated`. serde ignores keys this struct
does not name, like `logo`. the search never lists a package whose every
version is yanked. */
#[derive(Deserialize)]
struct SearchEntry {
    name: String,
    version: String,
    description: Option<String>,
    /// the latest version's targets. an older entry has one `target` instead.
    #[serde(default)]
    targets: Vec<String>,
    target: Option<String>,
    #[serde(rename = "type")]
    kind: Option<String>,
    language: Option<String>,
    downloads: Option<u64>,
    deprecated: Option<String>,
}

impl SearchEntry {
    /** the environments by embr's names, so `shared` reads as roblox. a name
    that embr does not know stays as the registry wrote it. */
    fn environments(&self) -> Vec<String> {
        let names: Vec<&str> = if self.targets.is_empty() {
            self.target
                .iter()
                .chain(&self.kind)
                .take(1)
                .map(String::as_str)
                .collect()
        } else {
            self.targets.iter().map(String::as_str).collect()
        };
        names
            .into_iter()
            .map(|name| {
                pesde::parse_environment(name).map_or(name.to_string(), |env| env.to_string())
            })
            .collect()
    }

    fn language(&self) -> &str {
        self.language.as_deref().unwrap_or("luau")
    }

    /// one line of the search answer.
    fn line(&self) -> String {
        let mut parts = vec![format!("{} {}", self.name, self.version)];
        let environments = self.environments();
        if !environments.is_empty() {
            parts.push(environments.join(", "));
        }
        if self.language() != "luau" {
            parts.push(format!("language: {}", self.language()));
        }
        let downloads = self.downloads.unwrap_or(0);
        parts.push(format!(
            "{downloads} download{}",
            if downloads == 1 { "" } else { "s" }
        ));
        if let Some(description) = non_empty(&self.description) {
            parts.push(description.to_string());
        }
        if let Some(reason) = non_empty(&self.deprecated) {
            parts.push(format!("deprecated: {reason}"));
        }
        parts.push(format!("install: embr add {}", self.name));
        format!("- {}", parts.join(" | "))
    }
}

/// the registry writes an empty string for "no value", as pesde does.
fn non_empty(text: &Option<String>) -> Option<&str> {
    text.as_deref()
        .map(str::trim)
        .filter(|text| !text.is_empty())
}

fn search_packages(arguments: Value) -> Result<String, Error> {
    search_packages_at(API_URL, arguments)
}

/// search_packages against the registry API at `api`.
fn search_packages_at(api: &str, arguments: Value) -> Result<String, Error> {
    let args: SearchArgs = serde_json::from_value(arguments)?;
    let page = ember_search(api, &args.query, args.environment, args.language)?;
    Ok(search(&page, &args))
}

/// one page of the registry's `GET /v1/search`.
#[derive(Deserialize)]
struct SearchPage {
    /// every match, not only this page
    total: usize,
    results: Vec<SearchEntry>,
}

/** the first [`MAX_LIMIT`] matches of `query` in the registry at `api`,
most downloaded first. the registry matches each word of the query as a
prefix, and it filters and ranks the packages. the page size does not
change with the limit, so search_packages and search_all send the same URL
for a query, and the API's 60s cache answers the second. */
fn ember_search(
    api: &str,
    query: &str,
    environment: Option<Environment>,
    language: Option<Language>,
) -> Result<SearchPage, HttpError> {
    let mut url = format!(
        "{api}/v1/search?q={}&sort=downloads&page=1&per_page={MAX_LIMIT}",
        encode(&query.trim().to_lowercase())
    );
    if let Some(environment) = environment {
        url.push_str(&format!("&environment={}", environment.as_str()));
    }
    if let Some(language) = language {
        url.push_str(&format!("&language={}", language.as_str()));
    }
    http::get_json(&url, &[])
}

/// the search_packages answer for one page of matches, as text.
fn search(page: &SearchPage, args: &SearchArgs) -> String {
    if page.results.is_empty() {
        return format!("No package matches \"{}\".", args.query);
    }

    let limit = args.limit.unwrap_or(DEFAULT_LIMIT).clamp(1, MAX_LIMIT);
    let mut text = format!(
        "{} of {} matching packages, most downloaded first:\n",
        page.results.len().min(limit),
        page.total
    );
    for entry in page.results.iter().take(limit) {
        text.push_str(&entry.line());
        text.push('\n');
    }
    text
}

/// a registry that search_all reads. embr installs from each one.
#[derive(Deserialize, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "lowercase")]
enum Registry {
    Ember,
    Wally,
    Pesde,
    Forest,
}

const REGISTRIES: [Registry; 4] = [
    Registry::Ember,
    Registry::Wally,
    Registry::Pesde,
    Registry::Forest,
];

impl Registry {
    /// also the known index name that `embr add --index` takes.
    fn name(self) -> &'static str {
        match self {
            Registry::Ember => "ember",
            Registry::Wally => "wally",
            Registry::Pesde => "pesde",
            Registry::Forest => "forest",
        }
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SearchAllArgs {
    query: String,
    registries: Option<Vec<Registry>>,
    limit: Option<usize>,
}

/** asks one registry for a query. it gives the search_all lines of the
matches, and the number of matches, which can be more than the lines. */
type Fetch = fn(Registry, &str) -> Result<(Vec<String>, usize), String>;

fn search_all(arguments: Value) -> Result<String, Error> {
    let args: SearchAllArgs = serde_json::from_value(arguments)?;
    Ok(search_registries(&args, fetch, REGISTRY_TIMEOUT))
}

/** the search_all answer. `fetch` runs on its own thread for each registry,
and a registry that has not answered after `timeout` is listed as failed. */
fn search_registries(args: &SearchAllArgs, fetch: Fetch, timeout: Duration) -> String {
    let query = args.query.trim().to_lowercase();
    let registries: Vec<Registry> = REGISTRIES
        .into_iter()
        .filter(|registry| {
            args.registries
                .as_ref()
                .is_none_or(|chosen| chosen.is_empty() || chosen.contains(registry))
        })
        .collect();

    let mut results = BTreeMap::new();
    let (sender, receiver) = mpsc::channel();
    for &registry in &registries {
        if registry != Registry::Ember && query.chars().count() < MIN_QUERY_LENGTH {
            let reason = format!("the query needs {MIN_QUERY_LENGTH} or more characters");
            results.insert(registry, Err(reason));
            continue;
        }
        let (sender, query) = (sender.clone(), query.clone());
        /* not a scoped thread, because the answer must not wait for a slow
        registry. a late thread stops at the HTTP agent's timeout, and its
        send then fails, which does no harm */
        std::thread::spawn(move || {
            let _ = sender.send((registry, fetch(registry, &query)));
        });
    }
    drop(sender);
    let deadline = Instant::now() + timeout;
    while let Ok((registry, result)) =
        receiver.recv_timeout(deadline.saturating_duration_since(Instant::now()))
    {
        results.insert(registry, result);
    }

    let limit = args.limit.unwrap_or(DEFAULT_ALL_LIMIT).clamp(1, MAX_LIMIT);
    let (mut counts, mut lines, mut failures) = (Vec::new(), String::new(), String::new());
    let mut needs_index = false;
    for registry in registries {
        let result = results
            .remove(&registry)
            .unwrap_or_else(|| Err(format!("no answer in {timeout:?}")));
        match result {
            Ok((found, total)) => {
                let shown = found.len().min(limit);
                needs_index |= registry != Registry::Ember && shown > 0;
                counts.push(format!("{} {shown} of {total}", registry.name()));
                for line in &found[..shown] {
                    lines.push_str(line);
                    lines.push('\n');
                }
            }
            Err(reason) => failures.push_str(&format!("- {}: {reason}\n", registry.name())),
        }
    }
    let mut text = if counts.is_empty() {
        "No registry answered.\n".to_string()
    } else {
        format!(
            "Matches in each registry's own order: {}.\n{lines}",
            counts.join(", ")
        )
    };
    if needs_index {
        text.push_str(
            "Before the first `embr add --index <name>` in a project, run `embr index add <name>` \
             once. It writes the index to [indices] in ember.toml.\n",
        );
    }
    if !failures.is_empty() {
        text.push_str(&format!("Failed registries:\n{failures}"));
    }
    text
}

/// the live [`Fetch`]: one search request, plus the mirror checks for Forest.
fn fetch(registry: Registry, query: &str) -> Result<(Vec<String>, usize), String> {
    fetch_at(API_URL, registry, query)
}

/// [`fetch`], with ember's registry API at `api`.
fn fetch_at(api: &str, registry: Registry, query: &str) -> Result<(Vec<String>, usize), String> {
    let encoded = encode(query);
    // the other registries give no total that search_all reads
    let counted = |lines: Vec<String>| {
        let total = lines.len();
        (lines, total)
    };
    let found = match registry {
        Registry::Ember => ember_search(api, query, None, None)
            .map(|page| (ember_lines(&page.results), page.total)),
        Registry::Wally => http::get_json(
            &format!("{WALLY_API}/v1/package-search?query={encoded}"),
            &[],
        )
        .map(wally_lines)
        .map(counted),
        Registry::Pesde => http::get_json(&format!("{PESDE_API}/v1/search?query={encoded}"), &[])
            .map(pesde_lines)
            .map(counted),
        // 50 is the most rows that Forest gives in one page
        Registry::Forest => http::get_json(
            &format!(
                "{}/v1/search/packages?q={encoded}&limit=50",
                forest::API_URL
            ),
            &[],
        )
        .and_then(|page: ForestPage| forest_lines(page.packages, forest_copy))
        .map(counted),
    };
    found.map_err(|error| error.to_string())
}

/// percent-encodes `text` for a URL, as JavaScript's encodeURIComponent does.
fn encode(text: &str) -> String {
    text.bytes()
        .map(|byte| match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                char::from(byte).to_string()
            }
            _ => format!("%{byte:02X}"),
        })
        .collect()
}

/// one search_all line for a Wally, pesde, or Forest package.
fn registry_line(
    registry: Registry,
    name: &str,
    version: &str,
    environments: &[String],
    description: &Option<String>,
) -> String {
    let mut parts = vec![format!("{} {name} {version}", registry.name())];
    if !environments.is_empty() {
        parts.push(environments.join(", "));
    }
    if let Some(description) = non_empty(description) {
        parts.push(description.to_string());
    }
    parts.push(format!(
        "install: embr add {name} --index {}",
        registry.name()
    ));
    format!("- {}", parts.join(" | "))
}

/// the search_packages lines with the registry name.
fn ember_lines(entries: &[SearchEntry]) -> Vec<String> {
    entries
        .iter()
        .map(|entry| entry.line().replacen("- ", "- ember ", 1))
        .collect()
}

/// one result of Wally's `GET /v1/package-search`.
#[derive(Deserialize)]
struct WallyPackage {
    scope: String,
    name: String,
    description: Option<String>,
    /// newest first, so the first one is the latest.
    versions: Vec<String>,
}

/// Wally has Roblox packages only, so its results carry no environment.
fn wally_lines(packages: Vec<WallyPackage>) -> Vec<String> {
    let roblox = [Environment::Roblox.to_string()];
    packages
        .iter()
        .map(|package| {
            let name = format!("{}/{}", package.scope, package.name);
            let version = package.versions.first().map_or("", String::as_str);
            registry_line(
                Registry::Wally,
                &name,
                version,
                &roblox,
                &package.description,
            )
        })
        .collect()
}

/// pesde's `GET /v1/search`, one page of 50 results.
#[derive(Deserialize)]
struct PesdePage {
    data: Vec<PesdePackage>,
}

#[derive(Deserialize)]
struct PesdePackage {
    name: String,
    version: String,
    description: Option<String>,
    #[serde(default)]
    targets: Vec<PesdeTarget>,
}

#[derive(Deserialize)]
struct PesdeTarget {
    kind: String,
}

fn pesde_lines(page: PesdePage) -> Vec<String> {
    page.data
        .iter()
        .map(|package| {
            let mut environments = Vec::new();
            for target in &package.targets {
                let name = pesde::parse_environment(&target.kind)
                    .map_or(target.kind.clone(), |env| env.to_string());
                if !environments.contains(&name) {
                    environments.push(name);
                }
            }
            registry_line(
                Registry::Pesde,
                &package.name,
                &package.version,
                &environments,
                &package.description,
            )
        })
        .collect()
}

/// Forest's `GET /v1/search/packages`.
#[derive(Deserialize)]
struct ForestPage {
    packages: Vec<ForestRow>,
}

#[derive(Deserialize)]
struct ForestRow {
    name: String,
    /// the scope
    owner: String,
    /// `"unclaimed"` for a scope with no Forest account
    #[serde(rename = "ownerType")]
    owner_type: String,
    /// roblox or uefn. embr installs Roblox packages only.
    platform: String,
    version: String,
    description: Option<String>,
}

/// `GET /v1/package/{scope}/{platform}/{name}/{version}`, the field read here.
#[derive(Deserialize)]
struct ForestVersion {
    /// the registry this version is copied from, `"wally"` today.
    #[serde(default, rename = "mirroredFrom")]
    mirrored_from: Option<String>,
}

/** Forest copies every open-source Wally package under the same name.
search_all lists only the packages published to Forest itself, as the
website does. an unclaimed scope has no Forest account, so only the copy
publishes there, and its rows drop out with no request. `is_copy` decides
each other row, at most [`FOREST_CHECKS`] of them. */
fn forest_lines(
    rows: Vec<ForestRow>,
    is_copy: impl Fn(&ForestRow) -> Result<bool, HttpError>,
) -> Result<Vec<String>, HttpError> {
    let roblox = [Environment::Roblox.to_string()];
    let mut lines = Vec::new();
    for row in rows
        .iter()
        .filter(|row| row.owner_type != "unclaimed" && row.platform == "roblox")
        .take(FOREST_CHECKS)
    {
        if !is_copy(row)? {
            // embr keys names in lowercase, and Forest matches any case
            let name = format!("{}/{}", row.owner, row.name).to_lowercase();
            lines.push(registry_line(
                Registry::Forest,
                &name,
                &row.version,
                &roblox,
                &row.description,
            ));
        }
    }
    Ok(lines)
}

/// the live `is_copy` of [`forest_lines`]: one request for the version metadata.
fn forest_copy(row: &ForestRow) -> Result<bool, HttpError> {
    let url = format!(
        "{}/v1/package/{}/roblox/{}/{}",
        forest::API_URL,
        encode(&row.owner.to_lowercase()),
        encode(&row.name.to_lowercase()),
        encode(&row.version)
    );
    let version: ForestVersion = http::get_json(&url, &[])?;
    Ok(version.mirrored_from.is_some())
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct InfoArgs {
    name: String,
}

fn package_info(arguments: Value) -> Result<String, Error> {
    let args: InfoArgs = serde_json::from_value(arguments)?;
    let name = args.name.trim().to_lowercase();
    // a bad name fails here, before the index clone
    split_package_name(&name)?;

    let index = Index::open(DEFAULT_INDEX_URL, Refresh::Ttl)?;
    /* the resolver's rule: a package that a TTL-fresh cache does not hold
    earns one forced refresh, since it can be minutes old */
    let (entries, deprecated) = match index.read(&name) {
        Err(Error::PackageNotFound { .. }) if index.ttl_skipped() => {
            Index::open(DEFAULT_INDEX_URL, Refresh::Force)?.read(&name)?
        }
        result => result?,
    };
    describe(&name, entries, deprecated.as_deref())
}

/** the package_info answer: what the package is, every version newest
first, and the dependencies of the newest version that is not yanked. */
fn describe(
    name: &str,
    mut entries: Vec<Candidate>,
    deprecated: Option<&str>,
) -> Result<String, Error> {
    entries.sort_by(|a, b| {
        b.version
            .cmp(&a.version)
            .then_with(|| a.target.cmp(&b.target))
    });
    let latest = entries.iter().find(|entry| !entry.yanked);

    let mut text = format!("{name}\n");
    if let Some(description) = latest
        .or(entries.first())
        .and_then(|entry| entry.entry.get("description"))
        .and_then(toml::Value::as_str)
    {
        text.push_str(&format!("{description}\n"));
    }
    if let Some(reason) = deprecated {
        text.push_str(&format!("Deprecated: {reason}\n"));
    }
    text.push_str(&format!(
        "Install: embr add {name}\n\nVersions, newest first:\n"
    ));
    if entries.is_empty() {
        text.push_str("- none that this embr can read\n");
    }
    for entry in &entries {
        let environment = entry
            .environment
            .map_or("none".to_string(), |env| env.to_string());
        let mut marks = vec![
            format!("target {environment}"),
            format!("language {}", entry.language),
        ];
        if entry.yanked {
            marks.push("yanked".to_string());
        }
        if let Some(reason) = &entry.deprecated {
            marks.push(format!("deprecated: {reason}"));
        }
        text.push_str(&format!("- {}: {}\n", entry.version, marks.join(", ")));
    }

    let Some(latest) = latest else {
        text.push_str("\nEvery version is yanked, so `embr add` finds none to install.\n");
        return Ok(text);
    };
    let environment = latest
        .environment
        .map_or("none".to_string(), |env| env.to_string());
    text.push_str(&format!(
        "\nDependencies of {} ({environment}):\n",
        latest.version
    ));
    let dependencies = pesde::parse_dependencies(&latest.entry, DEFAULT_INDEX_URL)?;
    if dependencies.is_empty() {
        text.push_str("- none\n");
    }
    for dependency in dependencies {
        text.push_str(&format!(
            "- {}: {} {}",
            dependency.alias, dependency.name, dependency.version_req
        ));
        if let Some(index) = dependency.index_url {
            text.push_str(&format!(" (index {index})"));
        }
        text.push('\n');
    }
    Ok(text)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// feeds `lines` to the server and returns its answers, parsed.
    fn exchange(lines: &[&str]) -> Vec<Value> {
        let mut output = Vec::new();
        serve(lines.join("\n").as_bytes(), &mut output).unwrap();
        String::from_utf8(output)
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect()
    }

    #[test]
    fn initialize_answers_and_notifications_do_not() {
        let answers = exchange(&[
            r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-03-26","capabilities":{},"clientInfo":{"name":"test","version":"0"}}}"#,
            r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#,
            r#"{"jsonrpc":"2.0","id":2,"method":"initialize","params":{"protocolVersion":"1999-01-01"}}"#,
            r#"{"jsonrpc":"2.0","id":3,"method":"ping"}"#,
        ]);
        assert_eq!(answers.len(), 3, "the notification gets no answer");

        let result = &answers[0]["result"];
        assert_eq!(answers[0]["id"], 1);
        assert_eq!(result["protocolVersion"], "2025-03-26");
        assert_eq!(result["capabilities"], json!({ "tools": {} }));
        assert_eq!(result["serverInfo"]["name"], "ember");
        assert_eq!(result["serverInfo"]["version"], env!("CARGO_PKG_VERSION"));
        assert!(
            result["instructions"]
                .as_str()
                .unwrap()
                .contains("search_all")
        );

        // a version the server does not know gets its default
        assert_eq!(
            answers[1]["result"]["protocolVersion"],
            DEFAULT_PROTOCOL_VERSION
        );
        assert_eq!(
            answers[2],
            json!({ "jsonrpc": "2.0", "id": 3, "result": {} })
        );
    }

    #[test]
    fn tools_list_gives_every_tool_with_a_schema() {
        let answers = exchange(&[r#"{"jsonrpc":"2.0","id":"t","method":"tools/list"}"#]);
        let tools = answers[0]["result"]["tools"].as_array().unwrap();
        let names: Vec<&str> = tools
            .iter()
            .map(|tool| tool["name"].as_str().unwrap())
            .collect();
        assert_eq!(names, ["search_packages", "search_all", "package_info"]);
        for tool in tools {
            assert!(!tool["description"].as_str().unwrap().is_empty());
            assert_eq!(tool["inputSchema"]["type"], "object");
        }
        assert_eq!(tools[0]["inputSchema"]["required"], json!(["query"]));
        assert_eq!(tools[1]["inputSchema"]["required"], json!(["query"]));
        assert_eq!(tools[2]["inputSchema"]["required"], json!(["name"]));
        for registry in tools[1]["inputSchema"]["properties"]["registries"]["items"]["enum"]
            .as_array()
            .unwrap()
        {
            serde_json::from_value::<Registry>(registry.clone()).unwrap();
        }

        // every name in the enums parses as the type the call reads
        let properties = &tools[0]["inputSchema"]["properties"];
        for env in properties["environment"]["enum"].as_array().unwrap() {
            serde_json::from_value::<Environment>(env.clone()).unwrap();
        }
        for language in properties["language"]["enum"].as_array().unwrap() {
            serde_json::from_value::<Language>(language.clone()).unwrap();
        }
    }

    #[test]
    fn unknown_methods_and_bad_json_get_errors() {
        let answers = exchange(&[
            r#"{"jsonrpc":"2.0","id":"a","method":"resources/list"}"#,
            "{not json",
            r#"{"jsonrpc":"2.0","method":"notifications/cancelled","params":{}}"#,
            r#"[{"jsonrpc":"2.0","id":4,"method":"ping"}]"#,
            r#"{"jsonrpc":"2.0","id":5,"result":{}}"#,
        ]);
        assert_eq!(
            answers.len(),
            3,
            "the notification and the response get no answer"
        );
        assert_eq!(answers[0]["id"], "a");
        assert_eq!(answers[0]["error"]["code"], METHOD_NOT_FOUND);
        assert_eq!(answers[1]["id"], Value::Null);
        assert_eq!(answers[1]["error"]["code"], PARSE_ERROR);
        assert_eq!(answers[2]["error"]["code"], INVALID_REQUEST);
    }

    #[test]
    fn tool_failures_set_is_error() {
        // each call fails on its arguments, before any network call
        let answers = exchange(&[
            r#"{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"search_packages","arguments":{"environment":"roblox"}}}"#,
            r#"{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"search_packages","arguments":{"query":"x","environment":"cobol"}}}"#,
            r#"{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"package_info","arguments":{"name":"not a name"}}}"#,
            r#"{"jsonrpc":"2.0","id":4,"method":"tools/call","params":{"name":"search_all","arguments":{"query":"x","registries":["npm"]}}}"#,
            r#"{"jsonrpc":"2.0","id":5,"method":"tools/call","params":{"name":"install","arguments":{}}}"#,
        ]);
        for answer in &answers[..4] {
            assert_eq!(answer["result"]["isError"], true);
            assert_eq!(answer["result"]["content"][0]["type"], "text");
        }
        let text = |index: usize| {
            answers[index]["result"]["content"][0]["text"]
                .as_str()
                .unwrap()
        };
        assert!(text(0).contains("query"), "{}", text(0));
        assert!(text(1).contains("cobol"), "{}", text(1));
        assert!(text(2).contains("not a name"), "{}", text(2));
        assert!(text(3).contains("npm"), "{}", text(3));
        // a tool that does not exist is a protocol error
        assert_eq!(answers[4]["error"]["code"], INVALID_PARAMS);
    }

    /** a `/v1/search` answer in the live shape, most downloaded first: a
    row from before target IDs, and a row with every field the registry adds. */
    const SEARCH_PAGE: &str = r#"{"total":6,"page":1,"per_page":50,"results":[
        {"name":"chief/core","version":"0.2.0","target":"shared","type":"roblox","description":"The core of Chief, a Luau module framework.","publishers":["ryancundiff"],"downloads":145},
        {"name":"chief/bin","version":"0.2.0","target":"shared","type":"roblox","description":"A cleanup container.","downloads":117},
        {"name":"acme/signal","version":"1.0.0","target":"roblox","type":"roblox","targets":["roblox","lune"],"language":"alloy","languages":["alloy"],"logo":"https://cdn.luaupm.com/acme/signal/logo.png","description":"Typed signals for Chief users.","downloads":3,"deprecated":"use acme/signal2"},
        {"name":"mayari/core","version":"0.3.0","target":"luau","type":"luau","targets":["luau"],"description":"Mayari core.","deprecated":""}]}"#;

    fn sample() -> SearchPage {
        serde_json::from_str(SEARCH_PAGE).unwrap()
    }

    fn names(text: &str) -> Vec<&str> {
        text.lines()
            .skip(1)
            .map(|line| line.split(' ').nth(1).unwrap())
            .collect()
    }

    fn search_for(args: Value) -> String {
        search(&sample(), &serde_json::from_value(args).unwrap())
    }

    /** a local registry API. it answers every request with `body` as JSON,
    and keeps the path and query of each request. */
    fn api(body: &'static str) -> (String, std::sync::Arc<std::sync::Mutex<Vec<String>>>) {
        use std::io::Read;

        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let paths = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let seen = paths.clone();
        std::thread::spawn(move || {
            for mut stream in listener.incoming().flatten() {
                let mut request = [0u8; 4096];
                let read = stream.read(&mut request).unwrap_or(0);
                // the request line is "GET /v1/search?q=... HTTP/1.1"
                let request = String::from_utf8_lossy(&request[..read]).into_owned();
                if let Some(path) = request.split(' ').nth(1) {
                    seen.lock().unwrap().push(path.to_string());
                }
                let _ = write!(
                    stream,
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\n\
                     Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
            }
        });
        (url, paths)
    }

    #[test]
    fn search_asks_the_api_for_one_page_and_reads_its_total() {
        let (url, paths) = api(SEARCH_PAGE);
        let text = search_packages_at(
            &url,
            json!({ "query": " Chief/ ", "environment": "roblox_server", "language": "alloy", "limit": 2 }),
        )
        .unwrap();
        assert!(
            text.starts_with("2 of 6 matching packages, most downloaded first:\n"),
            "{text}"
        );
        assert_eq!(names(&text), ["chief/core", "chief/bin"]);

        // search_all sends the same URL for the same query, so the API's cache answers it
        let (lines, total) = fetch_at(&url, Registry::Ember, "chief/").unwrap();
        assert_eq!(total, 6);
        assert_eq!(lines.len(), 4);
        assert!(lines[0].starts_with("- ember chief/core 0.2.0 | roblox |"));
        assert_eq!(
            *paths.lock().unwrap(),
            [
                "/v1/search?q=chief%2F&sort=downloads&page=1&per_page=50\
                 &environment=roblox_server&language=alloy",
                "/v1/search?q=chief%2F&sort=downloads&page=1&per_page=50",
            ]
        );
    }

    #[test]
    fn search_answers_no_match_in_words() {
        let empty: SearchPage = serde_json::from_str(r#"{"total":0,"results":[]}"#).unwrap();
        let args = serde_json::from_value(json!({ "query": "nothing" })).unwrap();
        assert_eq!(search(&empty, &args), "No package matches \"nothing\".");
    }

    #[test]
    fn search_lines_give_what_an_agent_needs() {
        let text = search_for(json!({ "query": "" }));
        let line = |n: usize| text.lines().nth(n).unwrap();
        // shared reads as roblox
        assert_eq!(
            line(1),
            "- chief/core 0.2.0 | roblox | 145 downloads | \
             The core of Chief, a Luau module framework. | install: embr add chief/core"
        );
        assert_eq!(
            line(3),
            "- acme/signal 1.0.0 | roblox, lune | language: alloy | 3 downloads | \
             Typed signals for Chief users. | deprecated: use acme/signal2 | install: embr add acme/signal"
        );
        // no downloads count reads as 0, and an empty reason is no deprecation
        assert_eq!(
            line(4),
            "- mayari/core 0.3.0 | luau | 0 downloads | Mayari core. | install: embr add mayari/core"
        );
    }

    #[test]
    fn search_limit_stays_in_range() {
        let text = search_for(json!({ "query": "chief", "limit": 1 }));
        assert_eq!(names(&text), ["chief/core"]);
        assert!(text.starts_with("1 of 6 matching packages"));
        // 0 asks for nothing useful, so the answer holds one package
        assert_eq!(
            names(&search_for(json!({ "query": "", "limit": 0 }))).len(),
            1
        );
        let text = search_for(json!({ "query": "", "limit": 500 }));
        assert_eq!(names(&text).len(), 4);
        assert!(text.starts_with("4 of 6 matching packages"));
    }

    // trimmed from api.wally.run/v1/package-search?query=signal
    const WALLY: &str = r#"[
        {"description":"Signal class","name":"signal","scope":"sleitnick","versions":["2.0.3"]},
        {"description":null,"name":"signal","scope":"ahmoin","versions":["1.0.1"]}]"#;

    // trimmed from registry.pesde.dev/v1/search?query=signal
    const PESDE: &str = r#"{"count":15,"data":[
        {"name":"molyidev/signal","version":"1.0.3","description":"Signal library.","license":"MIT",
         "published_at":"2026-02-04T21:48:17.91476814Z","targets":[{"bin":false,"kind":"roblox","lib":true}]},
        {"name":"ewdev/signal","version":"0.1.0","description":"Cross runtime and typed signal implementation",
         "published_at":"2025-11-01T06:30:05.174094223Z","targets":[{"bin":false,"kind":"roblox","lib":true},
         {"bin":false,"kind":"lune","lib":true},{"bin":false,"kind":"luau","lib":true}]}]}"#;

    // trimmed from api.forest.dev/v1/search/packages?q=signal&limit=50
    const FOREST: &str = r#"{"packages":[
        {"name":"signal","description":"Signal class","platform":"roblox",
         "version":"3.2.0","owner":"finobinos","ownerType":"unclaimed","likes":0},
        {"name":"Signal","description":"Custom event/signal implementation that avoids Roblox's deferred BindableEvent behavior.",
         "platform":"roblox","version":"1.1.1","owner":"stratiz","ownerType":"user","likes":1}],
        "page":1,"limit":50,"hasMore":true}"#;

    // trimmed from api.forest.dev/v1/package/stratiz/roblox/signal/1.1.1
    const FOREST_ORIGINAL: &str = r#"{"name":"Signal","scope":"stratiz","version":"1.1.1","dependencies":{},
        "integrity":"73b0e11275f60547042a508caf7b08c61c27f25096a54119d6a272177b508e37",
        "ownerType":"user","mirroredFrom":null}"#;

    // trimmed from api.forest.dev/v1/package/sleitnick/roblox/knit/1.7.0
    const FOREST_COPY: &str = r#"{"name":"knit","scope":"sleitnick","version":"1.7.0",
        "integrity":"7999efe9356b4f0db3951c555cd36b2bafaf33328352d7417e2c771e4dc5a94e",
        "ownerType":"unclaimed","mirroredFrom":"wally"}"#;

    #[test]
    fn registry_results_become_lines() {
        assert_eq!(
            ember_lines(&sample().results[2..3]),
            [
                "- ember acme/signal 1.0.0 | roblox, lune | language: alloy | 3 downloads | \
              Typed signals for Chief users. | deprecated: use acme/signal2 | install: embr add acme/signal"
            ]
        );
        assert_eq!(
            wally_lines(serde_json::from_str(WALLY).unwrap()),
            [
                "- wally sleitnick/signal 2.0.3 | roblox | Signal class | install: embr add sleitnick/signal --index wally",
                "- wally ahmoin/signal 1.0.1 | roblox | install: embr add ahmoin/signal --index wally",
            ]
        );
        assert_eq!(
            pesde_lines(serde_json::from_str(PESDE).unwrap()),
            [
                "- pesde molyidev/signal 1.0.3 | roblox | Signal library. | install: embr add molyidev/signal --index pesde",
                "- pesde ewdev/signal 0.1.0 | roblox, lune, luau | Cross runtime and typed signal implementation | \
                 install: embr add ewdev/signal --index pesde",
            ]
        );
    }

    fn forest_row(owner: &str, name: &str, owner_type: &str, platform: &str) -> ForestRow {
        ForestRow {
            name: name.to_string(),
            owner: owner.to_string(),
            owner_type: owner_type.to_string(),
            platform: platform.to_string(),
            version: "1.0.0".to_string(),
            description: None,
        }
    }

    #[test]
    fn forest_leaves_out_copies_of_wally_packages() {
        let mut rows = serde_json::from_str::<ForestPage>(FOREST).unwrap().packages;
        // a claimed scope that holds a copy, and a UEFN package
        rows.push(forest_row("sleitnick", "knit", "user", "roblox"));
        rows.push(forest_row("epic", "Signal", "organization", "uefn"));
        let checked = std::cell::RefCell::new(Vec::new());
        let lines = forest_lines(rows, |row| {
            checked.borrow_mut().push(row.owner.clone());
            Ok(row.name == "knit")
        })
        .unwrap();
        assert_eq!(
            lines,
            [
                "- forest stratiz/signal 1.1.1 | roblox | Custom event/signal implementation that avoids \
              Roblox's deferred BindableEvent behavior. | install: embr add stratiz/signal --index forest"
            ]
        );
        // the unclaimed row and the UEFN row cost no request
        assert_eq!(*checked.borrow(), ["stratiz", "sleitnick"]);

        // the live check reads mirroredFrom
        let copy: ForestVersion = serde_json::from_str(FOREST_COPY).unwrap();
        let original: ForestVersion = serde_json::from_str(FOREST_ORIGINAL).unwrap();
        assert_eq!(copy.mirrored_from.as_deref(), Some("wally"));
        assert_eq!(original.mirrored_from, None);

        // the checks stop at the cap, and a failed check fails the registry
        let many = || (0..20).map(|n| forest_row("acme", &format!("p{n}"), "user", "roblox"));
        let count = std::cell::Cell::new(0);
        let lines = forest_lines(many().collect(), |_| {
            count.set(count.get() + 1);
            Ok(false)
        })
        .unwrap();
        assert_eq!((lines.len(), count.get()), (FOREST_CHECKS, FOREST_CHECKS));
        assert!(forest_lines(many().collect(), |_| Err(HttpError::RateLimited)).is_err());
    }

    /// pesde fails, and Forest answers after the deadline of the tests.
    /// ember finds 3 packages and gives the line of 1.
    fn fake(registry: Registry, query: &str) -> Result<(Vec<String>, usize), String> {
        match registry {
            Registry::Ember => Ok((vec![format!("- ember acme/{query}")], 3)),
            Registry::Wally => Ok((
                (1..=30)
                    .map(|n| format!("- wally acme/{query}{n}"))
                    .collect(),
                30,
            )),
            Registry::Pesde => Err("request failed: status code 503".to_string()),
            Registry::Forest => {
                std::thread::sleep(Duration::from_secs(2));
                Ok((vec!["- forest late/package".to_string()], 1))
            }
        }
    }

    const INDEX_NOTE: &str = "Before the first `embr add --index <name>` in a project, \
        run `embr index add <name>` once. It writes the index to [indices] in ember.toml.\n";

    fn search_fake(args: Value) -> String {
        let args = serde_json::from_value(args).unwrap();
        search_registries(&args, fake, Duration::from_millis(300))
    }

    #[test]
    fn search_all_lists_failed_registries_and_keeps_the_rest() {
        let started = Instant::now();
        let text = search_fake(json!({ "query": " Signal " }));
        assert!(
            started.elapsed() < Duration::from_secs(1),
            "the slow registry held the answer"
        );
        let wally: String = (1..=10)
            .map(|n| format!("- wally acme/signal{n}\n"))
            .collect();
        assert_eq!(
            text,
            format!(
                "Matches in each registry's own order: ember 1 of 3, wally 10 of 30.\n\
                 - ember acme/signal\n\
                 {wally}\
                 {INDEX_NOTE}\
                 Failed registries:\n\
                 - pesde: request failed: status code 503\n\
                 - forest: no answer in 300ms\n"
            )
        );
    }

    #[test]
    fn search_all_limit_and_registries_choose_the_answer() {
        assert_eq!(
            search_fake(json!({ "query": "signal", "registries": ["wally"], "limit": 2 })),
            format!(
                "Matches in each registry's own order: wally 2 of 30.\n\
                 - wally acme/signal1\n\
                 - wally acme/signal2\n\
                 {INDEX_NOTE}"
            )
        );
        // the limit stays in range
        let wally = |limit: usize| {
            search_fake(json!({ "query": "signal", "registries": ["wally"], "limit": limit }))
        };
        assert!(wally(0).contains("wally 1 of 30"));
        assert!(wally(500).contains("wally 30 of 30"));

        // a short query reaches ember only
        assert_eq!(
            search_fake(json!({ "query": "a", "registries": ["wally", "ember"] })),
            "Matches in each registry's own order: ember 1 of 3.\n\
             - ember acme/a\n\
             Failed registries:\n\
             - wally: the query needs 2 or more characters\n"
        );
        assert_eq!(
            search_fake(json!({ "query": "x", "registries": ["pesde"] })),
            "No registry answered.\n\
             Failed registries:\n\
             - pesde: the query needs 2 or more characters\n"
        );
    }

    #[test]
    fn encode_escapes_what_a_query_cannot_hold() {
        assert_eq!(encode("chief/core ü&q=1"), "chief%2Fcore%20%C3%BC%26q%3D1");
        assert_eq!(encode("a-b_c.d~e"), "a-b_c.d~e");
    }

    fn candidate(version: &str, yanked: bool, deprecated: Option<&str>, entry: &str) -> Candidate {
        Candidate {
            version: semver::Version::parse(version).unwrap(),
            target: Some("shared".to_string()),
            environment: Some(Environment::Roblox),
            language: Language::Luau,
            yanked,
            deprecated: deprecated.map(str::to_string),
            entry: toml::from_str(entry).unwrap(),
        }
    }

    #[test]
    fn describe_lists_versions_and_live_dependencies() {
        let entries = vec![
            candidate("0.1.0", false, Some("leaks"), r#"description = "old""#),
            candidate(
                "0.3.0",
                true,
                None,
                "description = \"yanked\"\n[dependencies.core]\nname = \"chief/core\"\nversion = \"^0.3.0\"",
            ),
            candidate(
                "0.2.0",
                false,
                None,
                "description = \"Binds events.\"\n[dependencies.core]\nname = \"chief/core\"\nversion = \"^0.2.0\"",
            ),
        ];
        let text = describe("chief/lifecycles", entries, Some("use chief/events")).unwrap();
        assert_eq!(
            text,
            "chief/lifecycles\n\
             Binds events.\n\
             Deprecated: use chief/events\n\
             Install: embr add chief/lifecycles\n\
             \n\
             Versions, newest first:\n\
             - 0.3.0: target roblox, language luau, yanked\n\
             - 0.2.0: target roblox, language luau\n\
             - 0.1.0: target roblox, language luau, deprecated: leaks\n\
             \n\
             Dependencies of 0.2.0 (roblox):\n\
             - core: chief/core ^0.2.0\n"
        );

        let all_yanked =
            describe("acme/gone", vec![candidate("1.0.0", true, None, "")], None).unwrap();
        assert!(
            all_yanked.ends_with("Every version is yanked, so `embr add` finds none to install.\n")
        );
    }
}
