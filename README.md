<div align="center">

<a href="https://emberpm.dev">
  <img src="https://emberpm.dev/ember.png" alt="Ember logo" width="110" />
</a>

# ember

**A package manager built for Luau.** The command is `embr`.

<a href="https://emberpm.dev"><img src="https://img.shields.io/badge/emberpm.dev-F23C1B?style=flat-square&logoColor=white" alt="Website" /></a>
<a href="https://emberpm.dev/search"><img src="https://img.shields.io/badge/dynamic/json?url=https%3A%2F%2Fcdn.emberpm.dev%2Fstats.json&query=%24.packages&label=packages&color=F23C1B&style=flat-square" alt="Packages" /></a>
<a href="https://emberpm.dev"><img src="https://img.shields.io/badge/dynamic/json?url=https%3A%2F%2Fcdn.emberpm.dev%2Fstats.json&query=%24.downloads_total&label=downloads&color=F23C1B&style=flat-square" alt="Downloads" /></a>
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
   [Seal](https://github.com/seal-runtime/seal) runs it. A Luwu project
   installs those packages to `packages/luwu`, because Luwu code does not
   run as Luau.

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
  Wally and workspace dependencies, `[peer_dependencies]` as
  `[peer-dependencies]`, `[overrides]`, and `[scripts]`. pesde
  runs a script with Lune, so each one becomes `lune run <file>`, and
  `embr run <name>` passes its arguments through. pesde's `default` indices
  become `pesde` and `wally`, so ember's own default index stays. Git and
  path dependencies, dev dependencies, `[engines]`, `[patches]`,
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
  (as `private`), a `.png`, `.jpg`, `.webp`, or `.gif` `icon` (as `logo`),
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
embr's built-in shorthands, then from the ones the registry adds (such as
`azul`); anything else is `owner/repo`. A built-in shorthand always wins.

## `embr studio push`: packages in Studio with Azul

```toml
[studio]
sync = "azul"    # optional: a sync/ folder with a Studio service in it also tells embr

[tools]
azul = "npm:azul-sync@2.4.0"    # npm tools need Node.js
```

