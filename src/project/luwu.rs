/*! a Luau stand-in for a Luwu module, so that the link file can restate the
module's types. full_moon reads Luau only, and Luwu adds classes, traits, `.{ }`
destructuring and `export` on values. the type parse needs the declarations
only, so a token scan writes each one as a Luau line:

```luau
local Types = require("@self/Types")
type Private = any
export type Box<T = Types.Default> = any
```

the bodies are `any` because the link file restates the names and the generics
only, `export type Box<T> = module.Box<T>`. the checker reads the real types
through `module`. */

use super::package::{self, RequireFrame};
use std::fs;
use std::path::Path;

/// how many modules deep a re-export is followed. chimera goes two deep, entry to `src/components` to `code`.
const MAX_DEPTH: usize = 4;

/// type names that a default can name in any module, so a default made of these alone survives a move to the link file.
const BUILTIN_TYPES: &[&str] = &[
    "any", "unknown", "never", "nil", "none", "boolean", "number", "string", "buffer", "thread",
    "userdata", "vector", "true", "false",
];

/** the stand-in for the Luwu module `source`, see the module docs. `dir` is
the package root and `frame` the module's require frames, both to find the
modules that a `.{ }` export reads from. `keep_defaults` = false writes each
generic without its default, for a default that full_moon can't parse. */
pub fn stand_in(dir: &Path, source: &str, frame: &RequireFrame, keep_defaults: bool) -> String {
    let scan = scan(source);
    let mut lines = Vec::new();
    for (name, path) in &scan.requires {
        lines.push(format!("local {name} = require(\"{path}\")"));
    }
    for item in &scan.items {
        match item {
            Item::Private(name) => lines.push(format!("type {name} = any")),
            Item::Type { name, generics, .. } => lines.push(format!(
                "export type {name}{} = any",
                render(generics, |_| keep_defaults)
            )),
            Item::Function { name, parameters } => lines.push(format!(
                "export type function {name}({}) end",
                parameters.join(", ")
            )),
            Item::Destructure {
                fields,
                from,
                types,
            } => {
                for (key, name) in fields {
                    let found = from
                        .resolve(&scan.requires, frame)
                        .and_then(|module| find(dir, &module, key, *types, 1));
                    /* a value field is a type only when it is a class or a
                    trait. a type field is a type whatever we find, so a source
                    that we can't read still gets its name restated */
                    let generics = match found {
                        Some(generics) => generics,
                        None if *types => Vec::new(),
                        None => continue,
                    };
                    // a default from the other module can only name what every module has
                    let portable = |default: &[String]| {
                        keep_defaults
                            && default.iter().all(|token| {
                                BUILTIN_TYPES.contains(&token.as_str()) || is_literal(token)
                            })
                    };
                    lines.push(format!(
                        "export type {name}{} = any",
                        render(&generics, portable)
                    ));
                }
            }
        }
    }
    let mut out = lines.join("\n");
    out.push('\n');
    out
}

/// whether a token is a value that a singleton type can spell: a string, or punctuation like `|`, `?` and `{`.
fn is_literal(token: &str) -> bool {
    token.starts_with(['"', '\''])
        || !token.starts_with(|c: char| c.is_ascii_alphanumeric() || c == '_')
}

/** the generics of the type `key` that the module at `module` exports, for a
`.{ }` export. `types` = false is a value field, which finds only a class or a
trait. None = no such export, or nothing readable. */
fn find(dir: &Path, module: &str, key: &str, types: bool, depth: usize) -> Option<Vec<Generic>> {
    if depth > MAX_DEPTH {
        return None;
    }
    let source = fs::read_to_string(package::entry_source(dir, module)?).ok()?;
    let scan = scan(&source);
    for item in &scan.items {
        match item {
            Item::Type {
                name,
                generics,
                nominal,
            } if name == key && (types || *nominal) => return Some(generics.clone()),
            Item::Function { name, parameters } if name == key && types => {
                return Some(
                    parameters
                        .iter()
                        .map(|parameter| Generic::plain(parameter))
                        .collect(),
                );
            }
            Item::Destructure {
                fields,
                from,
                types: field_types,
            } if types || !field_types => {
                let Some((source_key, _)) = fields.iter().find(|(_, name)| name == key) else {
                    continue;
                };
                let frame = RequireFrame::of(dir, module);
                // a value field is a type only as a class or a trait, however it was asked for
                let types = types && *field_types;
                return find(
                    dir,
                    &from.resolve(&scan.requires, &frame)?,
                    source_key,
                    types,
                    depth + 1,
                );
            }
            _ => {}
        }
    }
    None
}

