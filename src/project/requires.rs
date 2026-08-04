/*! turns roblox instance requires in wally packages into string requires.
wally-era code says require(script.Parent.Foo), but there's no instance tree
in our layout, so that has to become a "./Foo" style path. anything we can't
map safely just stays as it was. */

use crate::error::Error;
use crate::project::rojo::PACKAGES_DIR;
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

/** rewrites every mappable `require(script...)` chain under the package's
entry module. `entry` is the normalized entry point, "" = root init, "src" =
dir module, "lib" = single file. returns how many requires got rewritten.

how the mapping works.
- paths come out instance-relative, string-require style. "./" is siblings,
  "@self/" is children. an init file IS its folder, so its whole frame sits
  one level above a plain file's
- climbing exactly one level past the module root means a wally dependency
  alias. those wait for `rewrite_escape_requires`, this pass can't know
  which environment folder each alias's nested link lands in
- anything weirder, children of a plain file module, climbing further, non
  literal segments, gets skipped */
pub fn rewrite_instance_requires(package_dir: &Path, entry: &str) -> Result<usize, Error> {
    rewrite_requires(package_dir, entry, None)
}

/** the second half, run from the nested-link phase once every dependency's
environment is known. maps chains that climb exactly one level past the
module root, wally's alias zone, onto the package's OWN nested links,
`packages/<env>/<alias>`. `aliases` is alias -> environment folder name.
aliases absent from it stay untouched, like any other unmappable chain. */
pub fn rewrite_escape_requires(
    package_dir: &Path,
    entry: &str,
    aliases: &BTreeMap<String, String>,
) -> Result<usize, Error> {
    rewrite_requires(package_dir, entry, Some(aliases))
}

/** what the `roblox` environment's folder was called before the rename,
see [`Environment::Roblox`]. a package published back then spells it in
its own source, and no republish will ever reach the versions already
out. */
const LEGACY_ROBLOX_DIR: &str = "shared";

/** retargets string requires that still spell a dependency's nested link
under the pre-rename folder name.

a package's own dependencies are linked at `packages/<env>/<alias>`, so a
package published before `shared` became `roblox` has
`require('../packages/shared/core')` compiled into it while lpm now
writes `packages/roblox/core.luau`. chief/lifecycles, chief/traits and
chief/dependencies are all in that state today; rewriting the stored copy
fixes every such package without anyone republishing anything, and does
nothing at all to a package published since.

`aliases` is what the nested-link pass just linked, alias -> environment
folder, so only a dependency lpm itself put in the roblox folder can be
retargeted -- a package vendoring its own `packages/shared` directory is
left alone. only the path inside a `require(...)` is considered, so the
same words in a comment stay as they are. */
pub fn rewrite_legacy_environment_requires(
    package_dir: &Path,
    entry: &str,
    aliases: &BTreeMap<String, String>,
) -> Result<usize, Error> {
    let replacements: Vec<(String, String)> = aliases
        .iter()
        .filter(|(_, environment)| environment.as_str() != LEGACY_ROBLOX_DIR)
        .map(|(alias, environment)| {
            (
                format!("{PACKAGES_DIR}/{LEGACY_ROBLOX_DIR}/{alias}"),
                format!("{PACKAGES_DIR}/{environment}/{alias}"),
            )
        })
        .collect();
    if replacements.is_empty() {
        return Ok(0);
    }

    let (_, files) = module_tree(package_dir, entry)?;
    let mut rewritten = 0;
    for file in files {
        let Ok(source) = fs::read_to_string(&file) else {
            continue; // binary or non-utf8, not ours to touch
        };
        if let Some((updated, count)) = retarget_string_requires(&source, &replacements) {
            fs::write(&file, updated)?;
            rewritten += count;
        }
    }
    Ok(rewritten)
}

