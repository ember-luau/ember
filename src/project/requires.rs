/*! turns roblox instance requires in wally packages into string requires.
wally-era code says require(script.Parent.Foo), but there's no instance tree
in our layout, so that has to become a "./Foo" style path. anything we can't
map safely just stays as it was. */

use crate::error::Error;
use std::fs;
use std::path::{Path, PathBuf};

/** rewrites every mappable `require(script...)` chain under the package's
entry module. `entry` is the normalized entry point ("" = root init, "src" =
dir module, "lib" = single file). returns how many requires got rewritten.

how the mapping works:
- paths come out instance-relative, string-require style: "./" is siblings,
  "@self/" is children. an init file IS its folder, so its whole frame sits
  one level above a plain file's
- climbing exactly one level past the module root means a wally dependency
  alias (wally drops those next to the package). our link files sit in the
  out dir instead, two levels above the package folder, hence the extra ups
- anything weirder (children of a plain file module, climbing further, non
  literal segments) gets skipped */
pub fn rewrite_instance_requires(package_dir: &Path, entry: &str) -> Result<usize, Error> {
    // where the mounted tree starts; files outside it have no instance position
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
        // a lone file module mounts by itself; its siblings aren't in the tree
        Some(entry_path) => ["luau", "lua"]
            .iter()
            .map(|ext| package_dir.join(entry_path).with_extension(ext))
            .filter(|path| path.is_file())
            .take(1)
            .collect(),
        None => luau_files(&package_dir.join(&module_root))?,
    };

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
        };
        let (updated, count) = rewrite_source(&source, &context);
        if count > 0 {
            fs::write(&file, updated)?;
            rewritten += count;
        }
    }
    Ok(rewritten)
}

struct FileContext {
    is_init: bool,
    /// how many dirs the file's folder sits below the module root.
    depth_in_module: usize,
    /// same but below the package folder; alias paths need this many ups + 2.
    depth_in_package: usize,
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

/// rewrites all mappable chains in one file. returns (new source, count).
fn rewrite_source(source: &str, context: &FileContext) -> (String, usize) {
    let mut output = String::with_capacity(source.len());
    let mut count = 0;
    let bytes = source.as_bytes();
    let mut position = 0;

    while position < bytes.len() {
        /* stay out of strings and comments so a require inside either is
        never touched */
        match bytes[position] {
            b'-' if bytes.get(position + 1) == Some(&b'-') => {
                let end = skip_comment(bytes, position);
                output.push_str(&source[position..end]);
                position = end;
            }
            b'"' | b'\'' => {
                let end = skip_short_string(bytes, position);
                output.push_str(&source[position..end]);
                position = end;
            }
            b'[' if long_bracket_level(bytes, position).is_some() => {
                let end = skip_long_string(bytes, position);
                output.push_str(&source[position..end]);
                position = end;
            }
            _ => {
                if at_word(bytes, position, b"require")
                    && let Some((end, replacement)) =
                        parse_chain(source, position + "require".len(), context)
                {
                    output.push_str(&format!("require(\"{replacement}\")"));
                    position = end;
                    count += 1;
                    continue;
                }
                // push whole identifiers at once so "myrequire" can't match
                let end = if is_ident_byte(bytes[position]) {
                    let mut end = position;
                    while end < bytes.len() && is_ident_byte(bytes[end]) {
                        end += 1;
                    }
                    end
                } else {
                    // advance a whole char; slicing mid-codepoint panics
                    position
                        + source[position..]
                            .chars()
                            .next()
                            .map_or(1, |c| c.len_utf8())
                };
                output.push_str(&source[position..end]);
                position = end;
            }
        }
    }
    (output, count)
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

    /* walk the chain file-relative: `leaf` means we're at the file itself
    (a non-init module), `ups` counts how far above the file's dir we are */
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
        return None; // require(script) / require(script.Parent): nothing to point at
    }

    /* string requires resolve instance-relative: "./" is the module's
    siblings, "@self/" its children. an init file IS its folder, so its
    frame sits one level higher than a plain file's: children need @self,
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
        /* one level above the module root is wally's alias zone; our link
        files are in the out dir, package depth + 2 ups away */
        let mut parts = vec![".."; context.depth_in_package + 2 - init_shift];
        parts.push(names[0].as_str());
        parts.join("/")
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

/// a quoted "name" / 'name'; escapes and empty names bail (not worth mapping).
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

/// `--` line or `--[[ ]]` block comment starting at `position`; returns the end.
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

    fn dir_module(depth_in_module: usize, is_init: bool) -> FileContext {
        FileContext {
            is_init,
            depth_in_module,
            // package with module root "src": file dir depth is one more
            depth_in_package: depth_in_module + 1,
        }
    }

    #[test]
    fn init_files_require_children_through_self() {
        /* src/init.luau in a src module, satset's shape. an init file IS its
        folder, so children are @self and "./" would hit siblings instead */
        let context = dir_module(0, true);
        let (out, count) =
            rewrite_source("local Batcher = require(script.Core.Batcher)\n", &context);
        assert_eq!(count, 1);
        assert_eq!(out, "local Batcher = require(\"@self/Core/Batcher\")\n");
    }

    #[test]
    fn init_parent_hops_render_one_level_shorter() {
        // src/Sub/init.luau: script.Parent.X is src/X, a sibling, so "./"
        let context = dir_module(1, true);
        let (out, count) = rewrite_source(
            "local A = require(script.Parent.Util)\n\
             local B = require(script.Other)\n",
            &context,
        );
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
        );
        assert_eq!(count, 2);
        assert_eq!(
            out,
            "local A = require(\"./Sanitizer\")\n\
             local B = require(\"../Core/Batcher\")\n"
        );
    }

    #[test]
    fn escaping_the_module_root_lands_on_dependency_links() {
        /* src/init.luau doing require(script.Parent.Dep): wally would find the
        alias next to the package, we keep links in the out dir. the init's
        frame is the pkg dir (out/.lpm/pkg), so 2 ups reach out/. */
        let context = dir_module(0, true);
        let (out, count) = rewrite_source("local Dep = require(script.Parent.Signal)\n", &context);
        assert_eq!(count, 1);
        assert_eq!(out, "local Dep = require(\"../../Signal\")\n");

        // same reach from a plain file next to that init needs the extra hop
        let context = dir_module(0, false);
        let (out, count) = rewrite_source(
            "local Dep = require(script.Parent.Parent.Signal)\n",
            &context,
        );
        assert_eq!(count, 1);
        assert_eq!(out, "local Dep = require(\"../../../Signal\")\n");
    }

    #[test]
    fn bracket_and_waitforchild_segments_work() {
        let context = dir_module(0, true);
        let (out, count) = rewrite_source(
            "local A = require(script[\"My Module\"])\n\
             local B = require(script:WaitForChild(\"Util\"))\n",
            &context,
        );
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
        let (out, count) = rewrite_source(source, &context);
        assert_eq!(count, 0);
        assert_eq!(out, source);
    }

    #[test]
    fn multibyte_text_survives_the_scan() {
        // satset ships comments with chars like 'ᴗ'; byte-stepping used to panic
        let context = dir_module(0, true);
        let source = "local face = \"(ᴗ_ᴗ)\" -- ᴗ\nlocal x = require(script.Core) .. \"日本語\"\n";
        let (out, count) = rewrite_source(source, &context);
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
        let (out, count) = rewrite_source(source, &context);
        assert_eq!(count, 0);
        assert_eq!(out, source);
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
        // outside the module root: not mounted, stays untouched
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