/** `<T, U = string>`, or nothing for no generics. `keep` decides, per
default, whether it stays. Luau wants the defaults last, so a dropped default
drops each one before it too. */
fn render(generics: &[Generic], keep: impl Fn(&[String]) -> bool) -> String {
    if generics.is_empty() {
        return String::new();
    }
    let kept: Vec<bool> = generics
        .iter()
        .map(|generic| generic.default.as_deref().is_some_and(&keep))
        .collect();
    let first = kept
        .iter()
        .rposition(|kept| !kept)
        .map_or(0, |bare| bare + 1);
    let parameters: Vec<String> = generics
        .iter()
        .enumerate()
        .map(|(index, generic)| match &generic.default {
            Some(default) if index >= first => format!("{} = {}", generic.parameter, join(default)),
            _ => generic.parameter.clone(),
        })
        .collect();
    format!("<{}>", parameters.join(", "))
}

/// what one module declares, in source order.
#[derive(Default)]
struct Scan {
    /// `local X = require("path")` and `const X = require("path")`, the binding and the raw path.
    requires: Vec<(String, String)>,
    items: Vec<Item>,
}

enum Item {
    /// a type, class or trait that the module keeps to itself. the stand-in declares it so that a default which names it gets dropped.
    Private(String),
    /// `export type X<T>`, `export class X<T>` or `export trait X<T>`. `nominal` = a class or a trait, which is a value too.
    Type {
        name: String,
        generics: Vec<Generic>,
        nominal: bool,
    },
    /// `export type function X(a, b)`.
    Function {
        name: String,
        parameters: Vec<String>,
    },
    /// `export type .{ A, B as C } = m` when `types`, else `export .{ ... } = m`. fields are `(key, name)`.
    Destructure {
        fields: Vec<(String, String)>,
        from: From,
        types: bool,
    },
}

#[derive(Clone)]
struct Generic {
    /// `T`, or `T...` for a pack.
    parameter: String,
    /// the tokens right of the `=`.
    default: Option<Vec<String>>,
}

impl Generic {
    fn plain(parameter: &str) -> Self {
        Generic {
            parameter: parameter.to_string(),
            default: None,
        }
    }
}

/// the right side of a `.{ }` export.
enum From {
    /// a binding, `core` in `= core`.
    Binding(String),
    /// a direct `= require("path")`.
    Require(String),
    /// anything else, `= hub.t` or a call. no module to read.
    Other,
}

impl From {
    /// the package-root-relative module this side names.
    fn resolve(&self, requires: &[(String, String)], frame: &RequireFrame) -> Option<String> {
        let raw = match self {
            From::Binding(name) => {
                &requires
                    .iter()
                    .rev()
                    .find(|(binding, _)| binding == name)?
                    .1
            }
            From::Require(raw) => raw,
            From::Other => return None,
        };
        package::resolve_import(raw, frame)
    }
}