/** rewrites the path inside every string require that names one of
`replacements`. None = nothing matched, and that path allocates nothing,
which matters because this runs over every file of every package. */
fn retarget_string_requires(
    source: &str,
    replacements: &[(String, String)],
) -> Option<(String, usize)> {
    // the overwhelming majority of files mention neither
    if !source.contains(LEGACY_ROBLOX_DIR) || !source.contains("require") {
        return None;
    }

    let bytes = source.as_bytes();
    let mut spliced: Vec<(usize, usize, String)> = Vec::new();
    let mut position = 0;
    while position < bytes.len() {
        /* strings and comments are skipped wholesale, exactly as the
        instance-require scan does, so only a real require argument is
        ever reached */
        match bytes[position] {
            b'-' if bytes.get(position + 1) == Some(&b'-') => {
                position = skip_comment(bytes, position);
            }
            b'"' | b'\'' => position = skip_short_string(bytes, position),
            b'[' if long_bracket_level(bytes, position).is_some() => {
                position = skip_long_string(bytes, position);
            }
            _ => {
                if at_word(bytes, position, b"require")
                    && let Some((start, end)) = string_argument(bytes, position + "require".len())
                {
                    let path = &source[start..end];
                    if let Some((from, to)) = replacements
                        .iter()
                        .find(|(from, _)| path.contains(from.as_str()))
                    {
                        spliced.push((start, end, path.replace(from, to)));
                    }
                    position = end;
                    continue;
                }
                // step whole identifiers so "myrequire" can't half-match
                if is_ident_byte(bytes[position]) {
                    while position < bytes.len() && is_ident_byte(bytes[position]) {
                        position += 1;
                    }
                } else {
                    position += 1;
                }
            }
        }
    }
    if spliced.is_empty() {
        return None;
    }

    let mut output = String::with_capacity(source.len());
    let mut cursor = 0;
    for (start, end, path) in &spliced {
        output.push_str(&source[cursor..*start]);
        output.push_str(path);
        cursor = *end;
    }
    output.push_str(&source[cursor..]);
    Some((output, spliced.len()))
}

/** the byte range of the string literal a require was called with, from
just past the `require` word. None for any other argument shape, and for
a literal carrying escapes, whose bytes are not ours to reinterpret. */
fn string_argument(bytes: &[u8], mut position: usize) -> Option<(usize, usize)> {
    position = skip_ws(bytes, position);
    if bytes.get(position) != Some(&b'(') {
        return None;
    }
    position = skip_ws(bytes, position + 1);
    let quote = *bytes.get(position)?;
    if quote != b'"' && quote != b'\'' {
        return None;
    }
    let start = position + 1;
    let mut end = start;
    while end < bytes.len() && bytes[end] != quote {
        if bytes[end] == b'\\' || bytes[end] == b'\n' {
            return None;
        }
        end += 1;
    }
    (end < bytes.len()).then_some((start, end))
}

/** the module root and the files under it, the ones a rewrite may touch.
files outside the mounted tree have no instance position and are not
ours. */
fn module_tree(package_dir: &Path, entry: &str) -> Result<(PathBuf, Vec<PathBuf>), Error> {
    let (module_root, single_file) = if entry.is_empty() {
        (PathBuf::new(), None)
    } else if package_dir.join(format!("{entry}.luau")).is_file()
        || package_dir.join(format!("{entry}.lua")).is_file()
    {
        let entry_path = PathBuf::from(entry);
        (
            entry_path
                .parent()
                .map(Path::to_path_buf)
                .unwrap_or_default(),
            Some(entry_path),
        )
    } else {
        (PathBuf::from(entry), None)
    };

    let files: Vec<PathBuf> = match &single_file {
        // a lone file module mounts by itself, its siblings aren't in the tree
        Some(entry_path) => ["luau", "lua"]
            .iter()
            .map(|ext| package_dir.join(entry_path).with_extension(ext))
            .filter(|path| path.is_file())
            .take(1)
            .collect(),
        None => luau_files(&package_dir.join(&module_root))?,
    };
    Ok((module_root, files))
}

