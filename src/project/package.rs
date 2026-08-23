/*! an extracted package on disk, entry point, target environment, and the link
file pointing at it. the reading half of install, no downloads here. has to
understand foreign manifests, pesde.toml, wally.toml, Rojo project files, since
that's all a published package carries. */

use crate::error::Error;
use crate::project::manifest::Environment;
use crate::project::rojo::{PACKAGES_DIR, PROJECT_FILE};
use full_moon::ast::luau::{ExportedTypeDeclaration, ExportedTypeFunction};
use full_moon::visitors::Visitor;
use std::fs;
use std::path::{Path, PathBuf};

/// what the link file calls the package it wraps. a package binding of the same name loses its imports rather than shadow this.
const MODULE_BINDING: &str = "module";

/** what a link file has to restate about one module: its exported types, the
modules those types reach through, and the aliases that make the reaching
legal. */
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Exports {
    /// `export type X<T> = module.X<T>` lines, in source order.
    pub types: Vec<String>,
    /// `(binding, package-root-relative module path)` for each module the lines name, first use first.
    pub imports: Vec<(String, String)>,
    /// `type __ember_X_T = Types.Default` lines, one per generic default that had to be hoisted.
    pub aliases: Vec<String>,
}

/** body of a generated link file. requires the stored package and restates its
exported types, e.g.

```luau
local module = require("./.ember/scope_pkg/lib")
local Types = require("./.ember/scope_pkg/lib/Types")
type __ember_Result_E = Types.Default
export type Result<T, E = __ember_Result_E> = module.Result<T, E>
return module
```

the require and the alias only appear when a default needs them, see
[`exported_types`]. no exported types at all = the compact
`return require(...)` form. */
pub fn link_contents(folder: &str, entry: &str, exports: &Exports) -> String {
    link_contents_at(&format!("./.ember/{folder}"), entry, exports)
}

/** like [`link_contents`] but rooted anywhere. workspace members link straight
to their source, like `../../packages/core` + `src`, instead of an extracted
copy under `.ember/`. `root` is the package, `entry` the module inside it, kept
apart because imports are relative to the package, not to the module. */
pub fn link_contents_at(root: &str, entry: &str, exports: &Exports) -> String {
    // empty entry = the package root itself is the module, a root init file
    let module_path = if entry.is_empty() {
        root.to_string()
    } else {
        format!("{root}/{entry}")
    };
    if exports.types.is_empty() {
        return format!("return require(\"{module_path}\")\n");
    }

    let mut contents = format!("local {MODULE_BINDING} = require(\"{module_path}\")\n");
    /* the aliases below name these the way the source did, so the bindings
    have to exist here too, pointing at the same modules through the link
    file's own path */
    for (name, path) in &exports.imports {
        contents.push_str(&format!("local {name} = require(\"{root}/{path}\")\n"));
    }
    for line in &exports.aliases {
        contents.push_str(line);
        contents.push('\n');
    }
    for line in &exports.types {
        contents.push_str(line);
        contents.push('\n');
    }
    contents.push_str("return module\n");
    contents
}

/** the two frames Luau's require-by-string gives a module, so a require in the
entry source can be pointed at from somewhere else.

an init file IS its folder: `./` from `src/init.luau` means a sibling of `src`,
which is the package root, while `@self/` means one of its children, `src/X`.
a plain `lib.luau` has both frames at the folder it sits in. same rule
`requires::parse_chain` renders instance chains with, read the other way. */
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RequireFrame {
    /// what `@self/` resolves against, package-root-relative.
    own: String,
    /// what `./` and `../` resolve against. None when that frame sits above the package, where nothing is ours to point at.
    siblings: Option<String>,
}

impl RequireFrame {
    /// the frames of `entry` in the package at `dir`, read from which file the entry actually resolves to.
    pub fn of(dir: &Path, entry: &str) -> Self {
        let is_init = entry_source(dir, entry)
            .and_then(|path| Some(path.file_stem()?.to_str()? == "init"))
            .unwrap_or(false);
        if is_init {
            Self::init(entry)
        } else {
            Self::file(&parent_dir(entry))
        }
    }

    /// frames of an `init` file, which stands for the folder it sits in: `src` for `src/init.luau`.
    pub fn init(folder: &str) -> Self {
        RequireFrame {
            own: folder.to_string(),
            // a root init's siblings are outside the package
            siblings: (!folder.is_empty()).then(|| parent_dir(folder)),
        }
    }

    /// frames of a plain file, both the folder it sits in.
    pub fn file(folder: &str) -> Self {
        RequireFrame {
            own: folder.to_string(),
            siblings: Some(folder.to_string()),
        }
    }
}

/// the folder part of a package-root-relative path, empty at the root.
fn parent_dir(path: &str) -> String {
    path.rsplit_once('/')
        .map(|(dir, _)| dir.to_string())
        .unwrap_or_default()
}

/** where a require in the entry source points, as a package-root-relative
module path.

None for everything the link file can't reach: a named `@alias/` needs the
package's own .luaurc, an instance require isn't a path at all, and a `../`
chain that climbs out of the package has nothing on the other side. */
fn resolve_import(raw: &str, frame: &RequireFrame) -> Option<String> {
    let (base, rest) = if let Some(rest) = raw.strip_prefix("@self/") {
        (frame.own.as_str(), rest)
    } else if raw.starts_with("./") || raw.starts_with("../") {
        (frame.siblings.as_deref()?, raw)
    } else {
        return None;
    };

    let mut parts: Vec<&str> = base.split('/').filter(|part| !part.is_empty()).collect();
    for part in rest.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                parts.pop()?;
            }
            name => parts.push(name),
        }
    }
    (!parts.is_empty()).then(|| parts.join("/"))
}

/// the file an extensionless entry resolves to, Luau string-require style, `<entry>.luau`, `<entry>.lua`, then the folder's init file. empty entry = the root's own init.
pub fn entry_source(dir: &Path, entry: &str) -> Option<PathBuf> {
    let candidates = if entry.is_empty() {
        vec!["init.luau".to_string(), "init.lua".to_string()]
    } else {
        vec![
            format!("{entry}.luau"),
            format!("{entry}.lua"),
            format!("{entry}/init.luau"),
            format!("{entry}/init.lua"),
        ]
    };
    candidates
        .into_iter()
        .map(|candidate| dir.join(candidate))
        .find(|path| path.is_file())
}

/// full_moon recurses per nesting level, so deep sources need serious stack. parsing gets its own thread with this much.
const PARSE_STACK_BYTES: usize = 64 * 1024 * 1024;

/** sources nested deeper than this are refused without parsing. a stack overflow
can't be caught, it aborts the whole process, so the ceiling must be enforced
before full_moon ever runs. real Luau tops out around depth ~50, this is 10x that. */
const MAX_NESTING_DEPTH: usize = 500;