/** the declarations in `source`. the scan reads tokens, so the text of
comments and strings never matches, and a body that full_moon can't read
costs nothing. `export` is only legal at the top level, so the scan doesn't
track blocks. */
fn scan(source: &str) -> Scan {
    let tokens = tokens(source);
    let text = |at: usize| tokens.get(at).map_or("", |token| token.text);
    let is_name = |at: usize| tokens.get(at).is_some_and(|token| token.kind == Kind::Name);
    let mut scan = Scan::default();
    let mut at = 0;
    while at < tokens.len() {
        // `x.export`, `self:type()`, and a name that a statement binds, `local type = 1`
        let follows = |words: &[&str]| at > 0 && words.contains(&text(at - 1));
        if follows(&[".", ":", "local", "const", "function"]) {
            at += 1;
            continue;
        }
        match text(at) {
            "local" | "const" if is_name(at + 1) => at = binding(&tokens, at + 1, &mut scan),
            "export" => {
                let mut next = at + 1;
                // `export declare class X`, `export declare extern type X`
                while matches!(text(next), "declare" | "extern") {
                    next += 1;
                }
                at = match declaration(&tokens, next, true, &mut scan) {
                    Some(after) => after,
                    // `export core = require(...)` binds `core` the way `const core` does
                    None if is_name(next) && text(next + 1) == "=" => {
                        binding(&tokens, next, &mut scan)
                    }
                    None => next,
                };
            }
            "type" | "class" | "trait" | "declare" => {
                at = declaration(&tokens, at, false, &mut scan).unwrap_or(at + 1);
            }
            _ => at += 1,
        }
    }
    scan
}

/// records `X = require("path")` for the name at `at` into `scan`, and returns where the scan goes on.
fn binding(tokens: &[Token], at: usize, scan: &mut Scan) -> usize {
    let text = |at: usize| tokens.get(at).map_or("", |token| token.text);
    // `const core.{ x } = require(...)` binds `core` too
    let mut next = at + 1;
    if text(next) == "." && text(next + 1) == "{" {
        next = closing(tokens, next + 1);
    }
    if text(next) == "="
        && let Some(path) = required(tokens, next + 1)
    {
        scan.requires.push((text(at).to_string(), path));
    }
    next
}

/** reads the declaration that starts at `at`, after any `export`, into
`scan`. returns where the scan goes on, or None when no declaration starts
there. */
fn declaration(tokens: &[Token], at: usize, exported: bool, scan: &mut Scan) -> Option<usize> {
    let text = |at: usize| tokens.get(at).map_or("", |token| token.text);
    let is_name = |at: usize| tokens.get(at).is_some_and(|token| token.kind == Kind::Name);
    match text(at) {
        // `declare class X` names a type like `class X` does
        "declare" if !exported => declaration(
            tokens,
            at + 1 + usize::from(text(at + 1) == "extern"),
            false,
            scan,
        ),
        "type" if text(at + 1) == "function" && is_name(at + 2) => {
            let name = text(at + 2).to_string();
            let (parameters, next) = parameters(tokens, at + 3);
            scan.items.push(if exported {
                Item::Function { name, parameters }
            } else {
                Item::Private(name)
            });
            Some(next)
        }
        "type" if text(at + 1) == "." && text(at + 2) == "{" => {
            let (fields, from, next) = destructure(tokens, at + 2);
            if exported {
                scan.items.push(Item::Destructure {
                    fields,
                    from,
                    types: true,
                });
            } else {
                scan.items
                    .extend(fields.into_iter().map(|(_, name)| Item::Private(name)));
            }
            Some(next)
        }
        "type" | "class" | "trait" if is_name(at + 1) => {
            // `declare extern type X` declares a nominal type, like a class
            let nominal = text(at) != "type" || (at > 0 && text(at - 1) == "extern");
            // `type X = ...` and `type X<T>`. a nominal name stands alone, `class Cat(name: string)`
            if !nominal && !matches!(text(at + 2), "=" | "<") {
                return None;
            }
            let name = text(at + 1).to_string();
            let (generics, next) = generics(tokens, at + 2);
            scan.items.push(if exported {
                Item::Type {
                    name,
                    generics,
                    nominal,
                }
            } else {
                Item::Private(name)
            });
            Some(next)
        }
        "." if exported && text(at + 1) == "{" => {
            let (fields, from, next) = destructure(tokens, at + 1);
            scan.items.push(Item::Destructure {
                fields,
                from,
                types: false,
            });
            Some(next)
        }
        _ => None,
    }
}

