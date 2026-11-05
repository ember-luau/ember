/*! the tool shorthands that the registry adds to the built-in ones.

`GET /v1/shorthands` gives every name and its target. embr keeps the answer
in ~/.ember/shorthands.json and asks again when the file is a day old, or
when it is five minutes old and does not have the name. a
failed or slow request uses the old file, then no registry names at all.
it prints one line and never fails the command. the built-in list in
`super::SHORTHANDS` always wins, so the registry cannot re-point one. */

use crate::net::http::{self, error::HttpError};
use crate::registry::index;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fs;
use std::path::Path;
use std::sync::OnceLock;
use std::time::Duration;

/// how long the kept list is fresh.
const TTL: Duration = Duration::from_secs(24 * 60 * 60);

/** how old the kept list may be when it does not have a name. an older list
is asked again once, so a shorthand that a developer adds works at once,
and a typo asks the registry at most once in this time. */
const MISS_TTL: Duration = Duration::from_secs(5 * 60);

/** the longest a refresh may take. a command waits this long at most for a
name the built-ins do not know. */
const TIMEOUT: Duration = Duration::from_secs(3);

type Shorthands = BTreeMap<String, String>;

/// the answer of `GET /v1/shorthands`, and the file embr keeps it in.
#[derive(Serialize, Deserialize)]
struct List {
    shorthands: Shorthands,
}

/** whether `name` can be a registry shorthand: 1 to 40 letters, digits and
hyphens. anything else, like owner/repo, never asks the registry. */
pub fn is_name(name: &str) -> bool {
    (1..=40).contains(&name.len())
        && name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
}

/// the registry's target for `name`, from a list read once a run.
#[cfg(not(test))]
pub fn lookup(name: &str) -> Option<String> {
    static KEPT: OnceLock<Shorthands> = OnceLock::new();
    static AGAIN: OnceLock<Shorthands> = OnceLock::new();
    let file = crate::sys::paths::shorthands_file().ok()?;
    find(name, &crate::net::registry::api_url(), &file, &KEPT, &AGAIN)
}

/** `name` in the kept list. on a miss, `name` in a list at most `MISS_TTL`
old, which asks the registry again when the kept one is older. each cell
loads once, so one run asks at most twice. */
fn find(
    name: &str,
    api: &str,
    file: &Path,
    kept: &OnceLock<Shorthands>,
    again: &OnceLock<Shorthands>,
) -> Option<String> {
    let key = name.to_ascii_lowercase();
    kept.get_or_init(|| load(api, file, TTL))
        .get(&key)
        .or_else(|| again.get_or_init(|| load(api, file, MISS_TTL)).get(&key))
        .cloned()
}

/** unit tests never ask the real registry. `load` has its own tests on a
local server. */
#[cfg(test)]
pub fn lookup(_name: &str) -> Option<String> {
    None
}

/** the list in `file` while it is younger than `ttl`. else a new list from
`api`, which then replaces the file, or on a failure the old file. */
fn load(api: &str, file: &Path, ttl: Duration) -> Shorthands {
    if index::stamp_fresh(file, ttl)
        && let Some(list) = read(file)
    {
        return list;
    }
    match fetch(api) {
        Ok(list) => {
            /* best effort. a file that cannot be written only costs one
            request on the next run */
            if let Some(parent) = file.parent() {
                let _ = fs::create_dir_all(parent);
            }
            if let Ok(bytes) = serde_json::to_vec(&list) {
                let _ = fs::write(file, bytes);
            }
            list.shorthands
        }
        Err(error) => {
            let kept = read(file);
            eprintln!(
                "warning: could not refresh the tool shorthands ({error}); using {}",
                if kept.is_some() {
                    "the list from the last refresh"
                } else {
                    "the built-in shorthands only"
                }
            );
            kept.unwrap_or_default()
        }
    }
}

fn read(file: &Path) -> Option<Shorthands> {
    let list: List = serde_json::from_slice(&fs::read(file).ok()?).ok()?;
    Some(list.shorthands)
}

