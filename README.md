<div align="center">
  <img src="https://github.com/luaupm.png?size=200" alt="lpm" width="120" height="120">

  <h1>lpm</h1>

  <p><strong>The Luau package manager.</strong><br>
  One CLI for your dependencies, your tools, and your scripts.</p>

  <p>
    <a href="https://github.com/luaupm/cli/actions/workflows/ci.yml"><img src="https://github.com/luaupm/cli/actions/workflows/ci.yml/badge.svg" alt="CI"></a>
    <a href="https://crates.io/crates/luaupm"><img src="https://img.shields.io/crates/v/luaupm?label=crates.io&color=e61048" alt="crates.io"></a>
    <a href="LICENSE"><img src="https://img.shields.io/badge/license-MIT-e61048" alt="MIT license"></a>
    <img src="https://img.shields.io/badge/rust-1.88%2B-lightgrey" alt="Rust 1.88+">
    <img src="https://img.shields.io/badge/platforms-windows%20%7C%20macos%20%7C%20linux-lightgrey" alt="Platforms">
  </p>
</div>

---

lpm installs Luau packages from the ecosystems you already use, pins the
command line tools your project needs, and runs your project's scripts. One
manifest, one lockfile, one binary.

- **Works across ecosystems.** Wally and pesde indices resolve side by side,
  including transitive dependencies that cross from one into the other.
- **Wally-style install layout.** Packages land under `packages/<environment>/`
  with generated link files, so `require` paths stay short and stable.
- **Tools are pinned like dependencies.** `rojo = "rojo-rbx/rojo@7.7.0"` in
  your manifest, and `rojo` on your PATH runs that exact version inside that
  project.
- **Scripts.** `lpm run build` executes what your manifest says, on any
  platform.

## Status

lpm is in beta and moving. Here is what stands today and what is being built:

| Area | State |
| --- | --- |
| Installing from wally and pesde indices | Works |
| Tools, scripts, lockfile, linker | Works |
| lpm's own package index | Being replaced by a first-party API |
| `lpm publish` | Packs your project, then stops. Uploading waits on that API |
| Extensions | In design. A Rust API for extending lpm itself |

Until the API lands, a dependency that does not name an index needs a
`default` entry under `[indices]`, and publishing is unavailable. Everything
left to build is marked `TODO(api)` in the source.

### Extensions

lpm will not ship every capability a project wants, so it is growing a way to
add them. Extensions are written in Rust against an exposed lpm API, and can
be loaded and unloaded rather than being baked into the binary: bring in what
your project needs, drop it when you don't.

The API surface is still being designed and nothing here is implemented yet,
so take this as the direction rather than a promise.

## Install

