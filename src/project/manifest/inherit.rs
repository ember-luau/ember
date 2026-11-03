/*! workspace inheritance, Cargo-style. a value written as `{ workspace = true }`
takes the value at the same path under the root's [workspace] table, so
`package.version` reads `workspace.package.version`. [dev-dependencies] and
[peer-dependencies] read `workspace.dependencies`, the one shared table, as
in Cargo. it works on the
plain TOML before serde, so every field and every whole table can inherit. */

use crate::error::Error;
use crate::project::workspace;
use std::path::Path;
use toml::{Table, Value};

/** whether `table` asks for its value from the workspace. a string, such
as `{ workspace = "scope/name" }`, is a workspace dependency and not this. */
fn is_marker(table: &Table) -> bool {
    table.get("workspace").and_then(Value::as_bool) == Some(true)
}

/// whether any value in the manifest `raw` inherits. the root's own [workspace] table is never read for markers.
pub(super) fn inherits(raw: &Table) -> bool {
    fn any(value: &Value) -> bool {
        match value {
            Value::Table(table) => is_marker(table) || table.values().any(any),
            Value::Array(items) => items.iter().any(any),
            _ => false,
        }
    }
    raw.iter()
        .any(|(key, value)| key != "workspace" && any(value))
}

/** replaces each marker in `raw` with the value it inherits. `dir` holds the
manifest. a workspace root inherits from its own [workspace] table, as a
Cargo root package does, and any other manifest from the root that lists
its folder. */
pub(super) fn apply(raw: &mut Table, dir: &Path) -> Result<(), Error> {
    let dir = if dir.as_os_str().is_empty() {
        Path::new(".")
    } else {
        dir
    };
    /* ponytail: each member load walks the root's tree again to find it.
    pass the root's table down from `Workspace::open` if big repos get slow */
    let root = if workspace::member_globs(raw).is_empty() {
        workspace::root_of(dir)?.map(|(_, root)| root)
    } else {
        Some(raw.clone())
    };
    for (key, value) in raw.iter_mut() {
        if key != "workspace" {
            resolve(value, &mut vec![key.clone()], root.as_ref())?;
        }
    }
    Ok(())
}

/// `path` names `value` from the top of the manifest. array items keep the path of their array.
fn resolve(value: &mut Value, path: &mut Vec<String>, root: Option<&Table>) -> Result<(), Error> {
    match value {
        Value::Table(table) if is_marker(table) => {
            let inherited = inherited(table, path, root)?;
            *value = inherited;
        }
        Value::Table(table) => {
            for (key, item) in table.iter_mut() {
                path.push(key.clone());
                resolve(item, path, root)?;
                path.pop();
            }
        }
        Value::Array(items) => {
            for item in items {
                resolve(item, path, root)?;
            }
        }
        _ => {}
    }
    Ok(())
}

/** the value that the marker at `path` inherits from `root`, the root's
whole manifest. a dependency can add `target`, `entry` and `optional`:
they are the consumer's choice, as Cargo lets a member add `features` and
`optional`. any other key
beside `workspace = true` is an error, so nothing overrides a shared
value without a sign. */
fn inherited(marker: &Table, path: &[String], root: Option<&Table>) -> Result<Value, Error> {
    let field = path.join(".");
    let root = root.ok_or_else(|| Error::InheritOutsideWorkspace(field.clone()))?;
    let from: Vec<&str> = std::iter::once("workspace")
        .chain(path.iter().enumerate().map(|(depth, key)| {
            if depth == 0 && matches!(key.as_str(), "dev-dependencies" | "peer-dependencies") {
                "dependencies"
            } else {
                key.as_str()
            }
        }))
        .collect();
    let mut found = root.get(from[0]);
    for key in &from[1..] {
        found = found.and_then(|value| value.get(*key));
    }
    let mut value = found.cloned().ok_or_else(|| Error::InheritMissing {
        field: field.clone(),
        from: from.join("."),
    })?;

    let dependency = path.len() == 2
        && matches!(
            path[0].as_str(),
            "dependencies" | "dev-dependencies" | "peer-dependencies"
        );
    for (key, item) in marker {
        match (&mut value, key.as_str()) {
            (_, "workspace") => {}
            (Value::Table(table), "target" | "entry" | "optional") if dependency => {
                table.insert(key.clone(), item.clone());
            }
            _ => {
                return Err(Error::InheritExtraKey {
                    field,
                    key: key.clone(),
                });
            }
        }
    }
    Ok(value)
}

#[cfg(test)]
mod tests {
    use crate::error::Error;
    use crate::project::manifest::{Dependency, Manifest};
    use crate::project::workspace;
    use std::fs;
    use std::path::Path;

    fn write(dir: &Path, contents: &str) {
        fs::create_dir_all(dir).unwrap();
        fs::write(dir.join("ember.toml"), contents).unwrap();
    }

