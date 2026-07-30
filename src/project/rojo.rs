/*! the Rojo project files a vendored package ships.

a `default.project.json` inside an installed package is a *nested project*.
Rojo mounts that folder using the file's own `name` and tree instead of the
folder's disk layout. both diverge from what our link files require. take
evaera/promise, whose project file is

```json
{ "name": "promise", "tree": { "$path": "lib" } }
```

extracted to `packages/shared/.lpm/evaera_promise/`, that mounts as a single
ModuleScript named `promise` while the generated wrapper requires
`./.lpm/evaera_promise/lib`. the folder is renamed and the `lib` level is
gone, so luau-lsp and Studio can't resolve the require, or any type
re-exported through it, even though darklua maps the require by file path
at runtime and works fine.

so after extraction each project file is renamed to its folder and its mount
re-nested under the same names the require path spells, keeping the tree
keys and the require in lockstep. both come from `normalize_entry`.
packages that ship no project file, every lpm-native one, already sync
that way and are left alone.

only what the project file itself mounts is covered. a package that ships
one *and* has dependencies of its own still won't have the `packages/`
folder install writes into it mounted anywhere. */

use crate::project::package::normalize_entry;
use serde_json::{Map, Value};
use std::fs;
use std::path::Path;

pub const PROJECT_FILE: &str = "default.project.json";

/** rewrites every project file inside an extracted package so its mounted
tree mirrors the disk. nested ones count too, evaera/promise ships one under
modules/testez, and any of them can rename a subtree the package requires
into itself.

best-effort by design. a vendored file we can't read or don't understand is
left as it came, exactly like the manifest readers in this module, rather
than failing an install that has already downloaded everything. */
pub fn mirror_disk_layout(package_dir: &Path, warn: &mut impl FnMut(String)) {
    let project = package_dir.join(PROJECT_FILE);
    // an unreadable or non-utf8 project file isn't ours to touch
    if let Ok(text) = fs::read_to_string(&project)
        && let Some(rewritten) = mirrored(&text, package_dir)
        && let Err(error) = fs::write(&project, rewritten)
    {
        warn(format!(
            "warning: could not update {} ({error}); Rojo will mount this package under its own name",
            project.display()
        ));
    }

    let Ok(entries) = fs::read_dir(package_dir) else {
        return;
    };
    for entry in entries.flatten() {
        // file_type doesn't follow symlinks, so a linked cycle can't recurse
        if entry.file_type().is_ok_and(|kind| kind.is_dir()) {
            mirror_disk_layout(&entry.path(), warn);
        }
    }
}

/** the rewritten project text, or None when the file already mirrors the
disk or isn't a project we understand. `dir` is the folder the file sits
in, its name is what Rojo must call the instance. */
fn mirrored(text: &str, dir: &Path) -> Option<String> {
    let mut project: Map<String, Value> = serde_json::from_str(text).ok()?;

    // the folder name is what a require path spells, so the instance must match
    let folder = dir.file_name()?.to_str()?;
    let renamed = project.get("name").and_then(Value::as_str) != Some(folder);

    // treeless files aren't mountable projects, for Rojo or for us
    let tree = project.get("tree")?.as_object()?;
    let remounted = tree
        .get("$path")
        .and_then(Value::as_str)
        .and_then(|path| renested(tree, path));

    if !renamed && remounted.is_none() {
        return None;
    }
    project.insert("name".to_string(), Value::String(folder.to_string()));
    if let Some(tree) = remounted {
        project.insert("tree".to_string(), tree);
    }

    let mut rewritten = serde_json::to_string_pretty(&Value::Object(project)).ok()?;
    rewritten.push('\n');
    Some(rewritten)
}

