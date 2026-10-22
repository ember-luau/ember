## Development

```bash
cargo build                  # target/debug/embr
cargo test                   # unit tests, no network
cargo fmt --check            # formatting
cargo clippy --all-targets   # keep this at zero warnings
```

`cargo test` does not rebuild the binary. Run `cargo build` before testing
`target/debug/embr` by hand or you will run stale code.

Source is grouped by what the code talks to:

| Folder | Contents |
| --- | --- |
| `src/commands/` | One file per subcommand |
| `src/project/` | Manifest schema and editing, lockfile, installed-package inspection |
| `src/registry/` | Index lookup, dependency resolution, publish packing |
| `src/net/` | HTTP wrapper, GitHub client, OAuth device flow |
| `src/sys/` | The `~/.ember` layout, subprocesses, git |
| `src/tools/` | Tool storage, release archives, shims |


### Releases

Pushing a `v*` tag builds and publishes the release. The tag has to match
Cargo.toml's version exactly, and asset names stay in the form
`embr-{os}-{arch}[.exe]`, because `embr self update` relies on both. Tags with a
`-` in them (`v0.1.0-beta.1`) are marked pre-release and stay invisible to
`self update`.

### AUR package

`packaging/aur/` holds the PKGBUILD and the `.SRCINFO` of `embr-bin`. The
package installs the `embr-linux-x86_64.zip` asset of a GitHub release. The
AUR keeps its own git repo, so a release does not update it. After each
stable release, on an Arch machine:

1. In `packaging/aur/PKGBUILD`, set `pkgver` to the new version and `pkgrel`
   to 1.
2. In `packaging/aur/`, run `updpkgsums` (from `pacman-contrib`). It downloads
   the new assets and writes their `sha256sums`.
3. Run `makepkg --printsrcinfo > .SRCINFO` in the same folder.
4. Test the build in a copy, so no build output lands in this repo:
   `cp -r packaging/aur /tmp/embr-bin && cd /tmp/embr-bin && makepkg -f`.
5. Commit both files here.
6. Copy `PKGBUILD` and `.SRCINFO` into a clone of
   `ssh://aur@aur.archlinux.org/embr-bin.git`, then commit and push there.

The push needs an AUR account with an SSH key. The first push to that URL
creates the package.

Commits follow [conventional commits](https://www.conventionalcommits.org).