    /// the issue's tree: shared values in the root, members under places/*.
    fn write_workspace(base: &Path, member: &str) {
        let _ = fs::remove_dir_all(base);
        write(
            base,
            "[package]\nname = \"acme/root\"\nversion = { workspace = true }\nprivate = true\n\n\
             [target]\nenvironment = \"roblox\"\nworkspace = [\"places/*\"]\n\n\
             [workspace.package]\nversion = \"2.1.0\"\nauthors = [\"savruun\"]\n\n\
             [workspace.target]\nlanguage = \"alloy\"\n\n\
             [workspace.dependencies]\n\
             signal = { name = \"acme/signal\", version = \"^1.0.0\" }\n\
             core = { workspace = \"acme/core\", version = \"~\" }\n\n\
             [workspace.tools]\nrojo = \"rojo-rbx/rojo@7.4.4\"\n",
        );
        write(
            &base.join("places/core"),
            "[package]\nname = \"acme/core\"\nversion = \"1.0.0\"\n\n\
             [target]\nenvironment = \"roblox\"\nmain = \"src/init.luau\"\n",
        );
        write(&base.join("places/lobby"), member);
    }

    #[test]
    fn members_inherit_values_from_the_root() {
        let base = std::env::temp_dir().join("embr-test-inherit-values");
        write_workspace(
            &base,
            "[package]\nname = \"acme/lobby\"\nversion.workspace = true\n\
             authors = { workspace = true }\n\n\
             [target]\nenvironment = \"roblox\"\nmain = \"src/init.luau\"\n\
             language = { workspace = true }\n\n\
             [dependencies]\nsignal = { workspace = true, optional = true }\ncore = { workspace = true }\n\n\
             [dev-dependencies]\nspy = { name = \"acme/spy\", version = \"^1\" }\n\
             server = { workspace = true, target = \"server\" }\n\n\
             [peer-dependencies]\nserver = { workspace = true }\n\n\
             [tools]\nrojo = { workspace = true }\n",
        );
        // dev-dependencies read workspace.dependencies, so give it the entry
        let root = fs::read_to_string(base.join("ember.toml"))
            .unwrap()
            .replace(
                "[workspace.tools]",
                "server = { name = \"acme/server\", version = \"^3\" }\n\n[workspace.tools]",
            );
        fs::write(base.join("ember.toml"), root).unwrap();

        let lobby = Manifest::load_from(&base.join("places/lobby/ember.toml")).unwrap();
        let package = lobby.package.as_ref().unwrap();
        assert_eq!(package.version, "2.1.0");
        assert_eq!(package.authors, ["savruun"]);
        assert_eq!(lobby.target().unwrap().language.as_str(), "alloy");
        assert!(matches!(
            &lobby.dependencies["signal"],
            Dependency::Registry { name, version, .. } if name == "acme/signal" && version == "^1.0.0"
        ));
        assert!(matches!(
            &lobby.dependencies["core"],
            Dependency::Workspace { workspace, version, .. } if workspace == "acme/core" && version == "~"
        ));
        // the member adds its own `optional`, and a peer reads the shared table too
        assert!(lobby.dependencies["signal"].optional());
        assert_eq!(lobby.peer_dependencies["server"].package(), "acme/server");
        // the member adds its own `target` to the shared entry
        let server = &lobby.dev_dependencies["server"];
        assert_eq!(server.package(), "acme/server");
        assert_eq!(server.target(), Some("server"));
        assert_eq!(lobby.tools["rojo"].version, "7.4.4");

        // what publish packs has the values and no markers
        let packed = toml::to_string(&lobby).unwrap();
        assert!(!packed.contains("workspace = true"), "{packed}");
        assert!(packed.contains("version = \"2.1.0\""), "{packed}");

        // the root inherits from its own table, and opening it loads every member
        let opened = workspace::containing(&base.join("places/lobby"))
            .unwrap()
            .unwrap();
        assert_eq!(opened.members.len(), 2);
        let root = Manifest::load_from(&base.join("ember.toml")).unwrap();
        assert_eq!(root.package.unwrap().version, "2.1.0");

        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn a_marker_with_nothing_to_inherit_is_an_error() {
        let base = std::env::temp_dir().join("embr-test-inherit-errors");
        let lobby = base.join("places/lobby/ember.toml");

        write_workspace(&base, "[dependencies]\nmissing = { workspace = true }\n");
        assert!(matches!(
            Manifest::load_from(&lobby),
            Err(Error::InheritMissing { field, from })
                if field == "dependencies.missing" && from == "workspace.dependencies.missing"
        ));

        // only a dependency can add keys, and only target and entry
        write_workspace(
            &base,
            "[dependencies]\nsignal = { workspace = true, version = \"^2\" }\n",
        );
        assert!(matches!(
            Manifest::load_from(&lobby),
            Err(Error::InheritExtraKey { field, key })
                if field == "dependencies.signal" && key == "version"
        ));

        // a folder that no root lists has nothing to inherit from
        let outside = std::env::temp_dir().join("embr-test-inherit-outside");
        let _ = fs::remove_dir_all(&outside);
        write(
            &outside,
            "[package]\nname = \"acme/x\"\nversion.workspace = true\n",
        );
        assert!(matches!(
            Manifest::load_from(&outside.join("ember.toml")),
            Err(Error::InheritOutsideWorkspace(field)) if field == "package.version"
        ));

        let _ = fs::remove_dir_all(&base);
        let _ = fs::remove_dir_all(&outside);
    }
}
