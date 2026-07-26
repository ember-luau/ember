<div align="center">

<a href="https://luaupm.com">
  <img src="https://luaupm.com/lpm-logo.png" alt="lpm logo" width="110" />
</a>

# lpm

**The package manager for Luau.**

<a href="https://luaupm.com"><img src="https://img.shields.io/badge/luaupm.com-e61048?style=flat-square&logoColor=white" alt="Website" /></a>
<a href="https://luaupm.com/search"><img src="https://img.shields.io/badge/dynamic/json?url=https%3A%2F%2Fcdn.luaupm.com%2Fstats.json&query=%24.packages&label=packages&color=e61048&style=flat-square" alt="Packages" /></a>
<a href="https://luaupm.com"><img src="https://img.shields.io/badge/dynamic/json?url=https%3A%2F%2Fcdn.luaupm.com%2Fstats.json&query=%24.downloads_total&label=downloads&color=e61048&style=flat-square" alt="Downloads" /></a>
<img src="https://img.shields.io/badge/license-MIT-2ea44f?style=flat-square" alt="MIT license" />

[Browse packages](https://luaupm.com/search) · [Docs](https://luaupm.com/docs) · [Policies](https://luaupm.com/policies)

</div>

---

The CLI for [luaupm.com](https://luaupm.com): install Luau packages from the
lpm registry (plus wally and pesde indices), publish your own, pin your
project's tools, and run its scripts — one manifest, one lockfile, one binary.

## Install

Grab the binary for your platform from the
[latest release](https://github.com/luaupm/cli/releases/latest), then let lpm
install itself:

```sh
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

```sh
lpm --version
lpm self update    # pulls the newest release when there is one
```

### With cargo

```sh
cargo install luaupm     # the crate is luaupm, the binary is lpm
lpm self install         # still worth running — tool shims live in ~/.lpm/bin
```

## Docs

Commands, the manifest, workspaces, publishing — everything lives at
[luaupm.com/docs](https://luaupm.com/docs).

## Contributing

See [CONTRIBUTING.md](CONTRIBUTING.md). Licensed [MIT](LICENSE).