/// the generic list that opens at `at`, if one does, and the index after it.
fn generics(tokens: &[Token], at: usize) -> (Vec<Generic>, usize) {
    if tokens.get(at).is_none_or(|token| token.text != "<") {
        return (Vec::new(), at);
    }
    let end = closing(tokens, at);
    let inner = &tokens[at + 1..end.saturating_sub(1).max(at + 1)];
    let generics = split(inner)
        .into_iter()
        .filter(|part| !part.is_empty())
        .map(|part| {
            let equals = part.iter().position(|token| token.text == "=");
            let (parameter, default) = match equals {
                Some(equals) => (&part[..equals], Some(&part[equals + 1..])),
                None => (part, None),
            };
            Generic {
                parameter: join(
                    &parameter
                        .iter()
                        .map(|token| token.text.to_string())
                        .collect::<Vec<_>>(),
                ),
                default: default
                    .map(|default| default.iter().map(|token| token.text.to_string()).collect()),
            }
        })
        .collect();
    (generics, end)
}

/// the parameter names of a type function, `(a: type, b: type)`, and the index after the list.
fn parameters(tokens: &[Token], at: usize) -> (Vec<String>, usize) {
    if tokens.get(at).is_none_or(|token| token.text != "(") {
        return (Vec::new(), at);
    }
    let end = closing(tokens, at);
    let inner = &tokens[at + 1..end.saturating_sub(1).max(at + 1)];
    let names = split(inner)
        .into_iter()
        .filter_map(|part| part.first().filter(|token| token.kind == Kind::Name))
        .map(|token| token.text.to_string())
        .collect();
    (names, end)
}

/** the fields of a `.{ }` pattern whose `{` is at `at`, the side right of
its `=`, and the index after that. a field is `(key, name)`: `A` is `(A, A)`,
`A as B` is `(A, B)`, `A.{ x }` binds `A` too, and `A as .{ x }` binds no name
of its own at this level. */
fn destructure(tokens: &[Token], at: usize) -> (Vec<(String, String)>, From, usize) {
    let end = closing(tokens, at);
    let inner = &tokens[at + 1..end.saturating_sub(1).max(at + 1)];
    let fields = split(inner)
        .into_iter()
        .filter_map(|part| {
            let key = part.first().filter(|token| token.kind == Kind::Name)?.text;
            match (part.get(1).map(|token| token.text), part.get(2)) {
                (Some("as"), Some(name)) if name.kind == Kind::Name => {
                    Some((key.to_string(), name.text.to_string()))
                }
                (Some("as"), _) => None,
                _ => Some((key.to_string(), key.to_string())),
            }
        })
        .collect();

    let text = |at: usize| tokens.get(at).map_or("", |token| token.text);
    if text(end) != "=" {
        return (fields, From::Other, end);
    }
    if let Some(path) = required(tokens, end + 1) {
        return (fields, From::Require(path), end + 1);
    }
    let binding = tokens.get(end + 1).filter(|token| {
        token.kind == Kind::Name && !matches!(text(end + 2), "." | ":" | "(" | "[")
    });
    let from = binding.map_or(From::Other, |token| From::Binding(token.text.to_string()));
    (fields, from, end + 1)
}

/// the path of a `require("path")` or `require "path"` call at `at`. None for any other shape, or a path with an escape.
fn required(tokens: &[Token], at: usize) -> Option<String> {
    let text = |at: usize| tokens.get(at).map_or("", |token| token.text);
    if text(at) != "require" {
        return None;
    }
    let (literal, after) = if text(at + 1) == "(" && text(at + 3) == ")" {
        (tokens.get(at + 2)?, at + 4)
    } else {
        (tokens.get(at + 1)?, at + 2)
    };
    // require("x").y is a value, not the module
    if literal.kind != Kind::Str || matches!(text(after), "." | ":" | "[" | "(") {
        return None;
    }
    let path = literal.text.get(1..literal.text.len().checked_sub(1)?)?;
    let quote = &literal.text[..1];
    (literal.text.ends_with(quote) && quote != "[" && !path.contains(['\\', '"', '\n']))
        .then(|| path.to_string())
}

/** the index after the bracket that closes the one at `at`. a `<` counts
`<>` too, since generics nest as `<T = Map<K, V>>`. any other bracket leaves
them out, so a `<` comparison can't throw the count off. */
fn closing(tokens: &[Token], at: usize) -> usize {
    let angles = tokens.get(at).is_some_and(|token| token.text == "<");
    let mut depth = 0usize;
    for (index, token) in tokens.iter().enumerate().skip(at) {
        if matches!(token.text, "(" | "{" | "[") || (angles && token.text == "<") {
            depth += 1;
        } else if matches!(token.text, ")" | "}" | "]") || (angles && token.text == ">") {
            depth = depth.saturating_sub(1);
            if depth == 0 {
                return index + 1;
            }
        }
    }
    tokens.len()
}