fn rewrite_requires(
    package_dir: &Path,
    entry: &str,
    aliases: Option<&BTreeMap<String, String>>,
) -> Result<usize, Error> {
    let (module_root, files) = module_tree(package_dir, entry)?;

    let mut rewritten = 0;
    for file in files {
        let source = match fs::read_to_string(&file) {
            Ok(source) => source,
            Err(_) => continue, // binary or non-utf8, not ours to touch
        };

        let file_dir = file.parent().unwrap_or(package_dir);
        let dir_in_module = file_dir
            .strip_prefix(package_dir.join(&module_root))
            .unwrap_or(Path::new(""))
            .components()
            .count();
        let dir_in_package = file_dir
            .strip_prefix(package_dir)
            .unwrap_or(Path::new(""))
            .components()
            .count();
        let is_init = matches!(
            file.file_name().and_then(|name| name.to_str()),
            Some("init.luau" | "init.lua")
        );

        let context = FileContext {
            is_init,
            depth_in_module: dir_in_module,
            depth_in_package: dir_in_package,
            aliases,
        };
        if let Some((updated, count)) = rewrite_source(&source, &context) {
            fs::write(&file, updated)?;
            rewritten += count;
        }
    }
    Ok(rewritten)
}

struct FileContext<'a> {
    is_init: bool,
    /// how many dirs the file's folder sits below the module root.
    depth_in_module: usize,
    /// same but below the package folder, escape targets climb this far up.
    depth_in_package: usize,
    /// alias -> environment folder for escape requires. None leaves them alone.
    aliases: Option<&'a BTreeMap<String, String>>,
}

/// every .luau/.lua file under `root`, recursively.
fn luau_files(root: &Path) -> Result<Vec<PathBuf>, Error> {
    let mut files = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries {
            let entry = entry?;
            let path = entry.path();
            if entry.file_type()?.is_dir() {
                stack.push(path);
            } else if matches!(
                path.extension().and_then(|ext| ext.to_str()),
                Some("luau" | "lua")
            ) {
                files.push(path);
            }
        }
    }
    Ok(files)
}

/** rewrites all mappable chains in one file. None = nothing to change, and
that path allocates nothing. the scan only records ranges, the new string
only gets built when a chain actually matched. matters because this runs
over every file of every installed package. */
fn rewrite_source(source: &str, context: &FileContext) -> Option<(String, usize)> {
    // most files have no requires at all, bail before even scanning
    if !source.contains("require") {
        return None;
    }

    let chains = find_chains(source, context);
    if chains.is_empty() {
        return None;
    }

    // one splice pass over the recorded ranges
    let mut output = String::with_capacity(source.len());
    let mut cursor = 0;
    for (start, end, path) in &chains {
        output.push_str(&source[cursor..*start]);
        output.push_str("require(\"");
        output.push_str(path);
        output.push_str("\")");
        cursor = *end;
    }
    output.push_str(&source[cursor..]);
    Some((output, chains.len()))
}

/// scan pass, finds every mappable chain as (start, end, replacement path).
fn find_chains(source: &str, context: &FileContext) -> Vec<(usize, usize, String)> {
    let bytes = source.as_bytes();
    let mut found = Vec::new();
    let mut position = 0;

    while position < bytes.len() {
        /* stay out of strings and comments so a require inside either is
        never touched */
        match bytes[position] {
            b'-' if bytes.get(position + 1) == Some(&b'-') => {
                position = skip_comment(bytes, position);
            }
            b'"' | b'\'' => {
                position = skip_short_string(bytes, position);
            }
            b'[' if long_bracket_level(bytes, position).is_some() => {
                position = skip_long_string(bytes, position);
            }
            _ => {
                if at_word(bytes, position, b"require")
                    && let Some((end, path)) =
                        parse_chain(source, position + "require".len(), context)
                {
                    found.push((position, end, path));
                    position = end;
                    continue;
                }
                // step whole identifiers so "myrequire" can't half-match
                if is_ident_byte(bytes[position]) {
                    while position < bytes.len() && is_ident_byte(bytes[position]) {
                        position += 1;
                    }
                } else {
                    position += 1;
                }
            }
        }
    }
    found
}

