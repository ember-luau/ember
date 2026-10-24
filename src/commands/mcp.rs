/*!
`embr mcp`, a Model Context Protocol server on stdio. A coding agent starts
it and calls two tools: a package search and a package lookup. The server
only reads. It writes no files, installs nothing, and publishes nothing.

The protocol is JSON-RPC 2.0, one JSON object per line. Only protocol
messages go to stdout, so any log goes to stderr.
*/

use crate::error::Error;
use crate::net::http;
use crate::project::manifest::{DEFAULT_INDEX_URL, Environment, Language, split_package_name};
use crate::registry::index::pesde::{self, Candidate};
use crate::registry::index::{Index, Refresh};
use serde::Deserialize;
use serde_json::{Value, json};
use std::io::{BufRead, Write};

/// the registry's search file, one entry for each package.
const SEARCH_INDEX_URL: &str = "https://cdn.luaupm.com/search-index.json";

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
search_packages finds packages in the ember registry by name or description. \
package_info shows the versions, deprecations, and dependencies of one package. \
Both tools only read. Each answer gives the `embr add <scope/name>` command that installs a package.";

/// the search answer holds this many packages when the call sets no limit.
const DEFAULT_LIMIT: usize = 20;
const MAX_LIMIT: usize = 50;

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
                An empty query matches every package. \
                The answer lists the most downloaded packages first, one on each line. \
                Each line gives the name, the latest version, the environments, and the download count. \
                It also gives the language when it is not luau, the description, a deprecation reason, and the install command.",
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

/** one package in the registry's search file. the registry adds `targets`,
`language`, `yanked`, and `deprecated` to each entry, so older entries lack
them. serde ignores keys this struct does not name, like `logo`. */
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
    /// set only when every version is yanked
    #[serde(default)]
    yanked: bool,
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
        if self.yanked {
            parts.push("yanked: every version".to_string());
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
    let args: SearchArgs = serde_json::from_value(arguments)?;
    // ponytail: fetches the whole file on each call. it is 4 KB today; cache it if it grows
    let entries: Vec<SearchEntry> = http::get_json(SEARCH_INDEX_URL, &[])?;
    Ok(search(&entries, &args))
}