/** `export type` re-export lines for a link file. Luau type exports are lexical,
they don't flow through `return require(...)`, so the link file must restate each
one as `export type X<T> = module.X<T>`, same scheme as pesde's linker. the
declaration side keeps generic defaults, the use side drops them. exported type
functions re-export the same way with their parameters as generics, parameterless
ones have no type-declaration equivalent and get skipped.

a kept default is copied verbatim, so whatever it names has to exist in the link
file too. a package that keeps its types in a second module, `export type
Signal<S = Types.Default>` over `local Types = require("@self/Types")`, needs
that require restated as well, which is what `frame` is for and what
[`Exports::imports`] carries. defaults naming something the link file can't
reach are dropped instead, leaving the parameter, since a type that needs its
argument spelled out beats one whose default is undefined.

None = source couldn't be parsed, invalid, absurdly nested, or a parser panic.
caller decides how loudly to say so. */
pub fn exported_types(source: &str, frame: &RequireFrame) -> Option<Exports> {
    if bracket_depth(source) > MAX_NESTING_DEPTH {
        return None;
    }
    let source = source.to_string();
    let frame = frame.clone();
    std::thread::Builder::new()
        .name("luau-parse".to_string())
        .stack_size(PARSE_STACK_BYTES)
        .spawn(move || extract_types(&source, &frame))
        .ok()?
        .join()
        .ok()? // full_moon panic reads as "couldn't parse"
}

/** deepest `(){}[]` nesting in `source`. cheap over-approximation of the parser's
recursion depth. brackets inside strings/comments count too, which only ever
refuses more, never less. */
fn bracket_depth(source: &str) -> usize {
    let mut depth = 0usize;
    let mut deepest = 0;
    for byte in source.bytes() {
        match byte {
            b'(' | b'{' | b'[' => {
                depth += 1;
                deepest = deepest.max(depth);
            }
            b')' | b'}' | b']' => depth = depth.saturating_sub(1),
            _ => {}
        }
    }
    deepest
}

fn extract_types(source: &str, frame: &RequireFrame) -> Option<Exports> {
    let ast = full_moon::parse(source).ok()?;
    let mut visitor = SourceVisitor::default();
    visitor.visit_ast(&ast);
    Some(visitor.into_exports(frame))
}

/// one exported type, kept whole until the whole source has been read: whether a default survives depends on declarations further down.
struct Export {
    name: String,
    generics: Vec<Generic>,
}

struct Generic {
    /// `T`, or `T...` for a pack. what the use side spells either way.
    parameter: String,
    default: Option<Default>,
}

/// a generic's `= <type>`, with what that type reaches for.
struct Default {
    /// the type itself, right of the `=`.
    body: String,
    /// modules indexed for a type, `Types` in `Types.Default`.
    modules: Vec<String>,
    /// type names used bare, `Default` in `type X = Default`.
    names: Vec<String>,
}

/// everything one source says that bears on its re-export lines.
#[derive(Default)]
struct SourceVisitor {
    /// `local X = require("...")` and its `const` twin, in source order.
    requires: Vec<(String, String)>,
    /// every type the source declares, exported or not.
    declared: Vec<String>,
    /// the exported subset, the ones the link file restates.
    exported: Vec<String>,
    exports: Vec<Export>,
}

impl Visitor for SourceVisitor {
    fn visit_local_assignment(&mut self, node: &full_moon::ast::LocalAssignment) {
        self.bind(node.names(), node.expressions());
    }

    fn visit_const_assignment(&mut self, node: &full_moon::ast::luau::ConstAssignment) {
        self.bind(node.names(), node.expressions());
    }

    /// fires for exported declarations too, so this sees every type the source declares.
    fn visit_type_declaration(&mut self, node: &full_moon::ast::luau::TypeDeclaration) {
        self.declared.push(trimmed(node.type_name()));
    }

    fn visit_type_function(&mut self, node: &full_moon::ast::luau::TypeFunction) {
        self.declared.push(trimmed(node.function_name()));
    }

    fn visit_exported_type_declaration(&mut self, node: &ExportedTypeDeclaration) {
        let declaration = node.type_declaration();
        let name = trimmed(declaration.type_name());
        self.exported.push(name.clone());

        let mut generics = Vec::new();
        if let Some(declared) = declaration.generics() {
            for generic in declared.generics() {
                generics.push(Generic {
                    parameter: trimmed(generic.parameter()),
                    default: generic.default_type().map(|default| Default {
                        body: trimmed(default),
                        modules: referenced_modules(default),
                        names: referenced_names(default),
                    }),
                });
            }
        }
        self.exports.push(Export { name, generics });
    }

    fn visit_exported_type_function(&mut self, node: &ExportedTypeFunction) {
        let function = node.type_function();
        let name = trimmed(function.function_name());
        self.exported.push(name.clone());

        /* parameters are values here, `ty: type`, not type expressions, so
        they carry nothing that could need a module of its own */
        let generics = function
            .function_body()
            .parameters()
            .iter()
            .map(|parameter| Generic {
                parameter: trimmed(parameter),
                default: None,
            })
            .collect::<Vec<_>>();
        if generics.is_empty() {
            return; // no parameters, no type-declaration equivalent
        }
        self.exports.push(Export { name, generics });
    }
}

impl SourceVisitor {
    /// records `X = require("path")` pairs, positionally, ignoring every other binding.
    fn bind(
        &mut self,
        names: &full_moon::ast::punctuated::Punctuated<full_moon::tokenizer::TokenReference>,
        expressions: &full_moon::ast::punctuated::Punctuated<full_moon::ast::Expression>,
    ) {
        for (name, expression) in names.iter().zip(expressions.iter()) {
            if let Some(path) = required_path(expression) {
                self.requires.push((trimmed(name), path));
            }
        }
    }

    fn into_exports(self, frame: &RequireFrame) -> Exports {
        let mut exports = Exports::default();
        for export in &self.exports {
            let mut declared = Vec::new();
            let mut used = Vec::new();
            for generic in &export.generics {
                used.push(generic.parameter.clone());
                declared.push(self.restate(export, generic, frame, &mut exports));
            }
            exports.types.push(reexport(&export.name, &declared, &used));
        }
        exports
    }

    /** how one generic parameter is declared in the link file: `T`, `T = string`,
    or `T = <alias>` with the alias and its import recorded in `exports`. */
    fn restate(
        &self,
        export: &Export,
        generic: &Generic,
        frame: &RequireFrame,
        exports: &mut Exports,
    ) -> String {
        let parameter = &generic.parameter;
        let Some(default) = &generic.default else {
            return parameter.clone();
        };
        let Some(imports) = self.reachable(default, frame) else {
            // nothing here can point at what the default names, keep the parameter alone
            return parameter.clone();
        };
        if imports.is_empty() {
            return format!("{parameter} = {}", default.body);
        }

        /* Luau rejects a dotted type in default position outright, `<S =
        Types.Default>` is "Unknown type 'Types.Default'" however the module is
        bound, while a plain alias in the same spot resolves. so a default that
        goes through a module gets hoisted into one and named instead. */
        if hoisting_would_escape(export, generic, default) {
            return parameter.clone();
        }
        let alias = format!("__ember_{}_{}", export.name, parameter);
        exports
            .aliases
            .push(format!("type {alias} = {}", default.body));
        for import in imports {
            if !exports.imports.contains(&import) {
                exports.imports.push(import);
            }
        }
        format!("{parameter} = {alias}")
    }