/** the tree a root `$path` becomes once re-nested, folders named after the
require path, with the original mount at the bottom.

`lib` -> `{"$className": "Folder", "lib": {"$path": "lib"}}`, and
`src/init.luau` -> `{"$className": "Folder", "src": {"$path": "src/init.luau"}}`.
the keys are what `normalize_entry`, and so the link file, says, which is
why an init file mounts as the folder around it rather than a level deeper.

None when the mount already mirrors the disk, or when re-nesting it would
mean inventing instance names. paths reaching outside the package are left
for Rojo to complain about instead of being quietly rewritten. */
fn renested(tree: &Map<String, Value>, path: &str) -> Option<Value> {
    let cleaned = path.replace('\\', "/");
    let cleaned = cleaned.trim_start_matches("./").trim_end_matches('/');
    if Path::new(cleaned).is_absolute()
        || cleaned
            .split('/')
            .any(|component| matches!(component, "" | "." | ".."))
    {
        return None;
    }

    /* an entry of "" is the package root's own init file, it already mounts
    as the folder's instance, so the name fix is the whole job */
    let entry = normalize_entry(cleaned);
    if entry.is_empty() {
        return None;
    }
    let components: Vec<&str> = entry.split('/').collect();

    // a child already using that name would be overwritten, leave it alone
    let (first, rest) = components.split_first()?;
    if tree.contains_key(*first) {
        return None;
    }

    /* the mount, plus whatever described it. $properties and friends belong
    to the instance being mounted, not to the folders now above it */
    let mut mount = Map::new();
    mount.insert("$path".to_string(), Value::String(path.to_string()));
    for (key, value) in tree {
        if key.starts_with('$') && key != "$path" && key != "$className" {
            mount.insert(key.clone(), value.clone());
        }
    }

    let mut node = Value::Object(mount);
    for component in rest.iter().rev() {
        node = serde_json::json!({ "$className": "Folder", *component: node });
    }

    // the new root is a plain folder, keeping any children the tree named
    let mut root = Map::new();
    root.insert(
        "$className".to_string(),
        Value::String("Folder".to_string()),
    );
    for (key, value) in tree {
        if !key.starts_with('$') {
            root.insert(key.clone(), value.clone());
        }
    }
    root.insert(first.to_string(), node);
    Some(Value::Object(root))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dir(name: &str) -> std::path::PathBuf {
        std::path::PathBuf::from(format!("/packages/shared/.lpm/{name}"))
    }

    fn rewrite(text: &str, folder: &str) -> Value {
        serde_json::from_str(&mirrored(text, &dir(folder)).unwrap()).unwrap()
    }

    #[test]
    fn renests_a_root_path_under_its_own_name() {
        // evaera/promise, verbatim
        let project = rewrite(
            "{\n  \"name\": \"promise\",\n  \"tree\": {\n    \"$path\": \"lib\"\n  }\n}",
            "evaera_promise",
        );
        assert_eq!(project["name"], "evaera_promise");
        assert_eq!(project["tree"]["$className"], "Folder");
        assert_eq!(project["tree"]["lib"]["$path"], "lib");
    }

    #[test]
    fn nests_a_folder_per_path_component() {
        let project = rewrite(
            r#"{"name": "x", "tree": {"$path": "./out/lpm"}}"#,
            "acme_pkg",
        );
        assert_eq!(project["tree"]["out"]["$className"], "Folder");
        assert_eq!(project["tree"]["out"]["lpm"]["$path"], "./out/lpm");
    }

    #[test]
    fn mounts_a_file_path_where_the_require_expects_it() {
        /* the tree keys come from the same normalize_entry the link file
        uses. an init file *is* the folder around it, and a plain module is
        named after its stem. one folder level deeper would break the
        require just like the bug being fixed */
        let init = rewrite(
            r#"{"name": "x", "tree": {"$path": "src/init.luau"}}"#,
            "acme_pkg",
        );
        assert_eq!(init["tree"]["src"]["$path"], "src/init.luau");
        assert!(init["tree"]["src"]["init.luau"].is_null());

        let module = rewrite(
            r#"{"name": "x", "tree": {"$path": "Maid.lua"}}"#,
            "acme_maid",
        );
        assert_eq!(module["tree"]["Maid"]["$path"], "Maid.lua");
    }

    #[test]
    fn keeps_what_the_tree_declared() {
        let project = rewrite(
            r#"{"name": "x", "servePort": 1234, "tree": {"$path": "src",
                "$ignoreUnknownInstances": true, "Vendor": {"$path": "third_party"}}}"#,
            "acme_pkg",
        );

        // top-level settings and named children stay put...
        assert_eq!(project["servePort"], 1234);
        assert_eq!(project["tree"]["Vendor"]["$path"], "third_party");
        // ...and directives follow the instance they described
        assert_eq!(project["tree"]["src"]["$ignoreUnknownInstances"], true);
        assert_eq!(project["tree"]["src"]["$path"], "src");
    }

    #[test]
    fn renames_without_touching_a_structured_tree() {
        /* a tree that names its own children already mirrors whatever it
        mounts. only the instance name can be wrong */
        let project = rewrite(
            r#"{"name": "wrong", "tree": {"$className": "Folder", "src": {"$path": "src"}}}"#,
            "acme_pkg",
        );
        assert_eq!(project["name"], "acme_pkg");
        assert_eq!(project["tree"]["src"]["$path"], "src");
    }

    #[test]
    fn leaves_alone_what_it_cannot_safely_renest() {
        for text in [
            // already mirrors the disk, or is the folder's own init file
            r#"{"name": "acme_pkg", "tree": {"$className": "Folder", "src": {"$path": "src"}}}"#,
            r#"{"name": "acme_pkg", "tree": {"$path": "."}}"#,
            r#"{"name": "acme_pkg", "tree": {"$path": "init.luau"}}"#,
            // reaches outside the package, not ours to reinterpret
            r#"{"name": "acme_pkg", "tree": {"$path": "/elsewhere/src"}}"#,
            r#"{"name": "acme_pkg", "tree": {"$path": "../sibling"}}"#,
            r#"{"name": "acme_pkg", "tree": {"$path": "src//lib"}}"#,
            // the name it would need is already taken
            r#"{"name": "acme_pkg", "tree": {"$path": "src", "src": {"$path": "other"}}}"#,
            // not projects we understand
            "not json",
            r#"{"name": "acme_pkg"}"#,
            "[1, 2]",
        ] {
            assert_eq!(mirrored(text, &dir("acme_pkg")), None, "for {text}");
        }
    }

    #[test]
    fn rewriting_twice_changes_nothing() {
        // link generation re-reads these files, so the shape has to settle
        let once = mirrored(
            r#"{"name": "promise", "tree": {"$path": "lib"}}"#,
            &dir("p"),
        )
        .unwrap();
        assert_eq!(mirrored(&once, &dir("p")), None);
    }

    #[test]
    fn rewrites_every_project_file_in_the_package() {
        let base = std::env::temp_dir().join("lpm-test-rojo-mirror");
        let _ = fs::remove_dir_all(&base);
        let package = base.join("evaera_promise");
        let nested = package.join("modules/testez");
        fs::create_dir_all(&nested).unwrap();
        fs::write(
            package.join(PROJECT_FILE),
            r#"{"name": "promise", "tree": {"$path": "lib"}}"#,
        )
        .unwrap();
        // promise really does ship a second one, naming itself "promise" too
        fs::write(
            nested.join(PROJECT_FILE),
            r#"{"name": "promise", "tree": {"$path": "src"}}"#,
        )
        .unwrap();
        // a package file that only looks like a project stays untouched
        fs::write(package.join("other.project.json"), "{}").unwrap();

        let mut warnings = Vec::new();
        mirror_disk_layout(&package, &mut |message| warnings.push(message));
        assert_eq!(warnings, Vec::<String>::new());

        let read = |path: &Path| -> Value {
            serde_json::from_str(&fs::read_to_string(path).unwrap()).unwrap()
        };
        let root = read(&package.join(PROJECT_FILE));
        assert_eq!(root["name"], "evaera_promise");
        assert_eq!(root["tree"]["lib"]["$path"], "lib");

        let inner = read(&nested.join(PROJECT_FILE));
        assert_eq!(inner["name"], "testez");
        assert_eq!(inner["tree"]["src"]["$path"], "src");

        assert_eq!(
            fs::read_to_string(package.join("other.project.json")).unwrap(),
            "{}"
        );

        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn a_package_without_project_files_is_untouched() {
        let base = std::env::temp_dir().join("lpm-test-rojo-none");
        let _ = fs::remove_dir_all(&base);
        fs::create_dir_all(base.join("src")).unwrap();
        fs::write(base.join("src/init.luau"), "return {}").unwrap();

        let mut warnings = Vec::new();
        mirror_disk_layout(&base, &mut |message| warnings.push(message));

        assert_eq!(warnings, Vec::<String>::new());
        assert!(!base.join(PROJECT_FILE).exists());
        assert_eq!(
            fs::read_to_string(base.join("src/init.luau")).unwrap(),
            "return {}"
        );

        let _ = fs::remove_dir_all(&base);
    }
}