[Azul](https://azul.ransomwave.games) mirrors Studio to disk, so it does not
read the packages folders into Studio. `embr studio push` runs
`azul push --source <folder> --destination <path> --destructive --rojo --no-warn`
for each installed folder. The roblox packages go to
`ReplicatedStorage.Packages` and the server packages go to
`ServerStorage.Packages`. `roblox-packages` and `server-packages` under
`[studio]` change that. After `embr install` changes a folder, embr pushes
it, if Studio is open and the Azul plugin connects in 15 seconds. Else embr
prints one line, and the install does not fail. embr adds the copies that
Azul mirrors back, such as `sync/ReplicatedStorage/Packages/`, to
`.gitignore`, and `embr publish` does not pack them.

## `embr remove`: take out a dependency

```sh
embr remove signal         # an alias from ember.toml
embr rm chief/core lest    # a scope/name, and more than one at a time
```

`embr remove` deletes entries from `[dependencies]`, `[dev-dependencies]`
and `[peer-dependencies]`. Each name is an alias or the `scope/name` of an
entry. The comments and the format of `ember.toml` stay. If a name matches
no entry, embr shows the aliases and changes nothing.

`embr remove` does not install. Run `embr install` after it. If
`[patches]` or `[overrides]` still name a removed dependency, embr shows a
warning, because `embr install` can fail on that entry. A `preremove`
script runs before the edit, and a `postremove` script runs after it.

## `embr search` and `embr info`: find a package

```sh
embr search signal                              # the ember registry
embr search ecs --environment lune --limit 5
embr search signal --all                        # also Wally, pesde, and Forest
embr info chief/core                            # versions, dependencies, details
embr info chief/core --version "^0.2"
```

- `embr search` gives the results of the `embr mcp` search tools, most
  downloaded first. Each line gives the latest version, the environments,
  the download count, and the `embr add` command. `--environment` and
  `--language` filter the ember registry. `--limit` sets the number of
  results: 20 by default, and 50 at most. With `--all`, the limit is for
  each registry, and the default is 10.
- `embr info` reads the index, as an install does. It shows the newest
  version that is not yanked, or the newest version that `--version`
  matches. It also shows the description, the repository, the targets, the
  dependencies, the dev dependencies, `requires`, a deprecation, and the 10
  newest versions. A pesde entry also gives the license and the authors.
  `--index` reads another index, such as `pesde`. A private or unlisted
  package comes from the registry API, with your login or its link key.

## `embr tree` and `embr why`: see what is installed

```sh
embr tree                  # each installed dependency, as a tree
embr tree --depth 0        # the direct dependencies only
embr why chief/traits      # each path from ember.toml to a package
```

Both commands read what `embr install` wrote: `ember.lock`, and the
manifest of each installed package. Run `embr install` first. A package
with dependencies that shows a second time gets `(*)`, and its
dependencies do not show again. When the direct dependencies install in
more than one environment, such as `roblox` and `roblox_server`, each
environment gets a heading.

## `embr notices`: what the owners say

The owners of a package can post a notice, a warning, or a security notice
on the package or on a range of its versions. A security notice names a
known vulnerability or a malicious version. `embr install` and `embr add`
end with one line for each item, security notices first, in red:

```text
security: acme/fuel@0.2.0: 0.2.0 sends the place file to a remote host
warning: acme/rocket@1.0.0: 1.x leaks memory
deprecated: acme/old: no longer kept up, use acme/new instead
notice: acme/rocket: the docs moved to rocket.dev
```

- A security notice, a warning and a deprecation show on each install. A
  notice shows when the install adds or updates the package.
- `embr notices` lists every notice, warning, deprecation and yank of the
  installed packages, with the dependency that brings each one in. It
  exits with 0. `embr notices --deny security,warnings,deprecations` exits
  with 1 when one of them applies, for CI.
- `embr audit` lists only the security notices and the yanked versions.
  It exits with 1 when a security notice applies, else with 0.
  `--deny warnings` (or another kind) also fails it on that kind.
- The owners post with `embr notices add acme/rocket "1.x leaks memory"
  --warning --versions "<2.0.0"`, or `--security` for a security notice,
  and remove with `embr notices remove acme/rocket <id>`. `embr info`
  shows each id.
- `embr deprecate acme/old "no longer kept up" --use acme/new` names the
  package to use instead.

The notices are in the index, so they cost no request. embr removes escape
sequences and control characters from them before it prints them.

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

## Runtime and language versions

A target can give the versions of its runtime and its language that it
needs:

```toml
[target]
environment = "lune"
language = "alloy"
requires = { lune = ">=0.9.0", alloy = "^0.4.0" }
```

The keys are the runtime of the target (`lune`, `lute`, `seal`, `zune`, or
`luau`) and its language (`luau`, `alloy`, `loom`, or `luwu`). Roblox has
no version key. Each value is a version requirement, the same as the
version of a dependency. Each `[[target]]` has its own `requires`.

`embr install` compares the `requires` of each installed package with the
program on PATH. A mismatch gives a warning, and the install continues:

```
warning: alloy/lattice wants alloy ^0.4.0; alloy 0.3.2 is on PATH
```

A runtime key counts only when your project uses that runtime. A language
key counts only when your project is in that language. A built Alloy
package in a Luau project needs no Alloy compiler, so embr does not check
`alloy` there.

## Dev dependencies

`[dev-dependencies]` holds the packages that only the tests and tools of
your project use. It takes the same entries as `[dependencies]`:

```toml
[dev-dependencies]
lest = { name = "alloy/lest", version = "^0.2.0" }
```

- `embr add --dev <scope/name>` writes to `[dev-dependencies]`.
- `embr install` installs the dev dependencies of your project. In a
  workspace, each member installs its own. The dev dependencies of a
  dependency never install.
- A dev dependency can be in any language, so a Luau project can use an
  Alloy test framework. The environment rules do not change.
- One alias cannot be in `[dependencies]` and in `[dev-dependencies]`.
- `embr outdated` lists the dev dependencies too.
- `embr publish` keeps the table in the package, so the registry can show
  it.

## Optional dependencies

An entry with `optional = true` is an optional dependency. It takes the
same keys as any other entry, `target` and `entry` too:

```toml
[dependencies]
signal = { name = "acme/signal", version = "^1.0.0", optional = true }
```

- `embr install` tries to install it. When it does not resolve, the
  install skips it with one line and goes on. That happens when no version
  matches, when it has no target for the environment, when its download
  fails, or when one of its own dependencies fails. An archive with
  another hash stops the install (see "Archive hashes").
- A skipped optional dependency gets no link file. The code checks for it
  at run time:

  ```luau
  local ok, Signal = pcall(require, "@packages/signal")
  if not ok then
    Signal = nil -- install skipped it, so the feature stays off
  end
  ```

- The optional dependencies of a dependency follow the same rule.
- `ember.lock` holds what was installed. A package that only optional
  dependencies bring in has `optional = true` there.
- `embr add --optional <scope/name>` writes the entry.

```
skipped optional dependency 'signal' (acme/signal): No version of acme/signal matches requirement '^9.0.0'
```

## Peer dependencies

`[peer-dependencies]` lists the packages that the project which installs
your package must have. There is then one copy of each, such as one React
or one Fusion:

```toml
[peer-dependencies]
React = { name = "jsdotlua/react", version = "^17.0.0" }
```

- `embr install` never installs a peer. It links the alias of the peer to
  the copy in the project, in the same environment root.
- When the project has no copy, or has a version that the requirement does
  not take, the install shows one warning line at the end and does not
  fail. `embr install --strict-peers` makes the install fail.
- In the project that you install, `[peer-dependencies]` does nothing.
  Put the same package in `[dev-dependencies]` to use it in your tests.
- pesde's `peer_dependencies` work the same way, and `embr migrate` moves
  them to `[peer-dependencies]`.
- `embr add --peer <scope/name>` writes the entry.

```
warning: acme/ui@1.0.0: needs the peer jsdotlua/react ^17.0.0, which the project does not have. Run `embr add jsdotlua/react --version "^17.0.0"`
warning: acme/menu@2.0.0: needs the peer jsdotlua/react ^17.0.0, and the project has 16.4.0
```

Both need embr 0.14.0, and a registry that keeps the new tables in the
index entry. The entry keeps an optional dependency in
`optional-dependencies` and a peer in `peer-dependencies`. An older embr
reads neither table, so it does not install an optional dependency and
does not fail on one.

## Workspace inheritance

The root of a workspace can hold shared values in a `[workspace]` table. A
member takes a value with `workspace = true`, as in Cargo:

```toml
# ember.toml at the root
[target]
environment = "roblox"
workspace = ["places/*"]

[workspace.package]
version = "1.4.0"

[workspace.dependencies]
signal = { name = "acme/signal", version = "^1.0.0" }
```

```toml
# places/lobby/ember.toml
[package]
name = "acme/lobby"
version.workspace = true

[dependencies]
signal = { workspace = true }
```

- Any value or table can inherit. It reads the same path under
  `[workspace]`, so `package.version` reads `workspace.package.version`.
- `[dev-dependencies]` reads `[workspace.dependencies]`, the same as
  `[dependencies]`.
- An inherited dependency can add `target` and `entry`. Any other key next
  to `workspace = true` is an error.
- The root can inherit from its own `[workspace]` table.
- `embr publish` writes the values into the package, and the `[workspace]`
  table stays home.
- `embr tool update` skips an inherited tool. Change its version in the
  root.

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

## Editor files on install

`embr install` keeps two editor files in step with the installed packages.

- **Require aliases.** Each packages folder that holds links gets an
  alias. The folder of the project's own environment gets `packages`, so
  `require("@packages/core")` works. Each other folder gets
  `<folder>-packages`, such as `server-packages` for `packages/server`,
  because Luau allows no `/` in an alias name. In a Luwu project on another
  runtime, `packages/luwu` gets `luwu-packages`, and the old
  `luau-packages` points there too until embr 0.15. The aliases go into
  `.config.luau` when the project has one, else into `.luaurc`. embr makes
  a `.luaurc` only when an alias needs it. A folder with both files gets no
  aliases, because Luau refuses that folder. A tree with
  `requires = "instance"` gets no alias.
- **Definition files.** The `.d.luau` files of the installed packages go
  into `luau-lsp.types.definitionFiles` in `.vscode/settings.json`. A
  package names its files in `[target] definitions`, as paths, folders, or
  globs. Without that list, embr takes each `.d.luau` in the package, at
  most ten. embr makes the settings file only when a package has a
  definition file.

embr changes only its own entries. Comments, other keys, your own aliases,
and your own definition files stay. An alias is embr's when its name is one
that embr gives a folder. A definition entry is embr's when its path goes
into a package store. When embr cannot read a file, it warns and does not
change it.

To turn off one or both:

```toml
[config]
aliases = false
definition-files = false
```

## Archive hashes

`embr install` checks the SHA-256 of each package archive before it
extracts it. `ember.lock` keeps the hash of each package as `sha256`.

- The hash comes from the index entry. The ember registry writes `sha256`
  on each version, and Forest names each archive by its hash.
- An entry with no hash gets the hash of its first install: a pesde or
  Wally package, or an ember version that the registry has not hashed yet.
  Later installs check against `ember.lock`.
- A cached archive with another hash downloads again.
- A downloaded archive with another hash stops the install. The error
  names the package, the version, both hashes and the URL. embr deletes
  the download and does not use it. This is true for an optional
  dependency too: a changed archive is not a failed download, so it is
  not skipped.
- When the index entry and `ember.lock` record different hashes for one
  version, the install stops too, because a published version does not
  change. If the publisher changed the archive on purpose, run
  `embr install --update <scope/name>` to lock the new hash.
- `embr install --locked` checks against `ember.lock`.
- A lockfile from before embr 0.14.0 gets the hashes on the next
  `embr install`. An older embr ignores them.
- Local packages and workspace members have no hash.

## Sign in

`embr publish`, `embr yank` and `embr deprecate` need a login. To sign in,
run `embr auth login`:

1. embr shows a link and a code, and opens the link in your browser.
2. Sign in on emberpm.dev, if you have no session there.
3. Make sure that the page shows the code from your terminal. Then click
   Approve.

embr then stores an Ember token in `~/.ember/credentials.toml`. You do not
make or paste a token. The link works on all devices, so over SSH you can
open it on a different computer. The code expires after 10 minutes.

- `embr auth login --token ember_...` stores a token that you made on
  emberpm.dev.
- In CI, set `EMBER_TOKEN`. It wins over the stored login.
- A GitHub login from embr 0.9 changes to an Ember token on first use.

### Trusted publishing

A GitHub Actions workflow can publish with no `EMBER_TOKEN` secret. The
registry accepts the OIDC token of the workflow when the package names that
repository and workflow as a trusted publisher. This needs embr 0.12.0 or
later.

1. Publish the first version with a token. A trusted publisher cannot make
   a new package.
2. On emberpm.dev, open the Manage tab of the package. Under Trusted
   publishing, add the repository (`owner/repo`), the workflow file name
   (`release.yml`) and, if you want, a GitHub environment.
3. Give the job `id-token: write`, and run `embr publish` with no
   `EMBER_TOKEN`:

```yaml
# .github/workflows/release.yml
on:
  push:
    tags: ["v*"]
jobs:
  publish:
    runs-on: ubuntu-latest
    permissions:
      id-token: write
      contents: read
    steps:
      - uses: actions/checkout@v4
      - run: |
          curl -fsSL -o embr https://github.com/ember-luau/ember/releases/latest/download/embr-linux-x86_64
          chmod +x embr
          ./embr publish
```

- When `EMBER_TOKEN` is set, it wins. The OIDC token then gives provenance
  only.
- With no `EMBER_TOKEN` and no stored login, embr uses the OIDC token. It
  gets a new one for each upload and never opens the browser login.
- The publish runs as the account that added the trusted publisher. When
  that account loses its access to the package, the publish fails.
- When the registry refuses, embr shows its message. It names the
  repository and the workflow that it saw.

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

## Private and unlisted packages

A private package is visible only to people with access to it. An unlisted
package is also visible to each person who has its link. Search does not
show either kind. The git index does not hold them, so embr gets them from
the registry API.

To publish one, set `visibility` in `[package]`:

```toml
[package]
name = "acme/tools"
version = "1.0.0"
visibility = "private"    # or "unlisted". No value means public.
```

- Publish the first version with embr 0.10.0 or later. An older embr drops
  `visibility`, and the package becomes public. A public package stays
  public.
- `private = true` is a different key. It means "never publish".

To install a private package, sign in:

- `embr auth login` stores an Ember token in `~/.ember/credentials.toml`.
  A login from embr 0.9 changes to an Ember token on first use.
- `embr auth login --token ember_...` stores a token from emberpm.dev.
- In CI, set `EMBER_TOKEN`. It wins over the stored login.

To install an unlisted package, add its link:

```sh
embr add 'https://emberpm.dev/package/acme/tools#k=<key>'
```

- embr writes the key on the dependency:
  `tools = { name = "acme/tools", version = "^", key = "<key>" }`. A clone
  of the project then installs the package with no setup.
- Each person who can read `ember.toml` can use the key. In a public
  repository, that is everyone. When the owner makes a new link, the old key
  stops working, and each project must add the new link.
- `ember.lock` holds the package name. It never holds the key.

embr sends the token and the key only to the registry API host,
`api.emberpm.dev`. When the API redirects a download to another host, embr
sends neither. Old index entries and lock files name the hosts from before
emberpm.dev, `api.luaupm.com` and `cdn.luaupm.com`. embr gets those files
from `api.emberpm.dev` and `cdn.emberpm.dev`. To use a local backend, set `EMBER_API_URL`, for example
`EMBER_API_URL=http://127.0.0.1:8787`.

## `embr mcp`: package search for coding agents

`embr mcp` runs a [Model Context Protocol](https://modelcontextprotocol.io)
server on stdio. A coding agent uses it to find packages in the ember
registry, and in Wally, pesde, and Forest. The server only reads. It does
not change files, install packages, or publish.

It gives the agent three tools:

- `search_packages` finds ember packages by name or description, through
  the registry's search API. It can filter by environment and language.
  Each result gives the latest version, the download count, and the
  `embr add` command that installs the package.
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