    /** the imports a default needs before the link file can restate it, or
    None when something it names can't be reached from there at all. */
    fn reachable(&self, default: &Default, frame: &RequireFrame) -> Option<Vec<(String, String)>> {
        /* a name the source declares privately exists nowhere else. one it
        exports is restated in the link file, and anything it never declared
        is a built-in or one of this declaration's own parameters */
        if default
            .names
            .iter()
            .any(|name| self.declared.contains(name) && !self.exported.contains(name))
        {
            return None;
        }

        default
            .modules
            .iter()
            .map(|module| {
                if module == MODULE_BINDING {
                    return None; // would shadow the link file's own binding
                }
                let (_, raw) = self.requires.iter().find(|(name, _)| name == module)?;
                Some((module.clone(), resolve_import(raw, frame)?))
            })
            .collect()
    }
}

/** whether hoisting a default out of its declaration would leave something
behind. an alias sits at file scope, so one naming the declaration's own
parameters, `<T, U = Types.Of<T>>`, can't go there, and a pack default like
`...any` isn't a type an alias can hold at all. */
fn hoisting_would_escape(export: &Export, generic: &Generic, default: &Default) -> bool {
    generic.parameter.ends_with("...")
        || export.generics.iter().any(|other| {
            // a pack reads as `T` where it's used, `T...` where it's declared
            default
                .names
                .iter()
                .any(|name| name == other.parameter.trim_end_matches("..."))
        })
}

/// the string a `require("path")` call was given, for the plain call shapes. None for anything else, including instance requires.
fn required_path(expression: &full_moon::ast::Expression) -> Option<String> {
    use full_moon::ast::{Call, Expression, FunctionArgs, Prefix, Suffix};

    let Expression::FunctionCall(call) = expression else {
        return None;
    };
    let Prefix::Name(name) = call.prefix() else {
        return None;
    };
    if trimmed(name) != "require" {
        return None;
    }

    let mut suffixes = call.suffixes();
    let Some(Suffix::Call(Call::AnonymousCall(arguments))) = suffixes.next() else {
        return None;
    };
    // require("x").y is a value, not the module
    if suffixes.next().is_some() {
        return None;
    }

    let literal = match arguments {
        FunctionArgs::String(literal) => literal,
        FunctionArgs::Parentheses { arguments, .. } => {
            match arguments.iter().collect::<Vec<_>>()[..] {
                [Expression::String(literal)] => literal,
                _ => return None,
            }
        }
        _ => return None,
    };
    string_value(literal)
}

/// the contents of a Luau string token, quotes off. None for the long-bracket and escaped forms, which no require path needs.
fn string_value(token: &full_moon::tokenizer::TokenReference) -> Option<String> {
    let text = trimmed(token);
    let unquoted = text
        .strip_prefix('"')
        .and_then(|rest| rest.strip_suffix('"'))
        .or_else(|| {
            text.strip_prefix('\'')
                .and_then(|rest| rest.strip_suffix('\''))
        })?;
    (!unquoted.contains('\\')).then(|| unquoted.to_string())
}

/// modules a type expression indexes, `Types` in `Types.Default`.
fn referenced_modules(node: &full_moon::ast::luau::TypeInfo) -> Vec<String> {
    references(node).0
}

/// type names a type expression uses bare, `Default` in `type X = Default`.
fn referenced_names(node: &full_moon::ast::luau::TypeInfo) -> Vec<String> {
    references(node).1
}

fn references(node: &full_moon::ast::luau::TypeInfo) -> (Vec<String>, Vec<String>) {
    use full_moon::ast::luau::TypeInfo;
    use full_moon::visitors::Visit;

    #[derive(Default)]
    struct RefVisitor {
        modules: Vec<String>,
        names: Vec<String>,
    }

    impl Visitor for RefVisitor {
        fn visit_type_info(&mut self, node: &TypeInfo) {
            match node {
                TypeInfo::Module { module, .. } => self.modules.push(trimmed(module)),
                TypeInfo::Basic(name) => self.names.push(trimmed(name)),
                // the `map` of `map<K, V>`, the parameters visit on their own
                TypeInfo::Generic { base, .. } => self.names.push(trimmed(base)),
                TypeInfo::GenericPack { name, .. } => self.names.push(trimmed(name)),
                _ => {}
            }
        }
    }

    let mut visitor = RefVisitor::default();
    node.visit(&mut visitor);
    (visitor.modules, visitor.names)
}

/// AST nodes print with surrounding trivia, whitespace and comments. trim so re-exports stay on one line.
fn trimmed(node: impl std::fmt::Display) -> String {
    node.to_string().trim().to_string()
}

fn reexport(name: &str, declared: &[String], used: &[String]) -> String {
    let angled = |params: &[String]| {
        if params.is_empty() {
            String::new()
        } else {
            format!("<{}>", params.join(", "))
        }
    };
    format!(
        "export type {name}{} = module.{name}{}",
        angled(declared),
        angled(used)
    )
}

/** finds a package's entry point relative to its root, extensionless since Luau
string requires reject extensions. checked in order, ember.toml `[target].main`,
pesde.toml `[target].lib`, a Rojo default.project.json tree `$path`, then
conventional init file locations. */
pub fn entry_point(dir: &Path) -> Option<String> {
    if let Some(main) = toml_string(dir, "ember.toml", &["target", "main"]) {
        return Some(normalize_entry(&main));
    }
    if let Some(lib) = toml_string(dir, "pesde.toml", &["target", "lib"]) {
        return Some(normalize_entry(&lib));
    }
    if let Some(path) = fs::read_to_string(dir.join(PROJECT_FILE))
        .ok()
        .and_then(|text| serde_json::from_str::<serde_json::Value>(&text).ok())
        .and_then(|json| project_tree_path(json.get("tree")?))
    {
        return Some(normalize_entry(&path));
    }

    for candidate in [
        "init.luau",
        "init.lua",
        "src/init.luau",
        "src/init.lua",
        "lib/init.luau",
        "lib/init.lua",
    ] {
        if dir.join(candidate).exists() {
            return Some(normalize_entry(candidate));
        }
    }
    None
}

/** the folder a Rojo tree mounts as the package. usually the root's own
`$path`, but `rojo::mirror_disk_layout` re-nests that under folders named
for the require path, so a single chain of them is followed down to the
`$path` it ends at. the entry reads the same before and after the rewrite.

only plain folders are followed. a package shipping a place-style project,
a DataModel of services say, names no single entry, and guessing one from
whatever it mounts first would be worse than falling through to the
conventional locations. */
fn project_tree_path(tree: &serde_json::Value) -> Option<String> {
    let mut node = tree;
    loop {
        if let Some(path) = node.get("$path").and_then(serde_json::Value::as_str) {
            return Some(path.to_string());
        }
        let object = node.as_object()?;
        match object.get("$className").and_then(serde_json::Value::as_str) {
            None | Some("Folder") => {}
            Some(_) => return None,
        }
        // $className and friends describe the node, anything else is a child
        let mut children: Vec<_> = object
            .iter()
            .filter(|(key, _)| !key.starts_with('$'))
            .collect();
        /* `rojo::mount_nested_packages` adds `packages` to this very file
        between two reads of this function, so discount it and the entry
        reads the same either side of the mount. not when it's all there is
        though: a tree that really does mount a folder by that name still
        names an entry, and the mount refuses such a tree anyway */
        if children.len() > 1 {
            children.retain(|(key, _)| key.as_str() != PACKAGES_DIR);
        }
        let mut children = children.into_iter();
        let (_, only) = children.next()?;
        if children.next().is_some() {
            // several children, no single folder is "the package"
            return None;
        }
        node = only;
    }
}