/// `tokens` split at each top-level comma.
fn split<'a, 'b>(tokens: &'b [Token<'a>]) -> Vec<&'b [Token<'a>]> {
    let mut parts = Vec::new();
    let mut depth = 0usize;
    let mut start = 0;
    for (index, token) in tokens.iter().enumerate() {
        match token.text {
            "(" | "{" | "[" | "<" => depth += 1,
            ")" | "}" | "]" | ">" => depth = depth.saturating_sub(1),
            "," if depth == 0 => {
                parts.push(&tokens[start..index]);
                start = index + 1;
            }
            _ => {}
        }
    }
    parts.push(&tokens[start..]);
    parts
}

/// tokens back to one line of type text, `{ read string | number }` as `{read string | number}`.
fn join(tokens: &[String]) -> String {
    let mut out = String::new();
    for (index, token) in tokens.iter().enumerate() {
        if index > 0 {
            let previous = tokens[index - 1].as_str();
            let tight_after = matches!(previous, "<" | "(" | "[" | "{" | ".");
            let tight_before = matches!(
                token.as_str(),
                "," | ">" | ")" | "]" | "}" | "..." | "?" | "." | ":"
            ) || (matches!(token.as_str(), "(" | "<")
                && previous.starts_with(|c: char| c.is_ascii_alphanumeric() || c == '_'));
            if !tight_after && !tight_before {
                out.push(' ');
            }
        }
        out.push_str(token);
    }
    out
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Kind {
    Name,
    /// a quoted or long-bracket string.
    Str,
    /// punctuation, a number, or an interpolated string.
    Other,
}

#[derive(Debug)]
struct Token<'a> {
    kind: Kind,
    text: &'a str,
}

/// Luau and Luwu tokens, comments and whitespace left out. an unclosed string or comment runs to the end of its line or the source.
fn tokens(source: &str) -> Vec<Token<'_>> {
    let bytes = source.as_bytes();
    let mut tokens = Vec::new();
    let mut at = 0;
    while at < bytes.len() {
        let start = at;
        let kind = match bytes[at] {
            byte if byte.is_ascii_whitespace() => {
                at += 1;
                continue;
            }
            b'-' if bytes.get(at + 1) == Some(&b'-') => {
                at = long_bracket(bytes, at + 2).unwrap_or_else(|| line_end(bytes, at));
                continue;
            }
            b'[' if long_bracket(bytes, at).is_some() => {
                at = long_bracket(bytes, at).unwrap_or(bytes.len());
                Kind::Str
            }
            b'"' | b'\'' => {
                at = quoted(bytes, at);
                Kind::Str
            }
            b'`' => {
                at = interpolated(bytes, at);
                Kind::Other
            }
            byte if byte.is_ascii_alphabetic() || byte == b'_' => {
                at = word_end(bytes, at);
                Kind::Name
            }
            byte if byte.is_ascii_digit() => {
                at = word_end(bytes, at);
                Kind::Other
            }
            _ => {
                let rest = &source[at..];
                at += ["...", "->", "::", "==", "~=", "<=", ">="]
                    .iter()
                    .find(|symbol| rest.starts_with(*symbol))
                    .map_or_else(
                        || rest.chars().next().map_or(1, char::len_utf8),
                        |symbol| symbol.len(),
                    );
                Kind::Other
            }
        };
        tokens.push(Token {
            kind,
            text: &source[start..at],
        });
    }
    tokens
}

/// the end of a name or a number. `.` continues a number, `1.5`, but `x.y` lexes the `.` on its own.
fn word_end(bytes: &[u8], at: usize) -> usize {
    let number = bytes[at].is_ascii_digit();
    let mut end = at;
    while end < bytes.len() {
        let byte = bytes[end];
        let exponent =
            number && matches!(byte, b'+' | b'-') && matches!(bytes[end - 1], b'e' | b'E');
        if byte.is_ascii_alphanumeric() || byte == b'_' || (number && byte == b'.') || exponent {
            end += 1;
        } else {
            break;
        }
    }
    end
}

