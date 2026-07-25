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


### Releases

Pushing a `v*` tag builds and publishes the release. The tag has to match
Cargo.toml's version exactly, and asset names stay in the form
`lpm-{os}-{arch}[.exe]`, because `lpm self update` relies on both. Tags with a
`-` in them (`v0.1.0-beta.1`) are marked pre-release and stay invisible to
`self update`.

Commits follow [conventional commits](https://www.conventionalcommits.org).