Grab the binary for your platform from the
[latest release](https://github.com/luaupm/cli/releases/latest), then let lpm
install itself:

```bash
# macOS / Linux
chmod +x lpm-macos-aarch64
./lpm-macos-aarch64 self install
```

```powershell
# Windows
.\lpm-windows-x86_64.exe self install
```

`self install` copies lpm to `~/.lpm/bin`. On Windows it adds that folder to
your PATH for you; elsewhere it prints the line to add to your shell profile.
Restart your terminal, then:

```bash
lpm --version
lpm self update    # pulls the newest release when there is one
```

### With cargo

```bash
cargo install luaupm     # the crate is luaupm, the binary is lpm
lpm self install         # optional, see below
```

The crate is published as `luaupm` because `lpm` was already taken on
crates.io. The binary it installs is still `lpm`.

Cargo puts it in `~/.cargo/bin`, which is already on your PATH, so lpm works
straight away. `self install` is still worth running: pinned tools are invoked
through shims in `~/.lpm/bin`, and that folder is what `self install` adds to
your PATH. Without it, `lpm install` will happily install `rojo` and you will
not be able to run it.

One thing to know: `self install` also copies lpm into `~/.lpm/bin`, and on
Windows that folder goes to the front of your PATH, so it wins over the cargo
copy. Update with `lpm self update` from then on, or, if you would rather cargo
stay in charge, skip `self install` and add `~/.lpm/bin` to your PATH yourself.

### From source

```bash
cargo build --release    # target/release/lpm
```

## Quick start

```bash
mkdir my-game && cd my-game
lpm init                                  # interactive manifest wizard
lpm index add wally                       # register an index to pull from
lpm add evaera/promise --index wally      # resolve and record the dependency
lpm install                               # download, link, write lpm.lock
```

That leaves you with:

```
my-game/
  lpm.toml
  lpm.lock
  packages/
    shared/
      promise.luau          <- link file
      .lpm/
        evaera_promise/     <- the package itself
```

and in your Luau code:

```lua
local Promise = require("./packages/shared/promise")
```

## Commands

| Command | What it does |
| --- | --- |
| `lpm init` | Interactive manifest wizard. Guesses name, authors and repository from git, and git-ignores `packages/` |
| `lpm add <scope>/<name>` | Resolves the package, then records it. `--version <req>`, `--index <key>`, `--alias <name>` |
| `lpm install` (`lpm i`) | Re-resolves everything, rebuilds the output folders, installs tools, writes `lpm.lock`. `--locked` installs exactly what the lockfile says without touching an index |
| `lpm index add [name] [url]` | Adds an index under `[indices]`. Knows `wally` and `pesde` by name; prompts for anything it doesn't |
| `lpm index remove [name]` | Removes one, and warns about dependencies still pointing at it |
| `lpm tool add <name>` | Pins a GitHub-released tool. `--version <v>`, `--global` |
| `lpm tool remove <name>` | Unpins it. `--global` |
| `lpm tool update` | Bumps every pinned tool to its latest release |
| `lpm tool list` | Shows what is pinned and what is installed, per scope |
| `lpm tool delete <name>` | Deletes the downloaded binaries. `--version <v>` keeps the others |
| `lpm run <name>` | Runs a `[scripts]` entry through the platform shell, forwarding its exit code |
| `lpm studio` | Launches Roblox Studio |
| `lpm publish` | Packs the project. `--dry-run` shows the archive and file list |
| `lpm self install\|update\|uninstall` | Manages the lpm installation itself |

Adding a dependency without `--index` prompts for one, so run it from a real
terminal.

## The manifest

```toml
[package]
name = "acme/rocket"          # scope/name, lowercase
version = "0.1.0"
description = "A rocket"      # optional
authors = ["Jane Doe <jane@example.com>"]
repository = "acme/rocket"    # owner/repo or a GitHub URL
license = "MIT"
include = ["src"]             # optional; what gets packed for publishing
exclude = ["src/tests"]       # optional; lpm.toml always ships

[target]
environment = "shared"        # shared | server | lune | luau | lute
main = "src/init.luau"        # entry point consumers require

[indices]
wally = "https://github.com/UpliftGames/wally-index"
pesde = "https://github.com/pesde-pkg/index"
default = "https://github.com/pesde-pkg/index"   # used by deps naming no index

[dependencies]
Promise = { name = "evaera/promise", version = "^4.0.0", index = "wally" }
Hello   = { name = "pesde/hello", version = "^" }   # "^" means latest

[tools]
rojo = "rojo-rbx/rojo@7.7.0"     # key is the command name you type
stylua = "johnnymorganz/stylua@2.5.2"

[scripts]
build = "rojo build -o game.rbxl"
fmt = "stylua src"

[config]                      # optional; these are the defaults
shared-packages-out = "packages/shared"
server-packages-out = "packages/server"
lune-packages-out   = "packages/lune"
luau-packages-out   = "packages/luau"
lute-packages-out   = "packages/lute"
```

**Environments** say where a package's code runs: `shared` and `server` for
Roblox, `lune`, `luau` and `lute` for the standalone runtimes. Foreign
manifests translate automatically (pesde's `roblox` and `roblox_server`,
wally's realms).

**Versions** are standard semver requirements. A bare `^` (or `*`) means
"whatever is newest".

## How installing works

Every `lpm install` starts clean: each environment's output folder is wiped
and rebuilt, so removing a dependency actually removes it from disk.

Packages are extracted to `<out>/.lpm/<scope>_<name>/`, and lpm writes a link
file beside it that re-exports the package's entry point. Direct dependencies
are named after their `[dependencies]` key, transitive ones after the package's
short name. Transitive dependencies flatten into the same folders, deduplicated
by package name; two requirements that cannot agree on a version are an error,
not a silent pick.

The entry point is found in this order: lpm.toml `[target].main`, pesde.toml
`[target].lib`, a Rojo `default.project.json` tree path, then conventional
locations like `init.luau` and `src/init.luau`.

`lpm.lock` records the resolved version and a baked download URL for each
package, so `lpm install --locked` reproduces an install without consulting
any index. Commit it.

## Tools

A tool is a GitHub-released binary. Pin one and lpm downloads the right asset
for your platform, stores it by version, and puts a shim on your PATH:

```bash
lpm tool add rojo              # newest release
lpm tool add stylua -v 2.5.2   # a specific one
lpm tool add rojo --global     # available everywhere, not just this project
lpm install                    # tools install here, alongside packages
```

The shim resolves per directory: inside a project, `rojo` runs the version that
project pins; outside it, the global one. Well-known tools (`rojo`, `stylua`,
`darklua`, `luau-lsp`, `lest`) can be added by bare name; anything else is
`owner/repo`.

If another toolchain manager's shim sits earlier on your PATH, `lpm install`
tells you, rather than leaving you wondering why a version you did not pin is
running.

## Indices

An index is a git repository lpm shallow-clones into `~/.lpm/index-cache/` and
pulls before resolving. Both major formats work:

- **Wally** — detected by a root `config.json`. Package files are JSON lines,
  one per version.
- **pesde** — detected by a root `config.toml`. Package files are TOML keyed by
  `"<version> <target>"`.

When a refresh fails, lpm falls back to the cached copy with a warning, so a
flaky connection does not block a build.

## What lpm writes

| Path | What it is |
| --- | --- |
| `lpm.toml` | Your manifest |
| `lpm.lock` | Resolved dependencies; commit it |
| `packages/` | Installed packages and link files; git-ignore it |
| `~/.lpm/bin/` | Tool shims, on your PATH |
| `~/.lpm/tools/` | Tool binaries, one folder per repo and version |
| `~/.lpm/tools.toml` | Globally pinned tools |
| `~/.lpm/index-cache/` | Cloned indices |
| `~/.lpm/credentials.toml` | GitHub token, when you have logged in (0600 on unix) |

## Development

```bash
cargo build                  # target/debug/lpm
cargo test                   # unit tests, no network
cargo fmt --check            # formatting
cargo clippy --all-targets   # keep this at zero warnings
```

`cargo test` does not rebuild the binary. Run `cargo build` before testing
`target/debug/lpm` by hand or you will run stale code.

Source is grouped by what the code talks to:

| Folder | Contents |
| --- | --- |
| `src/commands/` | One file per subcommand |
| `src/project/` | Manifest schema and editing, lockfile, installed-package inspection |
| `src/registry/` | Index lookup, dependency resolution, publish packing |
| `src/net/` | HTTP wrapper, GitHub client, OAuth device flow |
| `src/sys/` | The `~/.lpm` layout, subprocesses, git |
| `src/tools/` | Tool storage, release archives, shims |

[CLAUDE.md](CLAUDE.md) carries the deeper notes: the hard-won HTTP quirks, the
index format details, and the UI conventions.

### Releases

Pushing a `v*` tag builds and publishes the release. The tag has to match
Cargo.toml's version exactly, and asset names stay in the form
`lpm-{os}-{arch}[.exe]`, because `lpm self update` relies on both. Tags with a
`-` in them (`v0.1.0-beta.1`) are marked pre-release and stay invisible to
`self update`.

Commits follow [conventional commits](https://www.conventionalcommits.org).
