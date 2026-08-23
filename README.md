<div align="center">

<a href="https://luaupm.com">
  <img src="https://luaupm.com/ember-logo.png" alt="ember logo" width="110" />
</a>

# ember

**A package manager built for Luau.** The command is `embr`.

<a href="https://luaupm.com"><img src="https://img.shields.io/badge/luaupm.com-F23C1B?style=flat-square&logoColor=white" alt="Website" /></a>
<a href="https://luaupm.com/search"><img src="https://img.shields.io/badge/dynamic/json?url=https%3A%2F%2Fcdn.luaupm.com%2Fstats.json&query=%24.packages&label=packages&color=F23C1B&style=flat-square" alt="Packages" /></a>
<a href="https://luaupm.com"><img src="https://img.shields.io/badge/dynamic/json?url=https%3A%2F%2Fcdn.luaupm.com%2Fstats.json&query=%24.downloads_total&label=downloads&color=F23C1B&style=flat-square" alt="Downloads" /></a>
<img src="https://img.shields.io/badge/license-MIT-2ea44f?style=flat-square" alt="MIT license" />

[Browse packages](https://luaupm.com/search) · [Docs](https://luaupm.com/docs) · [Policies](https://luaupm.com/policies)

</div>

---

The CLI for [luaupm.com](https://luaupm.com): install Luau packages from the
ember registry (plus wally and pesde indices), publish your own, pin your
project's tools, and run its scripts — one manifest, one lockfile, one binary.

## Install

Grab the binary for your platform from the
[latest release](https://github.com/luaupm/cli/releases/latest), then let embr
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

### VS Code IntelliSense

```sh
embr self code    # ember.toml autocomplete + validation via Even Better TOML
```

Points your VS Code (or Insiders / VSCodium / Cursor) settings at the
[ember.toml schema](https://luaupm.com/ember.schema.json). Comments and existing
entries in `settings.json` survive the edit.

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

## Docs

Commands, the manifest, workspaces, publishing — everything lives at
[luaupm.com/docs](https://luaupm.com/docs).

## Contributing

See [CONTRIBUTING.md](CONTRIBUTING.md). Licensed [MIT](LICENSE).

<small>Ember/embr is not in any way affiliated with or endorsed by the Luau team or Roblox Corporation/any of its subsidaries.</small>