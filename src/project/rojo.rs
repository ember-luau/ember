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

that pass only covers what the project file already mounts. the `packages/`
folder install writes a package's own nested links into doesn't exist yet,
every package has to be extracted before any of them can be linked, so
`mount_nested_packages` comes back for it once the links are on disk. */

use crate::project::package::normalize_entry;
use serde_json::{Map, Value};
use std::fs;
use std::path::Path;

pub const PROJECT_FILE: &str = "default.project.json";

/// the folder a package's own nested links live in, the published default.
pub const PACKAGES_DIR: &str = "packages";

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

/** mounts the `packages/` folder holding a package's nested links, for a
package that ships its own project file.

such a file replaces the folder's disk layout with its tree, and a tree
written before install existed names no `packages`. Rojo then syncs the
package without it, so the `./packages/<env>/<alias>` requires the install
rewrote resolve to nothing in Studio or luau-lsp, even though darklua maps
them by file path and they work at runtime. packages shipping no project
file mount from disk and already carry the folder.

run per package after the nested-link pass, the earliest the folder is
really there, and only for a package that got links, so nothing mounts a
`$path` Rojo would reject as missing.

the folder goes at the tree root, where the require rewrite spells it, so
this covers the roots `mirror_disk_layout` re-nested and the ones it left
mounting a plain relative path. a root it refused, reaching outside the
package, is refused here too rather than being handed a child that its
requires don't climb to anyway. best-effort like the rest of this module. */
pub fn mount_nested_packages(package_dir: &Path, warn: &mut impl FnMut(String)) {
    /* the caller only gets here having written links, this catches the
    package that ships a `packages` folder it never linked into */
    if !package_dir.join(PACKAGES_DIR).is_dir() {
        return;
    }
    let project = package_dir.join(PROJECT_FILE);
    // no project file means Rojo mounts the disk, this folder included
    if let Ok(text) = fs::read_to_string(&project)
        && let Some(rewritten) = with_packages_mounted(&text, package_dir)
        && let Err(error) = fs::write(&project, rewritten)
    {
        warn(format!(
            "warning: could not update {} ({error}); Rojo will not sync the nested links in {}",
            project.display(),
            package_dir.display()
        ));
    }
}

/** the project text with `packages` added to its tree, or None when the
tree already syncs the folder or isn't one the folder belongs in. `dir` is
the folder the file sits in, what a relative `$path` is measured from. */
fn with_packages_mounted(text: &str, dir: &Path) -> Option<String> {
    let mut project: Map<String, Value> = serde_json::from_str(text).ok()?;
    let tree = project.get("tree")?.as_object()?;

    // a child of that name is the package's own, and inserting would clobber it
    if tree.contains_key(PACKAGES_DIR) {
        return None;
    }
    /* a differently cased one collides only where the filesystem folded it
    into the folder the links were written through, so ask the disk rather
    than the platform: on a case sensitive one `Packages` is a second folder
    and mounting ours beside it is exactly right */
    if tree
        .keys()
        .filter(|key| key.eq_ignore_ascii_case(PACKAGES_DIR))
        .any(|key| is_our_links_folder(dir, key))
    {
        return None;
    }
    /* only a plain folder takes the added child. a place-style project, a
    DataModel of services say, doesn't mount the package as one instance, so
    a `packages` folder hung off its root would sit where no require looks */
    match tree.get("$className").and_then(Value::as_str) {
        None | Some("Folder") => {}
        Some(_) => return None,
    }
    if let Some(path) = root_path(tree) {
        let cleaned = path.replace('\\', "/");
        let cleaned = cleaned.trim_start_matches("./").trim_end_matches('/');
        /* a root mounting the package directory itself, or anything outside
        it, is not ours to reinterpret, the same stance `renested` takes.
        the first already syncs every child it has, this folder among them */
        if Path::new(cleaned).is_absolute()
            || cleaned
                .split('/')
                .any(|component| matches!(component, "" | "." | ".."))
        {
            return None;
        }
        /* mounting a directory brings that directory's children with it, so
        a `packages` of its own would collide with the one being added */
        let mounted = dir.join(cleaned);
        if mounted.is_dir() && mounted.join(PACKAGES_DIR).exists() {
            return None;
        }
    }

    let mut tree = tree.clone();
    tree.insert(
        PACKAGES_DIR.to_string(),
        serde_json::json!({ "$path": PACKAGES_DIR }),
    );
    project.insert("tree".to_string(), Value::Object(tree));

    let mut rewritten = serde_json::to_string_pretty(&Value::Object(project)).ok()?;
    rewritten.push('\n');
    Some(rewritten)
}