/** folders a guess never looks in. output roots embr or the package itself
wrote, and the places a repo keeps code that isn't the library. */
const GUESS_SKIPS: &[&str] = &[
    ".git",
    ".ember",
    // a store left by an install from before the ember rename
    ".lpm",
    PACKAGES_DIR,
    "node_modules",
    "test",
    "tests",
    "spec",
    "specs",
    "example",
    "examples",
    "docs",
];

/** last resort for a package that names no entry point anywhere, `synttx/vow`
being one: a single `src/vow.luau` with no ember.toml, pesde.toml, project file
or init file to say so.

two things are worth guessing from. a package that ships exactly one Luau
file means that file, whatever it's called. one that ships several, but one
named after the package, means that one, which is the same convention Rojo
users get from mounting a folder. anything less clear stays None: a wrong
link file is worse than none, and `[dependencies]` can state an `entry`
outright, see [`Dependency::entry`].

test and example folders are skipped, so a library with a `tests/` beside it
still reads as the one file it is. */
pub fn guess_entry(dir: &Path) -> Option<String> {
    let mut files = Vec::new();
    collect_sources(dir, dir, &mut files).ok()?;

    if let [only] = files.as_slice() {
        return Some(normalize_entry(only));
    }

    let package = package_name(dir)?;
    let short = package
        .rsplit_once('/')
        .map_or(package.as_str(), |(_, s)| s);
    let mut named = files.iter().filter(|path| {
        Path::new(path)
            .file_stem()
            .and_then(|stem| stem.to_str())
            .is_some_and(|stem| stem.eq_ignore_ascii_case(short))
    });
    let first = named.next()?;
    // two files answering to the same name is not a guess, it's a coin flip
    named.next().is_none().then(|| normalize_entry(first))
}

/// every Luau file under `dir`, package-relative with forward slashes, skipping [`GUESS_SKIPS`].
fn collect_sources(root: &Path, dir: &Path, found: &mut Vec<String>) -> Result<(), Error> {
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if GUESS_SKIPS
            .iter()
            .any(|skip| name.eq_ignore_ascii_case(skip))
        {
            continue;
        }

        let path = entry.path();
        if entry.file_type()?.is_dir() {
            collect_sources(root, &path, found)?;
        } else if matches!(
            path.extension().and_then(|ext| ext.to_str()),
            Some("luau" | "lua")
        ) {
            let relative = path.strip_prefix(root).unwrap_or(&path);
            found.push(
                relative
                    .components()
                    .map(|part| part.as_os_str().to_string_lossy())
                    .collect::<Vec<_>>()
                    .join("/"),
            );
        }
    }
    Ok(())
}

/// what a package calls itself, from whichever manifest it shipped.
fn package_name(dir: &Path) -> Option<String> {
    ["ember.toml", "pesde.toml", "wally.toml"]
        .into_iter()
        .find_map(|file| toml_string(dir, file, &["package", "name"]))
}

/** reads an extracted package's own manifest for its environment. ember.toml
`[target].environment` first, then pesde.toml's, then wally.toml
`[package].realm`, the last two translated. */
pub fn environment(dir: &Path) -> Option<Environment> {
    if let Some(name) = toml_string(dir, "ember.toml", &["target", "environment"]) {
        return Environment::from_embr(&name).ok();
    }
    if let Some(name) = toml_string(dir, "pesde.toml", &["target", "environment"]) {
        return Environment::from_pesde(&name).ok();
    }
    if let Some(realm) = toml_string(dir, "wally.toml", &["package", "realm"]) {
        return Environment::from_wally_realm(&realm).ok();
    }
    None
}

/** an extracted package's declared runtime dependencies, as (alias,
lowercased package name) pairs, what install's nested-link pass consumes.
the first manifest with a matching table wins, same priority order as the
other readers here. ember.toml reads each entry's `name` key, pesde.toml
`name` or `wally` for wally-sourced entries, wally.toml
`alias = "scope/name@req"`. wally splits runtime deps by realm, so its
[server-dependencies] count too. the resolver installs them, wally.rs
chains both tables, and a server-realm package like lyra declares ALL its
deps there. reading only [dependencies] starved those packages of nested
links and left their escape requires unrewritten. dev/peer tables stay
out. missing or unparseable manifests read as no dependencies, same stance
as `toml_string`. */
pub fn declared_dependencies(dir: &Path) -> Vec<(String, String)> {
    /// how one manifest flavor names the package a dependency entry means
    type DependencyName = fn(&toml::Value) -> Option<String>;

    let manifests: [(&str, &[&str], DependencyName); 3] = [
        ("ember.toml", &["dependencies"], |entry| {
            Some(entry.get("name")?.as_str()?.to_string())
        }),
        ("pesde.toml", &["dependencies"], |entry| {
            let name = entry.get("name").or_else(|| entry.get("wally"))?.as_str()?;
            // pesde serializes wally package names with a "wally#" prefix
            Some(name.strip_prefix("wally#").unwrap_or(name).to_string())
        }),
        (
            "wally.toml",
            &["dependencies", "server-dependencies"],
            |entry| {
                let spec = entry.as_str()?;
                Some(
                    spec.split_once('@')
                        .map_or(spec, |(name, _)| name)
                        .to_string(),
                )
            },
        ),
    ];

    for (file, tables, dependency_name) in manifests {
        let Some(parsed) = fs::read_to_string(dir.join(file))
            .ok()
            .and_then(|text| text.parse::<toml::Value>().ok())
        else {
            continue;
        };
        let found: Vec<_> = tables
            .iter()
            .filter_map(|table| parsed.get(*table).and_then(toml::Value::as_table))
            .collect();
        if found.is_empty() {
            continue;
        }
        return found
            .into_iter()
            .flat_map(|table| table.iter())
            // entries this flavor can't name, like workspace specifiers, are skipped
            .filter_map(|(alias, entry)| {
                Some((alias.clone(), dependency_name(entry)?.trim().to_lowercase()))
            })
            .collect();
    }
    Vec::new()
}

/// archives sometimes wrap everything in one top-level folder, GitHub release tarballs do. unwrap so package files sit at the root.
pub fn flatten_single_dir(dir: &Path) -> Result<(), Error> {
    let entries: Vec<_> = fs::read_dir(dir)?.collect::<Result<_, _>>()?;
    let [only] = entries.as_slice() else {
        return Ok(());
    };
    if !only.file_type()?.is_dir() {
        return Ok(());
    }

    let inner = only.path();
    for entry in fs::read_dir(&inner)? {
        let entry = entry?;
        fs::rename(entry.path(), dir.join(entry.file_name()))?;
    }
    fs::remove_dir(inner)?;
    Ok(())
}