/** parses `(script.A.B)` style chains starting right after the require word.
returns (end offset just past the closing paren, replacement path) or None
when the chain isn't one we can map. */
fn parse_chain(
    source: &str,
    mut position: usize,
    context: &FileContext,
) -> Option<(usize, String)> {
    let bytes = source.as_bytes();
    position = skip_ws(bytes, position);
    if bytes.get(position) != Some(&b'(') {
        return None;
    }
    position = skip_ws(bytes, position + 1);
    if !at_word(bytes, position, b"script") {
        return None;
    }
    position = skip_ws(bytes, position + "script".len());

    /* walk the chain file-relative. `leaf` means we're at the file itself,
    a non-init module, `ups` counts how far above the file's dir we are */
    let mut leaf = !context.is_init;
    let mut ups = 0usize;
    let mut names: Vec<String> = Vec::new();

    loop {
        match bytes.get(position)? {
            b')' => {
                position += 1;
                break;
            }
            b'.' => {
                position = skip_ws(bytes, position + 1);
                let (end, name) = take_ident(source, position)?;
                position = skip_ws(bytes, end);
                if name == "Parent" {
                    if leaf {
                        leaf = false;
                    } else if !names.is_empty() {
                        names.pop();
                    } else {
                        ups += 1;
                    }
                } else if leaf {
                    return None; // children of a plain file module, no mapping
                } else {
                    names.push(name.to_string());
                }
            }
            b'[' => {
                position = skip_ws(bytes, position + 1);
                let (end, name) = take_string(source, position)?;
                position = skip_ws(bytes, end);
                if bytes.get(position) != Some(&b']') {
                    return None;
                }
                position = skip_ws(bytes, position + 1);
                if leaf {
                    return None;
                }
                names.push(name);
            }
            b':' => {
                position = skip_ws(bytes, position + 1);
                let (end, method) = take_ident(source, position)?;
                if method != "WaitForChild" && method != "FindFirstChild" {
                    return None;
                }
                position = skip_ws(bytes, end);
                if bytes.get(position) != Some(&b'(') {
                    return None;
                }
                position = skip_ws(bytes, position + 1);
                let (end, name) = take_string(source, position)?;
                position = skip_ws(bytes, end);
                if bytes.get(position) != Some(&b')') {
                    return None;
                }
                position = skip_ws(bytes, position + 1);
                if leaf {
                    return None;
                }
                names.push(name);
            }
            _ => return None,
        }
    }

    if leaf || (ups == 0 && names.is_empty()) {
        return None; // require(script) / require(script.Parent), nothing to point at
    }

    /* string requires resolve instance-relative. "./" is the module's
    siblings, "@self/" its children. an init file IS its folder, so its
    frame sits one level higher than a plain file's. children need @self,
    and every Parent hop renders with one less "../" */
    let init_shift = usize::from(context.is_init);
    let path = if ups <= context.depth_in_module {
        if ups == 0 && context.is_init {
            format!("@self/{}", names.join("/"))
        } else {
            let string_ups = ups - init_shift;
            let mut parts = vec![".."; string_ups];
            parts.extend(names.iter().map(String::as_str));
            if string_ups == 0 {
                format!("./{}", parts.join("/"))
            } else {
                parts.join("/")
            }
        }
    } else if ups == context.depth_in_module + 1 && names.len() == 1 {
        /* one level above the module root is wally's alias zone, the
        package's own nested link for that alias, packages/<env>/<alias>.
        the env folder is only known once dependencies are resolved, so
        pass one, aliases = None, leaves these chains for
        `rewrite_escape_requires` to come back for */
        let environment = context.aliases?.get(&names[0])?;
        if context.depth_in_package + 1 == init_shift {
            // a root init, the packages folder is among its own children
            format!("@self/{PACKAGES_DIR}/{environment}/{}", names[0])
        } else {
            let string_ups = context.depth_in_package - init_shift;
            let mut parts = vec![".."; string_ups];
            parts.push(PACKAGES_DIR);
            parts.push(environment);
            parts.push(names[0].as_str());
            if string_ups == 0 {
                format!("./{}", parts.join("/"))
            } else {
                parts.join("/")
            }
        }
    } else {
        return None; // climbing past the alias zone, nowhere to map that
    };

    Some((position, path))
}