/// the matches for `args` in `entries`, most downloaded first, as text.
fn search(entries: &[SearchEntry], args: &SearchArgs) -> String {
    let query = args.query.trim().to_lowercase();
    let mut matches: Vec<&SearchEntry> = entries
        .iter()
        .filter(|entry| {
            entry.name.to_lowercase().contains(&query)
                || entry
                    .description
                    .as_deref()
                    .is_some_and(|description| description.to_lowercase().contains(&query))
        })
        .filter(|entry| {
            args.environment
                .is_none_or(|env| entry.environments().contains(&env.to_string()))
        })
        .filter(|entry| {
            args.language
                .is_none_or(|language| entry.language() == language.as_str())
        })
        .collect();
    if matches.is_empty() {
        return format!("No package matches \"{}\".", args.query);
    }

    matches.sort_by(|a, b| {
        b.downloads
            .unwrap_or(0)
            .cmp(&a.downloads.unwrap_or(0))
            .then_with(|| a.name.cmp(&b.name))
    });
    let limit = args.limit.unwrap_or(DEFAULT_LIMIT).clamp(1, MAX_LIMIT);
    let mut text = format!(
        "{} of {} matching packages, most downloaded first:\n",
        matches.len().min(limit),
        matches.len()
    );
    for entry in matches.iter().take(limit) {
        text.push_str(&entry.line());
        text.push('\n');
    }
    text
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
                .contains("search_packages")
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
    fn tools_list_gives_both_tools_with_schemas() {
        let answers = exchange(&[r#"{"jsonrpc":"2.0","id":"t","method":"tools/list"}"#]);
        let tools = answers[0]["result"]["tools"].as_array().unwrap();
        let names: Vec<&str> = tools
            .iter()
            .map(|tool| tool["name"].as_str().unwrap())
            .collect();
        assert_eq!(names, ["search_packages", "package_info"]);
        for tool in tools {
            assert!(!tool["description"].as_str().unwrap().is_empty());
            assert_eq!(tool["inputSchema"]["type"], "object");
        }
        assert_eq!(tools[0]["inputSchema"]["required"], json!(["query"]));
        assert_eq!(tools[1]["inputSchema"]["required"], json!(["name"]));

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
            r#"{"jsonrpc":"2.0","id":4,"method":"tools/call","params":{"name":"install","arguments":{}}}"#,
        ]);
        for answer in &answers[..3] {
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
        // a tool that does not exist is a protocol error
        assert_eq!(answers[3]["error"]["code"], INVALID_PARAMS);
    }

    /// the live file's shape, plus an entry with every field the registry adds.
    fn sample() -> Vec<SearchEntry> {
        serde_json::from_str(
            r#"[
                {"name":"chief/core","version":"0.2.0","target":"shared","type":"roblox","description":"The core of Chief, a Luau module framework.","publishers":["ryancundiff"],"downloads":145},
                {"name":"chief/bin","version":"0.2.0","target":"shared","type":"roblox","description":"A cleanup container.","downloads":117},
                {"name":"mayari/core","version":"0.3.0","target":"luau","type":"luau","description":"Mayari core."},
                {"name":"acme/signal","version":"1.0.0","target":"roblox","type":"roblox","targets":["roblox","lune"],"language":"alloy","logo":"https://cdn.luaupm.com/acme/signal/logo.png","description":"Typed signals for Chief users.","downloads":3,"deprecated":"use acme/signal2"},
                {"name":"acme/gone","version":"2.0.0","target":"luau","description":"","downloads":1,"deprecated":"","yanked":true}
            ]"#,
        )
        .unwrap()
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

    #[test]
    fn search_matches_names_and_descriptions_most_downloaded_first() {
        let text = search_for(json!({ "query": "CHIEF" }));
        assert_eq!(names(&text), ["chief/core", "chief/bin", "acme/signal"]);
        assert!(text.starts_with("3 of 3 matching packages"));

        // an empty query matches every package
        assert_eq!(names(&search_for(json!({ "query": "" }))).len(), 5);
        assert_eq!(
            search_for(json!({ "query": "nothing" })),
            "No package matches \"nothing\"."
        );
    }

    #[test]
    fn search_filters_by_environment_and_language() {
        let filter = |args: Value| names(&search_for(args)).join(" ");
        // shared reads as roblox, and a targets list wins over the one target
        assert_eq!(
            filter(json!({ "query": "", "environment": "roblox" })),
            "chief/core chief/bin acme/signal"
        );
        assert_eq!(
            filter(json!({ "query": "", "environment": "lune" })),
            "acme/signal"
        );
        assert_eq!(
            filter(json!({ "query": "", "environment": "luau" })),
            "acme/gone mayari/core"
        );
        assert_eq!(
            filter(json!({ "query": "", "language": "alloy" })),
            "acme/signal"
        );
        // an entry with no language is luau
        assert_eq!(
            filter(json!({ "query": "chief", "language": "luau" })),
            "chief/core chief/bin"
        );
    }

    #[test]
    fn search_lines_give_what_an_agent_needs() {
        let text = search_for(json!({ "query": "signal" }));
        assert_eq!(
            text.lines().nth(1).unwrap(),
            "- acme/signal 1.0.0 | roblox, lune | language: alloy | 3 downloads | \
             Typed signals for Chief users. | deprecated: use acme/signal2 | install: embr add acme/signal"
        );
        // no downloads count reads as 0, and an empty reason is no deprecation
        let text = search_for(json!({ "query": "mayari" }));
        assert_eq!(
            text.lines().nth(1).unwrap(),
            "- mayari/core 0.3.0 | luau | 0 downloads | Mayari core. | install: embr add mayari/core"
        );
        let text = search_for(json!({ "query": "gone" }));
        assert_eq!(
            text.lines().nth(1).unwrap(),
            "- acme/gone 2.0.0 | luau | 1 download | yanked: every version | install: embr add acme/gone"
        );
    }

    #[test]
    fn search_limit_stays_in_range() {
        let text = search_for(json!({ "query": "chief", "limit": 1 }));
        assert_eq!(names(&text), ["chief/core"]);
        assert!(text.starts_with("1 of 3 matching packages"));
        // 0 asks for nothing useful, so the answer holds one package
        assert_eq!(
            names(&search_for(json!({ "query": "", "limit": 0 }))).len(),
            1
        );
        assert_eq!(
            names(&search_for(json!({ "query": "", "limit": 500 }))).len(),
            5
        );
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
