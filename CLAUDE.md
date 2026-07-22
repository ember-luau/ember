# lpm — Luau Package Manager (CLI)

Rust CLI (`lpm`) for managing Luau packages across ecosystems: our own index
(luaupm), pesde indices, and Wally indices. Windows is the primary dev platform.

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
- `add <scope>/<name> [--version <req>] [--index <key>] [--alias <name>]` —
  resolves first (fails before touching the manifest), then edits `lpm.toml`
  via `toml_edit` (preserves comments/formatting). Without `--index` it asks
  via a text prompt; empty input = default luaupm index.
- `install` / `i` — re-resolves everything each run (so `version = "^"` picks
  up new releases), wipes and rebuilds each environment's output folder,
  writes `lpm.lock`. `--locked` installs exactly from the lock, no resolution.
- `publish [--dry-run] [--index <key>]` — packs the project into a .tar.gz
  (optional `include`/`exclude` path lists under `[package]`; lpm.toml always
  ships), uploads it as an asset on a GitHub release tagged `v{version}` of
  `[package].repository` (required for publishing), then forks the index
  repo, writes the entry at `<scope>/<name>` (pesde-style key
  `"<version> <target>"` with lpm's `download` URL plus target/dependencies),
  and opens a PR. First publish to a scope adds `<scope>/owners.toml` in the
  same PR (see "Index formats"). `--dry-run` prints the file list and index
  entry without auth or network mutations.
- GitHub auth: OAuth device flow (`auth::ensure_logged_in`) using
  `github_oauth_id` from the target index's config.toml (pesde's
  `github_oauth_client_id` key accepted as an alias); token cached at
  `~/.lpm/credentials.toml` (0600 on unix). Index config with no oauth id →
  `IndexNotPublishable`.
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
# `default` key overrides the built-in default index

[dependencies]
Chief = { name = "chief/core", version = "^" }                # default index
Other = { name = "user/pkg", version = "^", index = "wally" } # named index
```

- Default index when none specified: `https://github.com/luaupm/index`.
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
- **lpm (ours)**: pesde format + per-entry `download` URL (direct .tar.gz), so
  no registry server is needed. Index config may carry a `download` template
  with `{API_URL}`/`{PACKAGE}`/`{PACKAGE_VERSION}`/`{PACKAGE_TARGET}`.
- Publishable indices carry a `github_oauth_id` in config.toml. Scope
  ownership: `<scope>/owners.toml` (`owners = ["login"]`) — the first
  publisher of a scope adds it, and publishing to a scope whose owners file
  omits you fails with `ScopeOwned`. Enforcement beyond the CLI check is the
  index repo's concern.
- **Private indices** (early, back-burner): pesde-format index with
  `private = true` in config.toml (reference: github.com/luaupm/pindex).
  Cloning uses the user's git credentials as always; `index::download` takes
  an optional token, attached as `Bearer` + `Accept:
  application/octet-stream` ONLY for GitHub-owned hosts (github.com,
  api.github.com, uploads.github.com, objects.githubusercontent.com,
  raw.githubusercontent.com). `install` passes the stored token when logged
  in.
- Indices are git repos, shallow-cloned/pulled to `~/.lpm/index-cache/`;
  offline falls back to the stale cache with a warning.

### Hard-won HTTP quirks (do not undo)

- Wally's API returns **426** without a `Wally-Version: 0.3.2` header.
- pesde's registry replies **307** to object storage; ureq 2 only auto-follows
  301/302/303, so `http_get_bytes` follows redirects manually.
- ureq transparently decodes `Content-Encoding: gzip`, so tarballs are
  **magic-byte sniffed** (`1f 8b`) before gunzipping.

## Module map

- `src/manifest.rs` — lpm.toml schema, `Environment` enum + translations,
  `packages_out()`, version-req parsing (`^` special case).
- `src/index/mod.rs` — git cache, kind detection, download/extract dispatch.
- `src/index/wally.rs`, `src/index/pesde.rs` — per-format parsing/resolution.
- `src/resolver.rs` — BFS over the dep graph, carries link names.
- `src/lockfile.rs` — `lpm.lock` (TOML, `version = 1`, `[[package]]` entries
  with baked download URLs so `--locked` never consults an index).
- `src/auth.rs` — GitHub OAuth device flow + credential store
  (`~/.lpm/credentials.toml`).
- `src/publish/` — `pack.rs` (tarball packing), `index_entry.rs` (index-entry
  + owners.toml generation).
- `src/commands/` — `init`, `add`, `install`, `publish`, `self_cmd`.
- `src/error.rs` — single `thiserror` enum for the whole crate.
- `src/ui.rs` — theming (see below).
- New deps: `indicatif` 0.18, `base64` 0.22.

## UI conventions

- Accent color `#e61048` lives ONLY in `ui::ACCENT`; everything derives from it
  (inquire render config, clap help styles, error/success lines).
- Errors print as accent `✗ <message>` via `ui::print_error`; successes as
  accent `✓` + default text via `ui::print_success`.
- Progress: `ui::progress_bar` / `ui::spinner` (indicatif, accent-styled —
  ACCENT stays the single color source). `ui::success_line` is the `✓ `
  string, for printing through a live bar.
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