/// string at `keys` in one of the package's manifests, if the file exists, parses, and has it. missing is the normal case, so no errors here.
fn toml_string(dir: &Path, file: &str, keys: &[&str]) -> Option<String> {
    let mut value: toml::Value = fs::read_to_string(dir.join(file)).ok()?.parse().ok()?;
    for key in keys {
        value = value.get(key)?.clone();
    }
    value.as_str().map(str::to_string)
}

/// normalizes an entry path for a string require. forward slashes, no leading "./", no .luau/.lua extension, a bare folder resolves its init file.
pub(crate) fn normalize_entry(path: &str) -> String {
    let path = path.replace('\\', "/");
    let path = path.trim_start_matches("./").trim_matches('/');
    let path = path
        .strip_suffix(".luau")
        .or_else(|| path.strip_suffix(".lua"))
        .unwrap_or(path);
    /* init files are the folder's module, mod.rs style, so the require has to
    point at the folder. pesde manifests like lib = "/src/init.luau" hit this. */
    let path = path.strip_suffix("/init").unwrap_or(path);
    if path == "init" { "" } else { path }.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_package(dir: &Path, file: &str, contents: &str) {
        fs::create_dir_all(dir).unwrap();
        fs::write(dir.join(file), contents).unwrap();
    }

    /// exports that need no imports of their own, which most sources are.
    fn type_lines<const N: usize>(lines: [&str; N]) -> Exports {
        Exports {
            types: lines.iter().map(|line| line.to_string()).collect(),
            ..Exports::default()
        }
    }

    #[test]
    fn detects_environment_from_manifests() {
        let base = std::env::temp_dir().join("embr-test-detect-env");
        let _ = fs::remove_dir_all(&base);

        let embr = base.join("embr");
        write_package(&embr, "ember.toml", "[target]\nenvironment = \"lune\"");
        assert_eq!(environment(&embr), Some(Environment::Lune));

        let pesde = base.join("pesde");
        write_package(&pesde, "pesde.toml", "[target]\nenvironment = \"roblox\"");
        assert_eq!(environment(&pesde), Some(Environment::Roblox));

        let wally = base.join("wally");
        write_package(&wally, "wally.toml", "[package]\nrealm = \"server\"");
        assert_eq!(environment(&wally), Some(Environment::Server));

        let none = base.join("none");
        fs::create_dir_all(&none).unwrap();
        assert_eq!(environment(&none), None);

        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn embr_manifest_takes_priority_over_wally() {
        let base = std::env::temp_dir().join("embr-test-detect-priority");
        let _ = fs::remove_dir_all(&base);

        write_package(&base, "wally.toml", "[package]\nrealm = \"server\"");
        write_package(&base, "ember.toml", "[target]\nenvironment = \"luau\"");
        assert_eq!(environment(&base), Some(Environment::Luau));

        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn reads_declared_dependencies_per_manifest_flavor() {
        let base = std::env::temp_dir().join("embr-test-declared-deps");
        let _ = fs::remove_dir_all(&base);

        // ember.toml reads `name` keys, lowercased. entries without one, like
        // workspace specifiers, are skipped.
        let embr = base.join("embr");
        write_package(
            &embr,
            "ember.toml",
            "[dependencies]\ncore = { name = \"Chief/Core\", version = \"^0.2.0\" }\n\
             local = { workspace = \"chief/dev\", version = \"^\" }\n",
        );
        assert_eq!(
            declared_dependencies(&embr),
            [("core".to_string(), "chief/core".to_string())]
        );

        /* pesde.toml reads `name`, or `wally` for wally-sourced entries,
        which pesde serializes with a "wally#" prefix the install set
        doesn't carry */
        let pesde = base.join("pesde");
        write_package(
            &pesde,
            "pesde.toml",
            "[dependencies]\nhello = { name = \"pesde/hello\", version = \"^1\" }\n\
             promise = { wally = \"wally#evaera/Promise\", version = \"^4\" }\n",
        );
        let mut deps = declared_dependencies(&pesde);
        deps.sort();
        assert_eq!(
            deps,
            [
                ("hello".to_string(), "pesde/hello".to_string()),
                ("promise".to_string(), "evaera/promise".to_string()),
            ]
        );

        // wally.toml reads `alias = "scope/name@req"`, req stripped.
        let wally = base.join("wally");
        write_package(
            &wally,
            "wally.toml",
            "[dependencies]\nPromise = \"evaera/promise@^4.0.0\"\n\n\
             [dev-dependencies]\nTestEZ = \"roblox/testez@^0.4\"\n",
        );
        assert_eq!(
            declared_dependencies(&wally),
            [("Promise".to_string(), "evaera/promise".to_string())]
        );

        /* wally splits runtime deps by realm, a server package like lyra
        puts ALL its deps under [server-dependencies]. both tables count,
        dev still doesn't */
        let server = base.join("wally-server");
        write_package(
            &server,
            "wally.toml",
            "[package]\nrealm = \"server\"\n\n\
             [dependencies]\nSignal = \"a/signal@^1\"\n\n\
             [server-dependencies]\nPromise = \"evaera/promise@4.0.0\"\n\
             GreenTea = \"corecii/greentea@0.4.11\"\n\n\
             [dev-dependencies]\nJest = \"jsdotlua/jest@3.10.0\"\n",
        );
        let mut server_deps = declared_dependencies(&server);
        server_deps.sort();
        assert_eq!(
            server_deps,
            [
                ("GreenTea".to_string(), "corecii/greentea".to_string()),
                ("Promise".to_string(), "evaera/promise".to_string()),
                ("Signal".to_string(), "a/signal".to_string()),
            ]
        );

        // no manifests at all -> no dependencies.
        let none = base.join("none");
        fs::create_dir_all(&none).unwrap();
        assert!(declared_dependencies(&none).is_empty());

        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn declared_dependencies_follow_reader_priority() {
        let base = std::env::temp_dir().join("embr-test-declared-deps-priority");
        let _ = fs::remove_dir_all(&base);

        // an ember.toml [dependencies] table wins outright over wally.toml...
        write_package(
            &base,
            "ember.toml",
            "[dependencies]\ncore = { name = \"acme/core\", version = \"^\" }\n",
        );
        write_package(&base, "wally.toml", "[dependencies]\nOther = \"a/b@^1\"\n");
        assert_eq!(
            declared_dependencies(&base),
            [("core".to_string(), "acme/core".to_string())]
        );

        // ...but an ember.toml without one falls through to the next manifest.
        fs::write(
            base.join("ember.toml"),
            "[package]\nname = \"acme/thing\"\n",
        )
        .unwrap();
        assert_eq!(
            declared_dependencies(&base),
            [("Other".to_string(), "a/b".to_string())]
        );

        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn flattens_single_wrapper_directory() {
        let base = std::env::temp_dir().join("embr-test-flatten");
        let _ = fs::remove_dir_all(&base);
        let wrapper = base.join("pkg-1.0.0");
        fs::create_dir_all(wrapper.join("src")).unwrap();
        fs::write(wrapper.join("init.luau"), "return {}").unwrap();

        flatten_single_dir(&base).unwrap();
        assert!(base.join("init.luau").exists());
        assert!(base.join("src").exists());
        assert!(!base.join("pkg-1.0.0").exists());

        let _ = fs::remove_dir_all(&base);
    }

    /** synttx/vow's shape: one `src/vow.luau` and nothing that names it, no
    ember.toml, no pesde.toml, no project file, no init file. detection has
    nothing to go on, so the guess is all that stands between it and no link
    file at all. */
    #[test]
    fn guesses_an_entry_from_what_a_package_ships() {
        let base = std::env::temp_dir().join("embr-test-guess-entry");
        let _ = fs::remove_dir_all(&base);

        // the only Luau file it ships, whatever it happens to be called
        let vow = base.join("vow");
        write_package(&vow, "wally.toml", "[package]\nname = \"synttx/vow\"\n");
        write_package(&vow.join("src"), "vow.luau", "return {}");
        assert_eq!(entry_point(&vow), None, "nothing declares it");
        assert_eq!(guess_entry(&vow).as_deref(), Some("src/vow"));

        // several files, but one carries the package's own name
        let named = base.join("named");
        write_package(&named, "wally.toml", "[package]\nname = \"synttx/vow\"\n");
        write_package(&named.join("src"), "Vow.luau", "return {}");
        write_package(&named.join("src"), "types.luau", "return {}");
        assert_eq!(
            guess_entry(&named).as_deref(),
            Some("src/Vow"),
            "matched case-insensitively, Roblox code capitalizes"
        );

        // a test folder beside the library still leaves one file
        let tested = base.join("tested");
        write_package(&tested.join("src"), "vow.luau", "return {}");
        write_package(&tested.join("tests"), "vow.spec.luau", "return {}");
        assert_eq!(guess_entry(&tested).as_deref(), Some("src/vow"));

        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn refuses_to_guess_when_the_answer_is_a_coin_flip() {
        let base = std::env::temp_dir().join("embr-test-guess-refuses");
        let _ = fs::remove_dir_all(&base);

        // two files, neither named for the package
        let unclear = base.join("unclear");
        write_package(&unclear, "wally.toml", "[package]\nname = \"acme/thing\"\n");
        write_package(&unclear.join("src"), "one.luau", "return {}");
        write_package(&unclear.join("src"), "two.luau", "return {}");
        assert_eq!(guess_entry(&unclear), None);

        // two that answer to the same name
        let twice = base.join("twice");
        write_package(&twice, "wally.toml", "[package]\nname = \"acme/thing\"\n");
        write_package(&twice, "thing.luau", "return {}");
        write_package(&twice.join("src"), "thing.luau", "return {}");
        assert_eq!(guess_entry(&twice), None);

        // and nothing to guess from at all
        let empty = base.join("empty");
        write_package(&empty, "README.md", "no code here");
        assert_eq!(guess_entry(&empty), None);

        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn detects_entry_points_in_priority_order() {
        let base = std::env::temp_dir().join("embr-test-detect-entry");
        let _ = fs::remove_dir_all(&base);

        // ember.toml main wins over everything.
        let a = base.join("a");
        write_package(&a, "ember.toml", "[target]\nmain = \"src/main.luau\"");
        write_package(&a, "init.luau", "");
        assert_eq!(entry_point(&a).as_deref(), Some("src/main"));

        // pesde.toml lib next.
        let b = base.join("b");
        write_package(&b, "pesde.toml", "[target]\nlib = \"lib.luau\"");
        assert_eq!(entry_point(&b).as_deref(), Some("lib"));

        // Rojo tree path, a folder whose init file resolves at require time.
        let c = base.join("c");
        write_package(
            &c,
            "default.project.json",
            r#"{"name": "pkg", "tree": {"$path": "src"}}"#,
        );
        assert_eq!(entry_point(&c).as_deref(), Some("src"));

        // conventional fallbacks.
        let d = base.join("d");
        write_package(&d.join("src"), "init.lua", "");
        assert_eq!(entry_point(&d).as_deref(), Some("src"));

        let e = base.join("e");
        fs::create_dir_all(&e).unwrap();
        assert_eq!(entry_point(&e), None);

        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn entry_point_survives_the_rojo_rewrite() {
        /* install mirrors a shipped project file onto the disk layout after
        reading the entry. later passes, nested links, read it again, so
        both shapes have to answer the same */
        let base = std::env::temp_dir().join("embr-test-entry-after-rewrite");
        let _ = fs::remove_dir_all(&base);
        let rewrite = |dir: &Path| {
            crate::project::rojo::mirror_disk_layout(dir, &mut |message| {
                panic!("unexpected warning: {message}")
            })
        };

        // a folder mount, and a file mount that resolves to the same entry
        for path in ["lib", "lib/init.luau"] {
            let package = base.join(path.replace('/', "_"));
            write_package(
                &package,
                PROJECT_FILE,
                &format!(r#"{{"name": "promise", "tree": {{"$path": "{path}"}}}}"#),
            );
            write_package(&package.join("lib"), "init.luau", "return {}");

            assert_eq!(entry_point(&package).as_deref(), Some("lib"), "for {path}");
            rewrite(&package);
            assert_eq!(
                entry_point(&package).as_deref(),
                Some("lib"),
                "after rewriting {path}"
            );
        }

        /* a place-style project names no single entry, so it keeps falling
        through to the conventional locations rather than mounting whatever
        it happens to reach first */
        let place = base.join("place");
        write_package(
            &place,
            PROJECT_FILE,
            r#"{"name": "x", "tree": {"$className": "DataModel",
                "ReplicatedStorage": {"Packages": {"$path": "Packages"}}}}"#,
        );
        assert_eq!(entry_point(&place), None);
        write_package(&place.join("src"), "init.luau", "return {}");
        assert_eq!(entry_point(&place).as_deref(), Some("src"));

        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn entry_point_survives_the_packages_mount() {
        /* the nested-link pass mounts `packages` into that same file, and a
        dependent reads this entry back afterwards to link against it. a
        package that is both dependency and dependent is read on both sides
        of its own mount, so the answer has to hold across it */
        let base = std::env::temp_dir().join("embr-test-entry-after-mount");
        let _ = fs::remove_dir_all(&base);

        /* `out` is roblox-ts shaped and `Maid.lua` a single file package:
        entries the conventional fallbacks can't recover */
        for (path, entry) in [("lib", "lib"), ("out", "out"), ("Maid.lua", "Maid")] {
            let package = base.join(path.replace(['/', '.'], "_"));
            write_package(
                &package,
                PROJECT_FILE,
                &format!(r#"{{"name": "pkg", "tree": {{"$path": "{path}"}}}}"#),
            );
            write_package(
                &package.join("packages/roblox"),
                "Promise.luau",
                "return nil",
            );
            let panicking = &mut |message| panic!("unexpected warning: {message}");

            crate::project::rojo::mirror_disk_layout(&package, panicking);
            assert_eq!(entry_point(&package).as_deref(), Some(entry), "for {path}");
            crate::project::rojo::mount_nested_packages(&package, panicking);

            // the mount really happened, so the invariant below is tested
            let project: serde_json::Value =
                serde_json::from_str(&fs::read_to_string(package.join(PROJECT_FILE)).unwrap())
                    .unwrap();
            assert_eq!(
                project["tree"]["packages"]["$path"], "packages",
                "for {path}"
            );
            assert_eq!(
                entry_point(&package).as_deref(),
                Some(entry),
                "after mounting packages into {path}"
            );
        }

        /* a package whose source folder really is called `packages`. the
        mount refuses that tree rather than clobber the child, so the entry
        has to keep reading through it instead of discounting it as ours */
        let own = base.join("own_packages");
        write_package(
            &own,
            PROJECT_FILE,
            r#"{"name": "pkg", "tree": {"$path": "packages"}}"#,
        );
        write_package(&own.join("packages"), "init.luau", "return {}");
        let panicking = &mut |message| panic!("unexpected warning: {message}");

        crate::project::rojo::mirror_disk_layout(&own, panicking);
        assert_eq!(entry_point(&own).as_deref(), Some("packages"));
        crate::project::rojo::mount_nested_packages(&own, panicking);
        assert_eq!(
            entry_point(&own).as_deref(),
            Some("packages"),
            "a tree that mounts a folder of its own by that name"
        );

        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn link_files_require_the_stored_package() {
        assert_eq!(
            link_contents("evaera_promise", "lib", &Exports::default()),
            "return require(\"./.ember/evaera_promise/lib\")\n"
        );
        assert_eq!(
            link_contents(
                "evaera_promise",
                "lib",
                &type_lines(["export type Status = module.Status"])
            ),
            "local module = require(\"./.ember/evaera_promise/lib\")\n\
             export type Status = module.Status\n\
             return module\n"
        );
        assert_eq!(normalize_entry("./src\\init.luau"), "src".to_string());
        assert_eq!(normalize_entry("lib.lua"), "lib".to_string());
        assert_eq!(normalize_entry("src"), "src".to_string());
    }

    #[test]
    fn absurdly_nested_sources_are_refused_not_crashed() {
        /* full_moon recurses per nesting level. past the guard's ceiling the source
        is refused before the parser can eat the stack. this once aborted the whole
        install with a stack overflow. */
        let depth = 2000;
        let deep = format!(
            "export type Deep = {}number{}\nreturn {{}}\n",
            "{ a: ".repeat(depth),
            " }".repeat(depth)
        );
        assert_eq!(exported_types(&deep, &RequireFrame::init("")), None);

        // deep-but-sane nesting still parses on the roomy parser thread.
        let sane = format!(
            "export type Deep = {}number{}\nreturn {{}}\n",
            "{ a: ".repeat(100),
            " }".repeat(100)
        );
        assert_eq!(
            exported_types(&sane, &RequireFrame::init(""))
                .unwrap()
                .types,
            ["export type Deep = module.Deep"]
        );

        assert_eq!(bracket_depth("({[]})"), 3);
        assert_eq!(bracket_depth("}}}((("), 3);
        assert_eq!(bracket_depth("plain"), 0);
    }

    /// the deepest source the guard lets through. it refuses `>` the ceiling, so this is it.
    #[cfg(not(debug_assertions))]
    fn worst_case_source() -> String {
        format!(
            "export type Deep = {}number{}\nreturn {{}}\n",
            "{ a: ".repeat(MAX_NESTING_DEPTH),
            " }".repeat(MAX_NESTING_DEPTH)
        )
    }

    /** how much of the parse thread's 64 MiB the worst case actually needs.

    measured by bisection on this fixture, and the two profiles are nothing
    alike.

      release (lto = "fat", opt-level = "s")   aborts at 8 MiB, passes at 10
      release (lto = "thin", opt-level = 3)    aborts at 8 MiB, passes at 10
      dev (unoptimized)                        aborts at 32 MiB, passes at 48

    so the profile change did not move the per-frame cost, and the shipped
    binary has roughly 6x headroom, not the ~135 KiB-per-level a naive
    PARSE_STACK_BYTES / MAX_NESTING_DEPTH division suggests. An unoptimized
    build wants closer to five times that, leaving only ~1.3x, which is why
    both of the max-depth tests are release-only. run under `cargo test`
    they sit near enough to the edge that a slightly different toolchain
    tips them over, and stack exhaustion does not fail politely.

    that is also the whole reason this one runs on a deliberately SMALL stack
    rather than the production 64 MiB. At 64 a regression would have to
    quadruple per-frame cost before anything noticed, and the first sign would
    be a user's install aborting. at 16 the same regression trips here first,
    with ~1.6x of slack so ordinary codegen churn is not noise.

    when it does trip, the symptom is the whole test binary dying with
    STATUS_STACK_OVERFLOW and no per-test attribution, because that is what
    stack exhaustion does, see MAX_NESTING_DEPTH. re-run the bisection above
    before touching either constant. */
    #[cfg(not(debug_assertions))]
    #[test]
    fn parser_stack_stays_within_a_sixth_of_its_budget() {
        let deep = worst_case_source();
        let outcome = std::thread::Builder::new()
            .stack_size(16 * 1024 * 1024)
            .spawn(move || extract_types(&deep, &RequireFrame::init("")))
            .expect("probe thread")
            .join();
        assert_eq!(
            outcome.expect("parser overflowed a 16 MiB stack"),
            Some(type_lines(["export type Deep = module.Deep"]))
        );
    }

    /** the guard's own ceiling, parsed for real.

    `absurdly_nested_sources_are_refused_not_crashed` covers depth 2000, refused
    before the parser runs, and depth 100, which parses comfortably. that leaves
    the case that actually costs the most stack untested, one notch under
    MAX_NESTING_DEPTH, so it clears the guard and then recurses all the way down.

    that gap is worth closing because the margin is a budget with a compiler on
    the other side of it, PARSE_STACK_BYTES against however many bytes per
    recursion level full_moon compiles down to, and inlining decisions move the
    second number. lto, opt-level, and a toolchain bump all change inlining, and
    the failure mode is a stack overflow, which aborts the process and cannot be
    caught, see MAX_NESTING_DEPTH's comment. run this under `--release` as well
    as dev. a dev-profile pass proves nothing about a release inlining change. */
    /* release-only for the same reason as the canary above. an unoptimized
    build needs ~48 MiB of the 64 to parse this, and a margin that thin turns
    an unrelated toolchain bump into an aborted test binary. the guard logic
    itself stays covered in every profile by
    `absurdly_nested_sources_are_refused_not_crashed`, which works at depths
    where dev has room to spare, 2000 refused, 100 parsed. */
    #[cfg(not(debug_assertions))]
    #[test]
    fn nesting_at_the_ceiling_still_parses() {
        let deep = worst_case_source();
        /* pinned, not bounded. `<= MAX_NESTING_DEPTH` would also hold for a
        fixture nested five levels deep, and the whole point is to sit exactly
        where the guard stops refusing. it refuses `>`, so the ceiling itself
        gets through and is the most expensive input embr will ever parse */
        assert_eq!(bracket_depth(&deep), MAX_NESTING_DEPTH);
        assert_eq!(
            exported_types(&deep, &RequireFrame::init(""))
                .unwrap()
                .types,
            ["export type Deep = module.Deep"]
        );
    }

    #[test]
    fn extracts_exported_types_for_reexport() {
        let source = r#"
            local private = {}
            type Hidden = { secret: boolean } -- not exported: stays hidden
            export type Status = "Started" | "Resolved"
            export type Promise<T> = { andThen: (Promise<T>, (T) -> ()) -> Promise<T> }
            export type Result<T, E = string> = { ok: T?, err: E? }
            export type Pack<T...> = (T...) -> ()
            return private
        "#;

        assert_eq!(
            exported_types(source, &RequireFrame::init(""))
                .unwrap()
                .types,
            [
                "export type Status = module.Status",
                "export type Promise<T> = module.Promise<T>",
                // declaration keeps the default, use side drops it.
                "export type Result<T, E = string> = module.Result<T, E>",
                "export type Pack<T...> = module.Pack<T...>",
            ]
        );

        assert_eq!(
            exported_types("return {}", &RequireFrame::init("")).unwrap(),
            Exports::default()
        );
        // non-Luau input parses to nothing, not bad re-exports.
        assert_eq!(exported_types("local = = =", &RequireFrame::init("")), None);
    }

    /** the shape issue #39 reported: a package whose types live in a second
    module, reached through a require the entry point makes.

    two things have to happen for `Signal` to mean anything in a consumer.
    the module the default comes from has to be required here too, and the
    default itself has to be hoisted into an alias, because Luau rejects a
    dotted type in default position however the module is bound. */
    #[test]
    fn defaults_reaching_into_another_module_bring_it_along() {
        let source = r#"
            local Types = require("@self/Types")
            export type Signal<Signature = Types.DefaultSignature> = Types.Signal<Signature>
            export type Wrap<Other> = Types.Wrap<Other>
            return {}
        "#;

        // the entry is src/init.luau, so `@self/` is its own folder
        let exports = exported_types(source, &RequireFrame::init("src")).unwrap();
        assert_eq!(
            exports.imports,
            [("Types".to_string(), "src/Types".to_string())]
        );
        assert_eq!(
            exports.aliases,
            ["type __ember_Signal_Signature = Types.DefaultSignature"]
        );
        assert_eq!(
            exports.types,
            [
                "export type Signal<Signature = __ember_Signal_Signature> = module.Signal<Signature>",
                // no default, nothing to hoist, and no second import either
                "export type Wrap<Other> = module.Wrap<Other>",
            ]
        );

        assert_eq!(
            link_contents("nowoshire_namedsignal", "src", &exports),
            "local module = require(\"./.ember/nowoshire_namedsignal/src\")\n\
             local Types = require(\"./.ember/nowoshire_namedsignal/src/Types\")\n\
             type __ember_Signal_Signature = Types.DefaultSignature\n\
             export type Signal<Signature = __ember_Signal_Signature> = module.Signal<Signature>\n\
             export type Wrap<Other> = module.Wrap<Other>\n\
             return module\n"
        );
    }

    /** a default the link file can't point at costs its default, not the type.
    `Signal<T>` still checks; only the bare `Signal` stops resolving, which
    beats a link file naming something that isn't there. */
    #[test]
    fn unreachable_defaults_leave_the_parameter_alone() {
        let cases = [
            // a type the source keeps to itself
            (
                r#"
                type Private = () -> ()
                export type Signal<S = Private> = { fire: S }
                return {}
                "#,
                "export type Signal<S> = module.Signal<S>",
            ),
            // a module embr can't resolve to a path of its own
            (
                r#"
                local Task = require("@lune/task")
                export type Handle<S = Task.Handle> = S
                return {}
                "#,
                "export type Handle<S> = module.Handle<S>",
            ),
            // an alias would have to hold `T`, which only exists inside the declaration
            (
                r#"
                local Types = require("@self/Types")
                export type Of<T, U = Types.Wrap<T>> = { value: U }
                return {}
                "#,
                "export type Of<T, U> = module.Of<T, U>",
            ),
        ];

        for (source, expected) in cases {
            let exports = exported_types(source, &RequireFrame::init("src")).unwrap();
            assert_eq!(exports.types, [expected]);
            // a dropped default drags in nothing
            assert!(exports.imports.is_empty(), "{source}");
            assert!(exports.aliases.is_empty(), "{source}");
        }
    }

    /** which folder a require resolves against, the rule
    `requires::parse_chain` writes chains with, read backwards. */
    #[test]
    fn require_frames_follow_the_init_file_rule() {
        let init = RequireFrame::init("src");
        // an init file IS its folder: its children need @self, its siblings sit above it
        assert_eq!(
            resolve_import("@self/Types", &init).as_deref(),
            Some("src/Types")
        );
        assert_eq!(resolve_import("./Types", &init).as_deref(), Some("Types"));
        assert_eq!(resolve_import("../Types", &init), None); // out of the package

        // a plain file has both frames where it sits
        let file = RequireFrame::file("src");
        assert_eq!(
            resolve_import("./Types", &file).as_deref(),
            Some("src/Types")
        );
        assert_eq!(
            resolve_import("@self/Types", &file).as_deref(),
            Some("src/Types")
        );
        assert_eq!(resolve_import("../Types", &file).as_deref(), Some("Types"));

        // a root init has no siblings inside the package at all
        assert_eq!(resolve_import("./Types", &RequireFrame::init("")), None);
        assert_eq!(
            resolve_import("@self/lib/Types", &RequireFrame::init("")).as_deref(),
            Some("lib/Types")
        );

        // nothing else is a path this side can follow
        assert_eq!(resolve_import("@lune/task", &init), None);
        assert_eq!(resolve_import("Types", &init), None);
        assert_eq!(resolve_import("@self/../../escape", &init), None);
    }

    /// which frames a package's entry actually gets, read off the file it resolves to.
    #[test]
    fn frames_come_from_the_resolved_entry_file() {
        let base = std::env::temp_dir().join("embr-test-require-frames");
        let _ = fs::remove_dir_all(&base);

        let init = base.join("init-style");
        write_package(&init.join("src"), "init.luau", "return {}");
        assert_eq!(RequireFrame::of(&init, "src"), RequireFrame::init("src"));

        let flat = base.join("file-style");
        write_package(&flat.join("src"), "lib.luau", "return {}");
        assert_eq!(
            RequireFrame::of(&flat, "src/lib"),
            RequireFrame::file("src")
        );

        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn reexports_exported_type_functions_with_parameters() {
        let source = r#"
            export type function Partial(ty)
                return ty
            end
            export type function Constant()
                return types.singleton("x")
            end
            return {}
        "#;

        // parameterless type functions can't be restated as a declaration.
        assert_eq!(
            exported_types(source, &RequireFrame::init(""))
                .unwrap()
                .types,
            ["export type Partial<ty> = module.Partial<ty>"]
        );
    }

    #[test]
    fn entry_source_resolves_like_a_string_require() {
        let base = std::env::temp_dir().join("embr-test-entry-source");
        let _ = fs::remove_dir_all(&base);

        write_package(&base, "lib.luau", "return {}");
        assert_eq!(entry_source(&base, "lib"), Some(base.join("lib.luau")));

        write_package(&base.join("src"), "init.lua", "return {}");
        assert_eq!(entry_source(&base, "src"), Some(base.join("src/init.lua")));
        assert_eq!(entry_source(&base, "missing"), None);

        let _ = fs::remove_dir_all(&base);
    }
}
