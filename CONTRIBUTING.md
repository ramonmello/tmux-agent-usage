# Contributing

Thanks for helping. Bug reports with the details listed in
[troubleshooting](docs/troubleshooting.md#reporting-a-problem) are very
welcome, as are pull requests. For anything beyond a small fix, open an issue
first so we can agree on the approach.

## Making a change

1. Fork the repository and create a branch from **`next`**, where development
   happens. `main` only changes when a version is released, because TPM
   installs from it.
2. Make the change with tests (see below for the parts that need care).
3. Run `sh scripts/ci.sh` and make sure it passes.
4. Open a pull request **against `next`**, describing what changed and why.
   GitHub suggests `main`; switch the base to `next` (it can also be changed
   after opening the pull request).

You don't need to bump the version or edit `CHANGELOG.md`; the maintainer does
that when releasing. If you'd like to, add a line under `## [Unreleased]` in
the changelog describing the change for users.

## Build and test

You need Rust 1.88 or later, tmux 3.4+, Python 3.9+ and Git. Everything runs
through Cargo:

```sh
cargo build --release
cargo test --locked
```

The suite takes about a minute. It uses fake clients, fixture credentials,
temporary `HOME`/XDG directories and tmux servers on private sockets, so it
never touches your accounts, caches or running tmux. A missing or broken tmux
fails the suite instead of skipping tests.

Before pushing, run what CI runs, with disposable provider paths:

```sh
sh scripts/ci.sh
```

That is `cargo fmt --check`, Clippy with warnings denied, the tests, the
package list and `cargo publish --dry-run` (which never uploads). It also runs
the two slow checks that `cargo test` skips by default (`#[ignore]`): the
release build and the package test, which rebuilds and reruns the suite from a
clean copy. To run only those, use `cargo test --locked -- --ignored`. CI runs it on
macOS arm64 and x86_64 and Linux arm64 and x86_64 (musl), with Rust 1.88.0
and stable.

## Developing against your own tmux

Load the plugin from your clone and link the binary where tmux and Claude Code
find it, so a rebuild shows up on the next status refresh:

<!-- test:tpm-local -->
```tmux
run-shell "'/path/to/tmux-agent-usage/agent-usage.tmux'"
```

```sh
ln -sf "$PWD/target/release/tmux-agent-usage" ~/.local/bin/tmux-agent-usage
```

Set `@agent-usage-bin` to that binary (or a name on tmux's `PATH`) so the
plugin doesn't try to download a release. After changing Rust code, run
`cargo build --release`; after changing a script or option, reload tmux.

To install the current source into a directory of your choice instead:

<!-- test:source-install -->
```sh
install_root=$(mktemp -d "${TMPDIR:-/tmp}/tmux-agent-usage-install.XXXXXX")
sh scripts/install.sh "$install_root"
"$install_root/bin/tmux-agent-usage" help
printf '%s\n' "$install_root/bin/tmux-agent-usage"
```

Rerun it with the same root to update:

<!-- test:source-update -->
```sh
sh scripts/install.sh "$install_root"
```

## Project layout

| Path | Contents |
| --- | --- |
| `src/` | The binary: status bodies, the popup, the Claude tap, provider readers |
| `agent-usage.tmux` | TPM entrypoint; runs `scripts/tmux-load.sh` |
| `scripts/tmux-load.sh` | Reads the options once, replaces the placeholders, binds the key, starts a managed install when needed |
| `scripts/install-release.sh` | Downloads and verifies the release binary for the plugin's version |
| `scripts/plugin-helpers.sh` | Shell helpers shared by the two scripts above |
| `scripts/install.sh` | `cargo install` of this source into an explicit root |
| `scripts/release.py` | Builds and assembles release assets ([releasing](docs/releasing.md)) |
| `tests/` | Integration tests, fixtures and byte-exact goldens |

The binary only takes arguments and flags; the shell scripts own everything
about tmux options. Keep it that way.

## Tests that need care

**Goldens.** `tests/goldens/baseline/` holds the exact bytes of status bodies,
themed status lines and popups. Tests never update them. If a change is meant
to alter output, regenerate and review every byte of the diff:

```sh
AGENT_USAGE_UPDATE_GOLDENS=1 cargo test --locked --bin tmux-agent-usage baseline_goldens
```

The goldens don't depend on your machine: they use a fixed clock, UTC, a
temporary `HOME` and the sample themes in `tests/fixtures/`. The themed status
lines use a Gruvbox theme with two icons from a private font, U+100006 and
U+100007; they show as boxes without that font, which is expected. A pull
request that changes goldens must say why the output changed.

**Documented snippets.** Code blocks marked `<!-- test:NAME -->` in the README
and in `docs/` and this file are executed by the tests. Edit them together with
the tests that use them.

**Packaged files.** The crate only ships what `include` in `Cargo.toml` lists,
and the package test builds and runs the whole suite from a clean copy. A new
file the tests need must be added there. Plain `cargo test` skips the package
test, so a missing entry only shows up in `sh scripts/ci.sh`.

## License

By contributing, you agree that your contributions are licensed under the
[MIT License](LICENSE).
