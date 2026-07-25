# lpm — Luau Package Manager (CLI)

Rust CLI (`lpm`) for managing Luau packages across ecosystems: pesde indices
and Wally indices today, lpm's own package API once it exists. Windows is the
primary dev platform.

**In flight:** lpm's own index (the luaupm git index) and the GitHub-based
publish flow were deliberately gutted; a first-party API replaces both. Every
hole is marked `TODO(api)` in the source — grep for it. `lpm publish` packs and
then stops at `PublishUnavailable`, and a dependency that names no index errors
with `NoDefaultIndex` unless the project defines a `default` one.

## Build / test / run

```
cargo build          # builds target/debug/lpm.exe
cargo test           # unit tests only (no network)
cargo fmt --check    # formatting lint (cargo fmt to fix)
cargo clippy --all-targets   # clippy lints; keep at zero warnings
```

- **Important:** `cargo test` does NOT rebuild `target/debug/lpm.exe`. Run
  `cargo build` before manually testing the binary or you'll run stale code.
- End-to-end testing needs the network (clones real indices, downloads real
  packages). Do it in a scratch dir with a hand-written `lpm.toml`
  (see "Manifest" below), then run `lpm add ... --index ...` and `lpm install`.
  Known-good test packages: `evaera/promise` (wally, no deps),
  `sleitnick/knit` (wally, has transitive deps), `pesde/hello` (pesde).
- `lpm init` / `add` / `uninstall` are interactive (inquire prompts); `add`
  skips its prompt when `--index` is passed. Don't run interactive commands
  from non-interactive shells.

## Commands

- `init` — interactive manifest wizard; also git-ignores `packages/`.
- `run <name>` — runs the `[scripts]` entry of that name through the platform
  shell (sh -c / cmd /C), forwarding a non-zero exit code.
- `studio` — subcommand group; bare `lpm studio` prints its help (exit 2,
  clap arg_required_else_help) rather than launching anything.
- `studio init` — inquire wizard that writes the `[studio]` table: either
  `universe` + `place` IDs or a local `file` (default guessed from a Rojo
  `*.project.json` `name`).