fn line_end(bytes: &[u8], at: usize) -> usize {
    bytes[at..]
        .iter()
        .position(|&byte| byte == b'\n')
        .map_or(bytes.len(), |offset| at + offset)
}

/// the end of a `[[...]]` or `[==[...]==]` that opens at `at`. None when no long bracket opens there.
fn long_bracket(bytes: &[u8], at: usize) -> Option<usize> {
    if bytes.get(at) != Some(&b'[') {
        return None;
    }
    let level = bytes[at + 1..]
        .iter()
        .take_while(|&&byte| byte == b'=')
        .count();
    if bytes.get(at + 1 + level) != Some(&b'[') {
        return None;
    }
    let close = format!("]{}]", "=".repeat(level));
    let body = at + 2 + level;
    Some(
        bytes[body..]
            .windows(close.len())
            .position(|window| window == close.as_bytes())
            .map_or(bytes.len(), |offset| body + offset + close.len()),
    )
}

/// the end of a `"..."` or `'...'` that opens at `at`.
fn quoted(bytes: &[u8], at: usize) -> usize {
    let quote = bytes[at];
    let mut end = at + 1;
    while end < bytes.len() {
        match bytes[end] {
            b'\\' => end += 2,
            b'\n' => return end,
            byte if byte == quote => return end + 1,
            _ => end += 1,
        }
    }
    bytes.len()
}

/// the end of a `` `...{expr}...` `` that opens at `at`. an expression can hold strings and braces of its own.
fn interpolated(bytes: &[u8], at: usize) -> usize {
    let mut end = at + 1;
    while end < bytes.len() {
        match bytes[end] {
            b'\\' => end += 2,
            b'\n' => return end,
            b'`' => return end + 1,
            b'{' => {
                let mut depth = 0usize;
                while end < bytes.len() {
                    match bytes[end] {
                        b'"' | b'\'' => end = quoted(bytes, end),
                        b'`' => end = interpolated(bytes, end),
                        b'{' => {
                            depth += 1;
                            end += 1;
                        }
                        b'}' => {
                            depth -= 1;
                            end += 1;
                            if depth == 0 {
                                break;
                            }
                        }
                        _ => end += 1,
                    }
                }
            }
            _ => end += 1,
        }
    }
    bytes.len()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stand_in_of(source: &str) -> String {
        stand_in(
            Path::new("/nonexistent"),
            source,
            &RequireFrame::init(""),
            true,
        )
    }

    /// comments, strings, field names and methods hold the word `export` without declaring anything.
    #[test]
    fn only_declarations_match() {
        let source = r#"
            -- export type InALineComment = string
            --[==[ export type InALongComment = string ]==]
            const a = "export type InAString = \" string"
            const b = 'export type InASingleQuote = string'
            const c = [[ export type InALongString = string ]]
            const d = `export type {"InInterpolation"} {`{nested}`} = string`
            const e = { export = 1, type = 2 }
            e.export = type(e)
            self:type()
            local type = 3
            export type Real<T = { [string]: (number) -> boolean }, U = Map<T, { T }>> = T
        "#;
        assert_eq!(
            stand_in_of(source),
            "export type Real<T = {[string]: (number) -> boolean}, U = Map<T, {T}>> = any\n"
        );
    }

    /// text that never closes runs to the end of its line or the source, and the scan still ends.
    #[test]
    fn unclosed_text_ends_the_scan() {
        for source in [
            "export type A = \"never closed",
            "--[[ never closed\nexport type A = string",
            "const s = `{ never closed",
            "const s = [==[ never closed",
            "export type A<T = { never closed",
            "export type A<T = é, U = \"\\",
            "export .{ A",
            "export type .{ A } =",
        ] {
            stand_in_of(source);
        }
        assert_eq!(
            stand_in_of("const s = \"no close\nexport type Next = string"),
            "export type Next = any\n"
        );
    }
}