/** true when `name` reaches the same folder on disk as `packages` does, as
a case-insensitive filesystem makes `Packages` do. both sides are resolved
rather than compared as text, since only the filesystem knows. */
fn is_our_links_folder(dir: &Path, name: &str) -> bool {
    let Ok(ours) = fs::canonicalize(dir.join(PACKAGES_DIR)) else {
        return false;
    };
    fs::canonicalize(dir.join(name)).is_ok_and(|theirs| theirs == ours)
}

/** a node's `$path`, in either spelling. Rojo also accepts the object form
`{"optional": "src"}`, which reads as a path like any other here. */
fn root_path(tree: &Map<String, Value>) -> Option<&str> {
    let path = tree.get("$path")?;
    path.as_str()
        .or_else(|| path.get("optional").and_then(Value::as_str))
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

    /// a package dir holding `files`, plus a linked dependency when `linked`.
    fn package_with(name: &str, files: &[(&str, &str)], linked: bool) -> std::path::PathBuf {
        let base = std::env::temp_dir().join(format!("lpm-test-rojo-{name}"));
        let _ = fs::remove_dir_all(&base);
        fs::create_dir_all(&base).unwrap();
        for (path, contents) in files {
            let file = base.join(path);
            fs::create_dir_all(file.parent().unwrap()).unwrap();
            fs::write(file, contents).unwrap();
        }
        if linked {
            let links = base.join("packages/shared");
            fs::create_dir_all(&links).unwrap();
            fs::write(links.join("Promise.luau"), "return nil\n").unwrap();
        }
        base
    }

    #[test]
    fn mounts_the_folder_a_shipped_project_file_never_named() {
        /* lyra shaped: the tree mirror_disk_layout left mounts only `lib`,
        so the links the nested pass then wrote synced nowhere */
        let package = package_with(
            "mount",
            &[(
                PROJECT_FILE,
                r#"{"name": "lyra_lyra", "tree": {"$className": "Folder", "lib": {"$path": "lib"}}}"#,
            )],
            true,
        );

        let mut warnings = Vec::new();
        mount_nested_packages(&package, &mut |message| warnings.push(message));
        assert_eq!(warnings, Vec::<String>::new());

        let project: Value =
            serde_json::from_str(&fs::read_to_string(package.join(PROJECT_FILE)).unwrap()).unwrap();
        assert_eq!(project["tree"]["packages"]["$path"], "packages");
        // and nothing the file already said moved
        assert_eq!(project["name"], "lyra_lyra");
        assert_eq!(project["tree"]["lib"]["$path"], "lib");

        // the shape has to settle, install re-runs over a warm tree
        let once = fs::read_to_string(package.join(PROJECT_FILE)).unwrap();
        assert_eq!(with_packages_mounted(&once, &package), None);

        let _ = fs::remove_dir_all(&package);
    }

    #[test]
    fn leaves_alone_a_tree_the_folder_does_not_belong_in() {
        let dir = std::path::Path::new("/nonexistent");
        for text in [
            // already names it, whatever it mounts there, and we'd clobber it
            r#"{"tree": {"$className": "Folder", "packages": {"$path": "vendor"}}}"#,
            /* a place-style project mounts no single instance, so a root
            `packages` child is not where any require looks */
            r#"{"tree": {"$className": "DataModel", "ServerScriptService": {"$path": "src"}}}"#,
            /* roots reaching outside the package, refused here exactly as
            `renested` refuses them, in both `$path` spellings */
            r#"{"tree": {"$path": "../sibling"}}"#,
            r#"{"tree": {"$path": "/elsewhere/src"}}"#,
            r#"{"tree": {"$path": {"optional": ".."}}}"#,
            // not projects we understand
            "not json",
            r#"{"name": "acme_pkg"}"#,
        ] {
            assert_eq!(with_packages_mounted(text, dir), None, "for {text}");
        }

        /* a root mounting the package itself, which already syncs every
        child it has, this folder among them. refused for naming `.` at all,
        before any of it is resolved, in each of Rojo's three spellings */
        for text in [
            r#"{"name": "acme_pkg", "tree": {"$path": "."}}"#,
            r#"{"name": "acme_pkg", "tree": {"$path": "./"}}"#,
            r#"{"name": "acme_pkg", "tree": {"$path": {"optional": "."}}}"#,
        ] {
            assert_eq!(with_packages_mounted(text, dir), None, "for {text}");
        }

        /* what the resolving half is really for: a root mounting a
        subfolder that carries a `packages` of its own, which the mount
        would collide with once Rojo brought it up */
        let vendored = package_with(
            "mount-vendored",
            &[("src/packages/keep.luau", "return nil\n")],
            true,
        );
        assert_eq!(
            with_packages_mounted(r#"{"tree": {"$path": "src"}}"#, &vendored),
            None
        );

        let _ = fs::remove_dir_all(&vendored);
    }

    #[test]
    fn a_differently_cased_child_collides_only_when_it_is_the_same_folder() {
        /* macOS and Windows fold `Packages` into the folder the links were
        written through, so it is already synced. Linux keeps two real
        folders and ours still has to be mounted. the disk decides */
        let package = package_with("mount-cased", &[], true);
        let mounted = with_packages_mounted(
            r#"{"tree": {"$className": "Folder", "Packages": {"$path": "Packages"}}}"#,
            &package,
        );

        if package.join("Packages").is_dir() {
            assert_eq!(mounted, None, "a folded name is our own folder");
        } else {
            let project: Value = serde_json::from_str(&mounted.unwrap()).unwrap();
            assert_eq!(project["tree"]["packages"]["$path"], "packages");
            // and the one the package shipped is still there beside it
            assert_eq!(project["tree"]["Packages"]["$path"], "Packages");
        }

        let _ = fs::remove_dir_all(&package);
    }

    #[test]
    fn mounts_beside_a_root_path_that_carries_no_packages_child() {
        /* a root init mounts as the package's own instance and the require
        rewrite spells `@self/packages/...`, a child of exactly that */
        let package = package_with(
            "mount-init",
            &[
                (PROJECT_FILE, r#"{"tree": {"$path": "init.luau"}}"#),
                ("init.luau", "return {}\n"),
            ],
            true,
        );
        let text = fs::read_to_string(package.join(PROJECT_FILE)).unwrap();
        let project: Value =
            serde_json::from_str(&with_packages_mounted(&text, &package).unwrap()).unwrap();
        assert_eq!(project["tree"]["$path"], "init.luau");
        assert_eq!(project["tree"]["packages"]["$path"], "packages");
        let _ = fs::remove_dir_all(&package);
    }

    #[test]
    fn a_package_that_linked_nothing_keeps_its_project_file() {
        /* the guard that matters: mounting a `packages` path that isn't
        there would make Rojo reject the whole project */
        let project = r#"{"name": "acme_pkg", "tree": {"$path": "src"}}"#;
        let package = package_with("mount-unlinked", &[(PROJECT_FILE, project)], false);

        let mut warnings = Vec::new();
        mount_nested_packages(&package, &mut |message| warnings.push(message));

        assert_eq!(warnings, Vec::<String>::new());
        assert_eq!(
            fs::read_to_string(package.join(PROJECT_FILE)).unwrap(),
            project
        );
        let _ = fs::remove_dir_all(&package);
    }

    #[test]
    fn links_without_a_project_file_need_no_mount() {
        // these mount from disk already, every lpm-native package
        let package = package_with("mount-native", &[("init.luau", "return {}\n")], true);

        let mut warnings = Vec::new();
        mount_nested_packages(&package, &mut |message| warnings.push(message));

        assert_eq!(warnings, Vec::<String>::new());
        assert!(!package.join(PROJECT_FILE).exists());
        let _ = fs::remove_dir_all(&package);
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