- `studio open` — opens what `[studio]` describes, validating everything
  first: table present/complete, no unknown keys, IDs non-zero, file exists
  and is .rbxl/.rbxlx, and Studio looks installed (registry
  shell\open\command on Windows, RobloxStudio.app bundle on macOS, xdg-mime
  best-effort on Linux — the opener still reports what pre-checks can't see).
  IDs become the `roblox-studio:1+launchmode:edit+task:EditPlace+...` deep
  link the website's Edit button uses; files go through the OS association.
  Both launch via ShellExecuteW on Windows, `open` on macOS, `xdg-open` on
  Linux. No credentials involved — Studio authenticates itself after handoff,
  and post-launch errors are Studio's to show.
- `add <scope>/<name> [--version <req>] [--index <key>] [--alias <name>]` —
  resolves first (fails before touching the manifest), then edits `lpm.toml`
  via `toml_edit` (preserves comments/formatting). Without `--index` it asks
  via a text prompt; empty input means the `default` index, which now has to be
  defined under [indices].
- `install` / `i` — re-resolves everything each run (so `version = "^"` picks
  up new releases), wipes and rebuilds each environment's output folder,
  writes `lpm.lock`. `--locked` installs exactly from the lock, no resolution.
- `publish [--dry-run] [--index <key>]` — packs the project into a .tar.gz
  (optional `include`/`exclude` path lists under `[package]`; lpm.toml always
  ships), then stops: uploading is the API's job. `--dry-run` prints the
  archive name, its size, and the file list. Everything after packing (upload,
  index entry, scope ownership) was removed; see the TODO(api) block at the top
  of `commands/publish.rs` for what the old flow did.
- GitHub auth: OAuth device flow (`net::auth::ensure_logged_in`) with the token
  cached at `~/.lpm/credentials.toml` (0600 on unix). Nothing calls it right
  now — it survives in case the API accepts a GitHub token.
- `self install|update|uninstall` — self-management (PATH edits on Windows via
  winreg; update pulls GitHub releases from luaupm/cli).

## Manifest (lpm.toml)

```toml
[package]
name = "scope/name"          # lowercase; validated
version = "0.1.0"
repository = "owner/repo"    # or GitHub URL; required only for publish
include = ["src"]            # optional; paths packed by publish
exclude = ["tests"]          # optional; lpm.toml always ships

[target]
environment = "shared"       # shared | server | lune | luau | lute
main = "init.luau"           # entry point

[config]                     # all optional; defaults shown as packages/<env>
shared-packages-out = "packages/shared"
server-packages-out = "packages/server"
lune-packages-out   = "packages/lune"
luau-packages-out   = "packages/luau"
lute-packages-out   = "packages/lute"

[indices]
wally = "https://github.com/UpliftGames/wally-index"
pesde = "https://github.com/pesde-pkg/index"
default = "https://github.com/pesde-pkg/index"  # used by deps naming no index

[dependencies]
Chief = { name = "chief/core", version = "^" }                # default index
Other = { name = "user/pkg", version = "^", index = "wally" } # named index

[studio]                           # what `lpm studio open` opens; one form only
universe = 13058                   # experience (universe) ID...
place = 1818                       # ...plus its place ID
# file = "game.rbxl"               # or a local place file instead of IDs

[tools]
rojo = "rojo-rbx/rojo@7.7.0"      # owner/repo@version, key is the shim alias

[scripts]                          # run with `lpm run <key>`
build = "rojo build -o game.rbxl"
```

- A dependency naming no index uses the `default` key under [indices]; with no
  such key it fails with `NoDefaultIndex` (TODO(api): the API becomes the
  fallback).
- `version = "^"` (or `*`) means "latest"; otherwise standard semver reqs.
- Environments translate: pesde `roblox`→shared, `roblox_server`→server;
  wally realm `shared`/`server` map directly.

## Install layout (wally-style linker)

```
packages/shared/              <- per-env output (configurable via [config])
  Chief.luau                  <- link file: return require("./.lpm/chief_core/<entry>")
  .lpm/
    chief_core/               <- actual extracted package
```

- Link names: `[dependencies]` alias for direct deps, package short name for
  transitive deps.
- Entry point detection order: lpm.toml `[target].main` → pesde.toml
  `[target].lib` → Rojo `default.project.json` tree `$path` → conventional
  `init.luau`/`src/init.luau`/etc. Extensions stripped (Luau string requires
  reject them). No entry found → warning, no link file.
- Environment detection when the index doesn't know it: extracted package's
  `lpm.toml` → `pesde.toml` → `wally.toml` (realm).
- Transitive deps flatten to the top-level env folders, deduped by package
  name; incompatible version requirements are a hard error.

## Index formats

- **Wally** (detected by root `config.json`): package files at `scope/name`
  are JSON-lines, one entry per version. Zips from
  `{api}/v1/package-contents/{scope}/{name}/{version}`.
- **pesde** (root `config.toml`): package files are TOML keyed by
  `"<version> <target>"`. Tarballs from
  `{api}/v1/packages/{name urlencoded}/{version}/{target}/archive`.
- **lpm's own format is gone** (TODO(api)): it was the pesde format plus a
  per-entry `download` URL and an optional config `download` template, which is
  what let it work with no registry server. Also removed with it: scope
  ownership (`<scope>/owners.toml`), `github_oauth_id` in index configs, and
  private indices (`private = true`, whose downloads carried the user's GitHub
  token as a Bearer header and only ever to GitHub-owned hosts — keep that
  restraint when the API needs credentials).
- Indices are git repos, shallow-cloned/pulled to `~/.lpm/index-cache/`;
  offline falls back to the stale cache with a warning.

### Hard-won HTTP quirks (do not undo)

- Wally's API returns **426** without a `Wally-Version: 0.3.2` header.
- pesde's registry replies **307** to object storage; ureq 2 only auto-follows
  301/302/303, so `http_get_bytes` follows redirects manually.
- ureq transparently decodes `Content-Encoding: gzip`, so tarballs are
  **magic-byte sniffed** (`1f 8b`) before gunzipping.

## Module map

Source is grouped by what code talks to: the project's own files, remote
package sources, the network, the local machine, and the CLI surface.

**`src/project/`** — the files a project owns.
- `manifest/mod.rs` — lpm.toml schema, `Environment` enum + translations,
  `packages_out()`, `script()`, version-req parsing (`^` special case).
- `manifest/edit.rs` — `ManifestDoc`/`Scope`: the toml_edit side of editing
  lpm.toml or the global tools file (open, get/create a table, write back).
  Every command that edits a manifest goes through it, so comments and
  formatting survive; nothing else should hand-roll the toml_edit dance.
- `lockfile.rs` — `lpm.lock` (TOML, `version = 1`, `[[package]]` entries with
  baked download URLs so `--locked` never consults an index).
- `package.rs` — reading an *installed* package: entry point detection,
  environment detection, link-file contents, single-dir flattening.
- `hooks.rs` — stub: lifecycle names that will map to `[scripts]` entries.

**`src/registry/`** — where packages come from (the layer the API replaces).
- `index/mod.rs` — git cache, kind detection, download/extract dispatch.
- `index/wally.rs`, `index/pesde.rs` — per-format parsing/resolution.
- `resolver.rs` — BFS over the dep graph, carries link names.
- `pack.rs` — tarball packing for publish.

**`src/net/`** — remote services.
- `http/` — ureq wrapper (`get_json`, `post_form`, `get_bytes`, ...), error
  mapping, response shapes.
- `github.rs` — GitHub REST client. Only the release endpoints are still
  called; the rest is leftover from publishing (TODO(api)).
- `auth.rs` — GitHub OAuth device flow + credential store (TODO(api): unused).

**`src/sys/`** — the local machine.
- `paths.rs` — the whole `~/.lpm` layout (bin, tools, tools.toml,
  credentials.toml, index-cache) plus `same_file`/`with_suffix`. Nothing else
  builds those paths by hand.
- `process.rs` — handing off to other programs: `shell()` builds a sh/cmd
  command, `exec()` replaces the lpm process (unix) or waits, `wait()` always
  waits and returns the exit code. On Windows the script is passed with
  `raw_arg`, not `arg`: std escapes inner quotes MSVCRT-style (`\"`) and
  cmd.exe does not understand that, so scripts with quoted paths break.
- `git.rs` — `run` (index clone/pull) and `output` (init's prompt defaults),
  plus remote-URL normalization.

**`src/tools/`** — GitHub-released binaries a project pins.
- `mod.rs` — storage layout, install, shorthand names.
- `archive.rs` — asset selection per os/arch, unpacking, executable picking.
- `shim.rs` — shims, alias resolution, which tools a scope pins.

**Top level** — `main.rs` (CLI wiring), `commands/` (one file per subcommand),
`error.rs` (single `thiserror` enum for the whole crate), `ui.rs` (theming;
see below).

## UI conventions

- Accent color `#e61048` lives ONLY in `ui::ACCENT`; everything derives from it
  (inquire render config, clap help styles, error/success lines).
- Errors print as accent `✗ <message>` via `ui::print_error`; successes as
  accent `✓` + default text via `ui::print_success`.
- Progress: `ui::with_spinner` / `ui::with_progress` wrap work so the spinner
  or bar is always cleared, including on errors; `ui::progress_bar` /
  `ui::spinner` are the raw builders (indicatif, accent-styled — ACCENT stays
  the single color source). `ui::success_line` is the `✓ ` string, for printing
  through a live bar, and `ui::plural` handles count suffixes.
- Every successful command ends with a dimmed "Done in 142ms" via
  `ui::print_elapsed`; `ui::format_duration` renders "<1ms", "142ms",
  "1.42s", "1m 12s". Tool-shim dispatch prints no timer.
- clap output is customized: `ui::help_styles()` (accent literals, plain
  headers), and `main::parse_cli()` intercepts clap errors to restyle
  `error:` → `✗`, capitalize the message, and strip `tip:` labels. Help and
  version output pass through untouched (exit codes preserved: 2 for usage
  errors, 1 for runtime errors).

## Releases

- Tag pushes (`v*`) trigger `.github/workflows/release.yml`. The tag must
  exactly match Cargo.toml's version (a `verify-version` job gates the build)
  because `lpm self update` compares versions.
- Pre-release tags (containing `-`, e.g. `v0.1.0-alpha.3`) get GitHub's
  pre-release badge and are invisible to `lpm self update` (it polls
  `releases/latest`).
- Asset names must stay `lpm-{os}-{arch}[.exe]` — `self update` derives them
  from `std::env::consts`.

## Conventions

- Conventional commits (`feat(cli): ...`, `chore: ...`).
- The repo's own `lpm.toml` at the root is a local test manifest (gitignored),
  not part of the build.