fn fetch(api: &str) -> Result<List, HttpError> {
    let agent = ureq::AgentBuilder::new().timeout(TIMEOUT).build();
    let response = agent
        .get(&format!("{api}/v1/shorthands"))
        .set("User-Agent", http::USER_AGENT)
        .call()?;
    Ok(response.into_json()?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::net::http::mock;

    const ANSWER: &str = r#"{"shorthands":{"azul":"npm:azul-sync","rojo":"evil/rojo"}}"#;

    fn file_in(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(name);
        let _ = fs::remove_dir_all(&dir);
        dir.join("shorthands.json")
    }

    fn age(file: &Path, by: Duration) {
        fs::File::options()
            .write(true)
            .open(file)
            .unwrap()
            .set_modified(std::time::SystemTime::now() - by)
            .unwrap();
    }

    #[test]
    fn only_bare_names_can_be_registry_shorthands() {
        for name in ["azul", "luau-lsp", "Tool2"] {
            assert!(is_name(name), "{name}");
        }
        for name in [
            "",
            "acme/tool",
            "npm:azul-sync",
            "tool@1.0.0",
            "a b",
            "a\\b",
        ] {
            assert!(!is_name(name), "{name}");
        }
        assert!(!is_name(&"a".repeat(41)));
    }

    #[test]
    fn a_fresh_file_answers_with_no_request() {
        let file = file_in("embr-test-shorthands-fresh");
        let (api, log) = mock::serve(|_| mock::reply("200 OK", ANSWER));

        // no file yet: one request, and the answer is kept
        let list = load(&api, &file, TTL);
        assert_eq!(list["azul"], "npm:azul-sync");
        assert_eq!(log.lock().unwrap().len(), 1);
        assert!(log.lock().unwrap()[0].starts_with("get /v1/shorthands "));
        assert!(file.exists());

        // a fresh file: no request
        assert_eq!(load(&api, &file, TTL)["azul"], "npm:azul-sync");
        assert_eq!(log.lock().unwrap().len(), 1);
        let _ = fs::remove_dir_all(file.parent().unwrap());
    }

    #[test]
    fn a_day_old_file_is_asked_again_and_replaced() {
        let file = file_in("embr-test-shorthands-stale");
        fs::create_dir_all(file.parent().unwrap()).unwrap();
        fs::write(&file, r#"{"shorthands":{"old":"acme/old"}}"#).unwrap();
        age(&file, TTL + Duration::from_secs(60));
        let (api, log) = mock::serve(|_| mock::reply("200 OK", ANSWER));

        let list = load(&api, &file, TTL);
        assert_eq!(log.lock().unwrap().len(), 1);
        assert_eq!(list.get("old"), None);
        assert_eq!(list["azul"], "npm:azul-sync");
        assert_eq!(read(&file).unwrap()["azul"], "npm:azul-sync");
        // the new file is fresh
        assert!(index::stamp_fresh(&file, TTL));
        let _ = fs::remove_dir_all(file.parent().unwrap());
    }

    #[test]
    fn a_name_that_the_kept_list_lacks_asks_again_once() {
        let file = file_in("embr-test-shorthands-miss");
        fs::create_dir_all(file.parent().unwrap()).unwrap();
        fs::write(&file, r#"{"shorthands":{"rojo":"rojo-rbx/rojo"}}"#).unwrap();
        let answer = r#"{"shorthands":{"rojo":"rojo-rbx/rojo","selene":"Kampfkarren/selene"}}"#;
        let (api, log) = mock::serve(move |_| mock::reply("200 OK", answer));

        // a list younger than MISS_TTL: a miss asks nothing
        let (kept, again) = (OnceLock::new(), OnceLock::new());
        assert_eq!(find("selene", &api, &file, &kept, &again), None);
        assert_eq!(log.lock().unwrap().len(), 0);

        // an older list, fresh for TTL: a hit asks nothing, a miss asks once
        age(&file, MISS_TTL + Duration::from_secs(60));
        let (kept, again) = (OnceLock::new(), OnceLock::new());
        assert_eq!(
            find("Rojo", &api, &file, &kept, &again).unwrap(),
            "rojo-rbx/rojo"
        );
        assert_eq!(log.lock().unwrap().len(), 0);
        assert_eq!(
            find("selene", &api, &file, &kept, &again).unwrap(),
            "Kampfkarren/selene"
        );
        assert_eq!(find("typo", &api, &file, &kept, &again), None);
        assert_eq!(log.lock().unwrap().len(), 1);
        // the new answer replaced the file
        assert_eq!(read(&file).unwrap()["selene"], "Kampfkarren/selene");
        let _ = fs::remove_dir_all(file.parent().unwrap());
    }

    #[test]
    fn a_failed_refresh_uses_the_old_file_then_nothing() {
        let file = file_in("embr-test-shorthands-offline");
        let (api, log) = mock::serve(|_| mock::reply("503 Service Unavailable", "{}"));

        // no file: no registry names, and no error
        assert!(load(&api, &file, TTL).is_empty());
        assert!(!file.exists());

        // a stale file: its names, and it stays for the next run to ask again
        fs::create_dir_all(file.parent().unwrap()).unwrap();
        fs::write(&file, ANSWER).unwrap();
        age(&file, TTL + Duration::from_secs(60));
        assert_eq!(load(&api, &file, TTL)["azul"], "npm:azul-sync");
        assert!(!index::stamp_fresh(&file, TTL));
        assert_eq!(log.lock().unwrap().len(), 2);

        // an answer that is not the list counts as a failure too
        let (bad, _) = mock::serve(|_| mock::reply("200 OK", "<html>"));
        assert_eq!(load(&bad, &file, TTL)["azul"], "npm:azul-sync");
        let _ = fs::remove_dir_all(file.parent().unwrap());
    }
}
