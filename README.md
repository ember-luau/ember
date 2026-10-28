<div align="center">

<a href="https://emberpm.dev">
  <img src="https://emberpm.dev/ember.png" alt="Ember logo" width="110" />
</a>

# ember

**A package manager built for Luau.** The command is `embr`.

<a href="https://emberpm.dev"><img src="https://img.shields.io/badge/emberpm.dev-F23C1B?style=flat-square&logoColor=white" alt="Website" /></a>
<a href="https://emberpm.dev/search"><img src="https://img.shields.io/badge/dynamic/json?url=https%3A%2F%2Fcdn.luaupm.com%2Fstats.json&query=%24.packages&label=packages&color=F23C1B&style=flat-square" alt="Packages" /></a>
<a href="https://emberpm.dev"><img src="https://img.shields.io/badge/dynamic/json?url=https%3A%2F%2Fcdn.luaupm.com%2Fstats.json&query=%24.downloads_total&label=downloads&color=F23C1B&style=flat-square" alt="Downloads" /></a>
<img src="https://img.shields.io/badge/license-MIT-2ea44f?style=flat-square" alt="MIT license" />

[Browse packages](https://emberpm.dev/search) · [Docs](https://emberpm.dev/docs) · [Policies](https://emberpm.dev/policies)

</div>

---

The CLI for [emberpm.dev](https://emberpm.dev): install Luau packages from the
ember registry (plus wally, pesde, and Forest indices), publish your own, pin your
project's tools, and run its scripts — one manifest, one lockfile, one binary.

## Install

Grab the binary for your platform from the
[latest release](https://github.com/ember-luau/ember/releases/latest), then let embr
install itself:

```sh
# macOS / Linux
chmod +x embr-macos-aarch64
./embr-macos-aarch64 self install
```

```powershell
# Windows
.\embr-windows-x86_64.exe self install
```

`self install` copies embr to `~/.ember/bin`. On Windows it adds that folder to
your PATH for you; elsewhere it prints the line to add to your shell profile.
Restart your terminal, then:

```sh
embr --version
embr self update    # pulls the newest release when there is one
```

### With cargo

```sh
cargo install embr    # crate and binary share the name
embr self install     # still worth running: tool shims live in ~/.ember/bin
```

### Arch Linux (AUR)

```sh
yay -S embr-bin    # or: paru -S embr-bin
embr self install  # still worth running: tool shims live in ~/.ember/bin
```

The package puts `embr` in `/usr/bin`, and pacman updates it with each
release.

### VS Code IntelliSense

```sh
embr self code    # ember.toml autocomplete + validation via Even Better TOML
```

Points your VS Code (or Insiders / VSCodium / Cursor) settings at the
[ember.toml schema](https://emberpm.dev/ember.schema.json). Comments and existing
entries in `settings.json` survive the edit.

## Start a project or a package

`embr init` writes `ember.toml`. It asks three questions:

1. **Project or package.** A project is code you run, such as a game or a
   tool. A package is code you publish, so it also asks for a name, a
   version, authors and a license.
2. **Language:** Luau, Alloy, Loom, or Luwu.
3. **Runtime:** Roblox, Roblox Server, Lune, Lute, Seal, Zune, or portable
   Luau. Loom offers the two Roblox sides only. Luwu gets no question:
   embr gives Luwu code the portable Luau environment, and a Luwu host such as
   [Seal](https://github.com/seal-runtime/seal) runs it.

The answers go into `[target]`, so the project takes only packages it can
run. Flags answer the questions for scripts and CI:

```sh
embr init --kind package --language alloy --environment lune --name me/tool --yes
```

Without a terminal, or with `--yes`, each question that has no flag takes
its default: a Roblox project in Luau.

`embr add scope/name` asks which index to use. To skip the question and
use the default index, run `embr self index-prompt off`. Run
`embr self index-prompt on` to get it back.

## Move from pesde, Wally, Forest, or Crater

`embr migrate` moves `pesde.toml`, `wally.toml`, `forest.json`, and
`crater.json` into `ember.toml`. It lists the files it finds, all checked, and moves the ones
that stay checked. Without a terminal, or with `--yes`, it moves all of them.

Each setting that ember has a place for moves, and leaves its old file. A
setting that ember has no place for stays in the old file, so nothing is
lost. A file with nothing left in it is deleted. Where `ember.toml` already
sets a key, its own value stays. Lockfiles stay, and `embr install` writes
`ember.lock`.

- **pesde:** the package fields, the `[target]` environment and `lib`,
  `includes`, `workspace_members`, `[indices]` and `[wally_indices]`, pesde,
  Wally and workspace dependencies, `[overrides]`, and `[scripts]`. pesde
  runs a script with Lune, so each one becomes `lune run <file>`, and
  `embr run <name>` passes its arguments through. pesde's `default` indices
  become `pesde` and `wally`, so ember's own default index stays. Git and
  path dependencies, dev and peer dependencies, `[engines]`, `[patches]`,
  `[place]`, and the `sourcemap_generator` hook stay.
- **Wally:** the package fields, `realm`, `include`, `exclude`, `registry`,
  `[dependencies]`, and `[server-dependencies]`, which install into the
  Roblox server tree. `[dev-dependencies]` and `[place]` stay.
- **Forest:** `author` and `name`, the version, description, and license,
  `platform = "roblox"`, `root`, and the dependencies with their aliases.
  `mounts`, `overrides`, `excludes`, `packagesDir`, and UEFN projects stay.
- **Crater:** embr reads the `.crater-declare` file that Crater 0.5.5 and
  later write beside `crater.json`. It gives each registry name's address,
  the realm of each dependency list, and the default of each field, so
  embr keeps no copy of Crater's rules. The package fields, `can_publish`
  (as `private`), a `.png`, `.jpg`, or `.webp` `icon` (as `logo`),
  `import_to` (as the environment), `path_to_main`, `exclude` or
  `package_files.exclude`, `workspace`, `scripts`, `tools`, and each
  client and server dependency from Ember, Wally, pesde, or Forest move. A
  server dependency installs into the Roblox server tree. A dependency
  with no registry name moves when `registry` is one such index alone.
  Packages of a crater registry, `path:` and `git+` dependencies, develop
  dependencies, `overrides`, `keywords`, and `crater.settings.json` stay. A
  field equal to its default goes.

## `embx`: run without installing

```sh
embx create-chief-project my-game    # scaffold a project, zero setup
embx stylua --check .                # one-off run of any released tool
embx JohnnyMorganz/StyLua@2.0.2 .    # any GitHub repo, exact versions too
```

`embx` (installed by `self install`, same command as `embr x`) downloads a
GitHub-released executable on first use, caches it under `~/.ember/tools`, and
hands your terminal straight to it — nothing is added to any manifest. Names
pinned under `[tools]` run their pinned version; other bare names come from
embr's shorthand list; anything else is `owner/repo`.

## One package, several targets

A package can ship one release for each target. Write one `[[target]]`
table for each target in place of the single `[target]` table:

```toml
[package]
name = "acme/signal"
version = "1.0.0"

[[target]]
environment = "roblox"
main = "lib/init.luau"
includes = ["lib"]

[[target]]
environment = "roblox"
language = "alloy"
main = "build/init.luau"
includes = ["src", "build"]

[[target]]
environment = "luau"
language = "luwu"
main = "luwu/init.luwu"
includes = ["luwu"]
```

Each target has an ID. For Luau, the ID is the environment. For another
language, the ID is the environment and the language: `roblox`,
`roblox-alloy`, `luau-luwu`. Two targets with the same ID are an error.

- `embr publish` uploads one archive for each target. Each archive holds
  the files, the `main`, and the build output of its target only.
- `embr publish --target roblox-alloy` publishes only that target. Repeat
  `--target` to publish more than one.
- `embr yank` and `embr deprecate` take `--target <id>` to change one
  target of a version.

The first target is the project's own. `embr add` and `embr install` use
it to pick a target of each dependency. They prefer the same environment
and language, then the same environment in plain Luau.

## Entry points and collections

The link file of a package requires its entry point. embr looks for it in
this order: `[target] main`, pesde's `lib`, Forest's `root`, a Rojo
`$path`, an `init` file in the root, `src`, or `lib`. Then it guesses: the
only module, or the module named after the package.

A library of separate modules needs no entry point. When embr finds none,
or `main` names a folder with no init file, the link file returns a
collection: a table with one field for each module.

```luau
local Utils = require("@packages/utils")

local changed = Utils.Signal.new()
local spring = Utils.Spring.new(0)
```

- **Members:** each `.luau` and `.lua` file, and each folder with an
  `init.luau` or `init.lua`, one level deep. Scripts (`*.server.luau`,
  `*.client.luau`), tests (`*.spec.luau`, `*.test.luau`), and folders with
  no init file stay out.
- **Folder:** with no `main`, the collection is the first of `src`, `lib`,
  and the package root that has a member. A package with a source folder
  keeps its modules there.
- **Lazy on Roblox:** a Roblox project requires a module when code first
  reads its field, so a client does not run a server module. A module that
  yields while it loads cannot load this way. Other projects require all
  modules at once, because Lune's `require` yields.
- **Typed:** the link file exports a `Modules` type, so luau-lsp completes
  `Utils.Signal` and reports `Utils.Nope`. A name that is no identifier, or
  is a keyword, takes brackets: `Utils["my-module"]`, `Utils["end"]`.
- `embr publish --dry-run` shows the entry it found, for example
  `Entry: collection of 3 modules (Promise, Signal, Spring)`. A package with
  nothing to require gets a warning at publish and at install.

## Models in packages

A Roblox package can ship `.rbxm` models next to its code: UI templates,
rigs, effects. On install a model becomes an instance named after its file,
so `src/Models/Rock.rbxm` is `script.Models.Rock`. The registry scans each
model at publish and refuses:

- a model with a `Script`, `LocalScript` or `ModuleScript` anywhere in it.
  Code goes in `.luau` files, where anyone can read it;
- a model file with more than one root. Save one model per file;
- an XML model (`.rbxmx`). `embr publish` stops on it before the upload;
- a package with models but fewer than 10 lines of code.

## Local packages

A local package is a package that only this computer can install. Use it
for a library that you use in more than one of your projects but do not
publish.

```sh
cd my-lib && embr publish --local    # no login, no upload
cd my-game && embr add my-lib        # a name with no scope is a local package
embr install
```

- `embr publish --local` packs the same archive as `embr publish`, with
  each target, `--target` and the build. It writes the archive and its index
  entry to `~/.ember/local`.
- `embr add my-lib` finds the package by its name in every scope. If two
  scopes have that name, use `embr add me/my-lib --index local`.
- The dependency is `{ name = "me/my-lib", version = "^", index = "local" }`.
  The index `local` needs no `[indices]` entry.
- Publish the same version again after a change. The next `embr install`
  installs the new archive.
- ember's index stays the default. To make `local` the default for this
  computer, run `embr self default-index local`.

`embr yank` and `embr deprecate` change the registry only. They do not
change a local package.

## `embr mcp`: package search for coding agents

`embr mcp` runs a [Model Context Protocol](https://modelcontextprotocol.io)
server on stdio. A coding agent uses it to find packages in the ember
registry, and in Wally, pesde, and Forest. The server only reads. It does
not change files, install packages, or publish.

It gives the agent three tools:

- `search_packages` finds ember packages by name or description. It can
  filter by environment and language. Each result gives the latest version,
  the download count, and the `embr add` command that installs the package.
- `search_all` searches ember, Wally, pesde, and Forest at the same time.
  Each result gives its registry, the latest version, and the `embr add`
  command. For Wally, pesde, and Forest, that command adds `--index`, and
  the project needs the index first: `embr index add wally` adds it once. A
  registry that fails or does not answer in 10 seconds is listed with the
  reason, and the other registries still answer. The results leave out
  Forest's copies of Wally packages.
- `package_info` shows one package. It lists every version with its target,
  its language, and yanked or deprecated marks. It also gives the
  dependencies of the newest version that is not yanked.

To register it with Claude Code:

```sh
claude mcp add ember -- embr mcp
```

Other clients read an `mcpServers` block like this one:

```json
{
  "mcpServers": {
    "ember": {
      "command": "embr",
      "args": ["mcp"]
    }
  }
}
```

## Docs

Commands, the manifest, workspaces, publishing — everything lives at
[emberpm.dev/docs](https://emberpm.dev/docs).

## Contributing

See [CONTRIBUTING.md](CONTRIBUTING.md). Licensed [MIT](LICENSE).

<small>Ember/embr is not in any way affiliated with or endorsed by the Luau team or Roblox Corporation/any of its subsidaries.</small>