fn is_ident_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || byte == b'_'
}

/// true when `word` sits at `position` with identifier boundaries on both sides.
fn at_word(bytes: &[u8], position: usize, word: &[u8]) -> bool {
    bytes[position..].starts_with(word)
        && (position == 0 || !is_ident_byte(bytes[position - 1]))
        && bytes
            .get(position + word.len())
            .is_none_or(|next| !is_ident_byte(*next))
}

fn skip_ws(bytes: &[u8], mut position: usize) -> usize {
    while position < bytes.len() && bytes[position].is_ascii_whitespace() {
        position += 1;
    }
    position
}

fn take_ident(source: &str, position: usize) -> Option<(usize, &str)> {
    let bytes = source.as_bytes();
    let mut end = position;
    while end < bytes.len() && is_ident_byte(bytes[end]) {
        end += 1;
    }
    if end == position || bytes[position].is_ascii_digit() {
        return None;
    }
    Some((end, &source[position..end]))
}

/// a quoted "name" / 'name'. escapes and empty names bail, not worth mapping.
fn take_string(source: &str, position: usize) -> Option<(usize, String)> {
    let bytes = source.as_bytes();
    let quote = *bytes.get(position)?;
    if quote != b'"' && quote != b'\'' {
        return None;
    }
    let mut end = position + 1;
    while end < bytes.len() && bytes[end] != quote {
        if bytes[end] == b'\\' {
            return None;
        }
        end += 1;
    }
    if end >= bytes.len() || end == position + 1 {
        return None;
    }
    Some((end + 1, source[position + 1..end].to_string()))
}

/// `--` line or `--[[ ]]` block comment starting at `position`. returns the end.
fn skip_comment(bytes: &[u8], position: usize) -> usize {
    let after = position + 2;
    if long_bracket_level(bytes, after).is_some() {
        return skip_long_string(bytes, after);
    }
    let mut end = after;
    while end < bytes.len() && bytes[end] != b'\n' {
        end += 1;
    }
    end
}

/// level of a long bracket `[`, `[=`... at `position`, if one opens there.
fn long_bracket_level(bytes: &[u8], position: usize) -> Option<usize> {
    if bytes.get(position) != Some(&b'[') {
        return None;
    }
    let mut level = 0;
    let mut current = position + 1;
    while bytes.get(current) == Some(&b'=') {
        level += 1;
        current += 1;
    }
    (bytes.get(current) == Some(&b'[')).then_some(level)
}

/// skips a whole `[[ ]]` / `[=[ ]=]` string or block comment body.
fn skip_long_string(bytes: &[u8], position: usize) -> usize {
    let Some(level) = long_bracket_level(bytes, position) else {
        return position + 1;
    };
    let mut current = position + level + 2;
    while current < bytes.len() {
        if bytes[current] == b']' {
            let mut end = current + 1;
            let mut count = 0;
            while bytes.get(end) == Some(&b'=') {
                count += 1;
                end += 1;
            }
            if count == level && bytes.get(end) == Some(&b']') {
                return end + 1;
            }
        }
        current += 1;
    }
    bytes.len()
}

fn skip_short_string(bytes: &[u8], position: usize) -> usize {
    let quote = bytes[position];
    let mut current = position + 1;
    while current < bytes.len() {
        match bytes[current] {
            b'\\' => current += 2,
            byte if byte == quote => return current + 1,
            b'\n' => return current, // unterminated, don't run away
            _ => current += 1,
        }
    }
    bytes.len()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dir_module(depth_in_module: usize, is_init: bool) -> FileContext<'static> {
        FileContext {
            is_init,
            depth_in_module,
            // package with module root "src", file dir depth is one more
            depth_in_package: depth_in_module + 1,
            aliases: None,
        }
    }

    #[test]
    fn init_files_require_children_through_self() {
        /* src/init.luau in a src module, satset's shape. an init file IS its
        folder, so children are @self and "./" would hit siblings instead */
        let context = dir_module(0, true);
        let (out, count) =
            rewrite_source("local Batcher = require(script.Core.Batcher)\n", &context).unwrap();
        assert_eq!(count, 1);
        assert_eq!(out, "local Batcher = require(\"@self/Core/Batcher\")\n");
    }

    #[test]
    fn init_parent_hops_render_one_level_shorter() {
        // in src/Sub/init.luau, script.Parent.X is src/X, a sibling, so "./"
        let context = dir_module(1, true);
        let (out, count) = rewrite_source(
            "local A = require(script.Parent.Util)\n\
             local B = require(script.Other)\n",
            &context,
        )
        .unwrap();
        assert_eq!(count, 2);
        assert_eq!(
            out,
            "local A = require(\"./Util\")\n\
             local B = require(\"@self/Other\")\n"
        );
    }

    #[test]
    fn siblings_and_uncles_resolve_relative() {
        // src/Serialization/Serializer.luau
        let context = dir_module(1, false);
        let (out, count) = rewrite_source(
            "local A = require(script.Parent.Sanitizer)\n\
             local B = require(script.Parent.Parent.Core.Batcher)\n",
            &context,
        )
        .unwrap();
        assert_eq!(count, 2);
        assert_eq!(
            out,
            "local A = require(\"./Sanitizer\")\n\
             local B = require(\"../Core/Batcher\")\n"
        );
    }

    #[test]
    fn escaping_the_module_root_lands_on_dependency_links() {
        /* src/init.luau doing require(script.Parent.Signal). wally would
        find the alias next to the package, ours is the package's OWN
        nested link, packages/<env>/Signal. only the nested-link phase
        knows <env>, so the first pass must leave the chain alone... */
        let context = dir_module(0, true);
        let source = "local Dep = require(script.Parent.Signal)\n";
        assert!(rewrite_source(source, &context).is_none());

        // ...and the second pass, armed with the alias map, retargets it
        let aliases: BTreeMap<String, String> =
            [("Signal".to_string(), "shared".to_string())].into();
        let mut context = dir_module(0, true);
        context.aliases = Some(&aliases);
        let (out, count) = rewrite_source(source, &context).unwrap();
        assert_eq!(count, 1);
        assert_eq!(out, "local Dep = require(\"./packages/shared/Signal\")\n");

        // same reach from a plain file next to that init needs the extra hop
        let mut context = dir_module(0, false);
        context.aliases = Some(&aliases);
        let (out, count) = rewrite_source(
            "local Dep = require(script.Parent.Parent.Signal)\n",
            &context,
        )
        .unwrap();
        assert_eq!(count, 1);
        assert_eq!(out, "local Dep = require(\"../packages/shared/Signal\")\n");

        // a root init's packages folder is among its own children, so @self
        let root_init = FileContext {
            is_init: true,
            depth_in_module: 0,
            depth_in_package: 0,
            aliases: Some(&aliases),
        };
        let (out, count) = rewrite_source(source, &root_init).unwrap();
        assert_eq!(count, 1);
        assert_eq!(
            out,
            "local Dep = require(\"@self/packages/shared/Signal\")\n"
        );

        // an alias the map doesn't know stays exactly as it was
        let mut context = dir_module(0, true);
        context.aliases = Some(&aliases);
        let (out, count) = rewrite_source(
            "require(script.Parent.Signal)\nrequire(script.Parent.Mystery)\n",
            &context,
        )
        .unwrap();
        assert_eq!(count, 1);
        assert!(out.contains("require(\"./packages/shared/Signal\")"));
        assert!(out.contains("require(script.Parent.Mystery)"));
    }

    /// alias -> environment folder, the map the nested-link pass hands over.
    fn linked(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        pairs
            .iter()
            .map(|(alias, environment)| (alias.to_string(), environment.to_string()))
            .collect()
    }

    /** chief/lifecycles@0.3.0, verbatim. published before the rename, so it
    requires its dependency through `packages/shared`, while lpm now links
    it under `packages/roblox` -- the require resolves to nothing until
    this pass retargets it. */
    #[test]
    fn retargets_a_require_left_on_the_pre_rename_folder() {
        let aliases = linked(&[("core", "roblox")]);
        let (out, count) = retarget_string_requires(
            "local Chief = require('../packages/shared/core')\n",
            &replacements(&aliases),
        )
        .unwrap();

        assert_eq!(count, 1);
        assert_eq!(out, "local Chief = require('../packages/roblox/core')\n");
    }

    #[test]
    fn retargeting_leaves_everything_it_was_not_asked_about() {
        let aliases = linked(&[("core", "roblox"), ("net", "server")]);
        let replacements = replacements(&aliases);

        // an alias lpm never linked, and a dependency that really is elsewhere
        let source = "\
            local Chief = require('../packages/shared/core')\n\
            local Other = require('../packages/shared/mystery')\n\
            local Net = require('../packages/server/net')\n\
            local Deep = require('@self/packages/shared/core')\n";
        let (out, count) = retarget_string_requires(source, &replacements).unwrap();
        assert_eq!(count, 2, "{out}");
        assert!(out.contains("require('../packages/roblox/core')"), "{out}");
        assert!(
            out.contains("require('@self/packages/roblox/core')"),
            "{out}"
        );
        // untouched: not a linked alias, and already where it belongs
        assert!(
            out.contains("require('../packages/shared/mystery')"),
            "{out}"
        );
        assert!(out.contains("require('../packages/server/net')"), "{out}");

        /* a package published SINCE the rename says roblox already, and a
        mention outside a require is prose, not a path */
        assert_eq!(
            retarget_string_requires(
                "local Chief = require('../packages/roblox/core')\n\
                 -- moved from ../packages/shared/core\n\
                 local note = \"../packages/shared/core\"\n",
                &replacements
            ),
            None
        );
    }

    #[test]
    fn a_vendored_folder_of_the_same_name_is_never_retargeted() {
        /* the guard that matters: `aliases` is what lpm itself linked, so a
        package shipping its own packages/shared directory keeps it */
        let aliases = linked(&[("core", "roblox")]);
        assert_eq!(
            retarget_string_requires(
                "local Vendored = require('./packages/shared/vendored')\n",
                &replacements(&aliases)
            ),
            None
        );
    }

    #[test]
    fn retargeting_skips_requires_it_cannot_read() {
        let aliases = linked(&[("core", "roblox")]);
        let replacements = replacements(&aliases);
        // a computed path, and a literal carrying escapes, are not ours
        for source in [
            "local Chief = require(base .. '/packages/shared/core')\n",
            "local Chief = require('..\\\\packages/shared/core')\n",
        ] {
            assert_eq!(
                retarget_string_requires(source, &replacements),
                None,
                "{source}"
            );
        }
    }

    /// the replacement pairs `rewrite_legacy_environment_requires` derives.
    fn replacements(aliases: &BTreeMap<String, String>) -> Vec<(String, String)> {
        aliases
            .iter()
            .filter(|(_, environment)| environment.as_str() != LEGACY_ROBLOX_DIR)
            .map(|(alias, environment)| {
                (
                    format!("{PACKAGES_DIR}/{LEGACY_ROBLOX_DIR}/{alias}"),
                    format!("{PACKAGES_DIR}/{environment}/{alias}"),
                )
            })
            .collect()
    }

    #[test]
    fn retargets_on_disk_under_the_module_root() {
        let base = std::env::temp_dir().join("lpm-test-legacy-requires");
        let _ = fs::remove_dir_all(&base);
        let write = |file: &str, contents: &str| {
            let path = base.join(file);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, contents).unwrap();
        };
        write("lpm.toml", "[target]\nmain = \"out/lpm\"\n");
        write(
            "out/lpm/init.luau",
            "local Chief = require('../packages/shared/core')\n",
        );
        // outside the module root, not mounted, not ours
        write("tests/spec.luau", "require('../packages/shared/core')\n");

        let aliases = linked(&[("core", "roblox")]);
        let rewritten = rewrite_legacy_environment_requires(&base, "out/lpm", &aliases).unwrap();

        assert_eq!(rewritten, 1);
        assert_eq!(
            fs::read_to_string(base.join("out/lpm/init.luau")).unwrap(),
            "local Chief = require('../packages/roblox/core')\n"
        );
        assert_eq!(
            fs::read_to_string(base.join("tests/spec.luau")).unwrap(),
            "require('../packages/shared/core')\n"
        );

        // and it settles: a second install over a warm tree changes nothing
        assert_eq!(
            rewrite_legacy_environment_requires(&base, "out/lpm", &aliases).unwrap(),
            0
        );

        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn bracket_and_waitforchild_segments_work() {
        let context = dir_module(0, true);
        let (out, count) = rewrite_source(
            "local A = require(script[\"My Module\"])\n\
             local B = require(script:WaitForChild(\"Util\"))\n",
            &context,
        )
        .unwrap();
        assert_eq!(count, 2);
        assert_eq!(
            out,
            "local A = require(\"@self/My Module\")\n\
             local B = require(\"@self/Util\")\n"
        );
    }

    #[test]
    fn unmappable_chains_are_left_alone() {
        let context = dir_module(0, true);
        let source = "local A = require(script)\n\
                      local B = require(script.Parent.Parent.TooFar.Extra)\n\
                      local C = require(game.ReplicatedStorage.Thing)\n\
                      local D = require(script:FindFirstAncestor(\"x\"))\n\
                      local E = require(modules[i])\n";
        assert_eq!(rewrite_source(source, &context), None);
    }

    #[test]
    fn multibyte_text_survives_the_scan() {
        // satset ships comments with chars like 'ᴗ', byte-stepping used to panic
        let context = dir_module(0, true);
        let source = "local face = \"(ᴗ_ᴗ)\" -- ᴗ\nlocal x = require(script.Core) .. \"日本語\"\n";
        let (out, count) = rewrite_source(source, &context).unwrap();
        assert_eq!(count, 1);
        assert!(out.contains("require(\"@self/Core\")"));
        assert!(out.contains("(ᴗ_ᴗ)"));
        assert!(out.contains("日本語"));
    }

    #[test]
    fn strings_and_comments_are_never_touched() {
        let context = dir_module(0, true);
        let source = "-- require(script.Core.Batcher)\n\
                      --[[ require(script.Core.Batcher) ]]\n\
                      local s = \"require(script.Core.Batcher)\"\n\
                      local l = [[require(script.Core.Batcher)]]\n\
                      local myrequire = 1\n";
        assert_eq!(rewrite_source(source, &context), None);
    }

    #[test]
    fn rewrites_files_on_disk_under_the_module_root() {
        let base = std::env::temp_dir().join("lpm-test-instance-requires");
        let _ = fs::remove_dir_all(&base);
        let write = |file: &str, contents: &str| {
            let path = base.join(file);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, contents).unwrap();
        };
        write("wally.toml", "[package]\nrealm = \"shared\"\n");
        write("src/init.luau", "return require(script.Core)\n");
        write("src/Core/init.luau", "return require(script.Parent.Util)\n");
        write("src/Util.luau", "return {}\n");
        // outside the module root, not mounted, stays untouched
        write("tests/spec.luau", "require(script.Parent.Whatever)\n");

        let rewritten = rewrite_instance_requires(&base, "src").unwrap();
        assert_eq!(rewritten, 2);
        assert_eq!(
            fs::read_to_string(base.join("src/init.luau")).unwrap(),
            "return require(\"@self/Core\")\n"
        );
        assert_eq!(
            fs::read_to_string(base.join("src/Core/init.luau")).unwrap(),
            "return require(\"./Util\")\n"
        );
        assert_eq!(
            fs::read_to_string(base.join("tests/spec.luau")).unwrap(),
            "require(script.Parent.Whatever)\n"
        );

        let _ = fs::remove_dir_all(&base);
    }
}
