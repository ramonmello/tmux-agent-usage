# Releasing

This is the maintainer's checklist for publishing a version. Contributors
don't need it; see [CONTRIBUTING.md](../CONTRIBUTING.md) instead.

## Why `main` must match a release

The TPM installer downloads the release that matches the version in the
plugin's `Cargo.toml`, and TPM users follow `main`. So **`main` must always
match a published release**. Development happens on `next`, where pull
requests are merged; `main` moves forward only when a version is released. Users who want to stay on one version can
pin it with `set -g @plugin 'ramonmello/tmux-agent-usage#v0.1.0'`.

Taking the version from the checkout keeps the scripts and the binary from the
same release, always. The alternatives were worse: downloading the latest
release would pair old scripts with a new binary (or the reverse) and make the
same checkout install different things on different days, and building on the
user's machine would require a Rust toolchain. What actually breaks is a
version on `main` with no published binary, or scripts on `main` that need a
binary newer than that version; keeping `main` equal to a release rules out
both without judging each change.

## Steps

Versions follow [Semantic Versioning](https://semver.org) and the changelog
follows [Keep a Changelog](https://keepachangelog.com). Before releasing, make
sure `## [Unreleased]` in `CHANGELOG.md` lists every user-visible change,
including those from pull requests that didn't add an entry.

Every step that publishes something is done by the maintainer, one at a time.
The examples release `0.2.0`; replace it with your version.

1. **Prepare the version on `next`.** Set `version = "0.2.0"` in
   `Cargo.toml`, update the `--version` in the README's `cargo install` example
   (a test checks they match), and rename `[Unreleased]` in `CHANGELOG.md` to
   `[0.2.0] - YYYY-MM-DD` with its link at the bottom. Then run the checks with
   the minimum Rust:

   ```sh
   cargo update --workspace   # records the new version in Cargo.lock only
   RUSTUP_TOOLCHAIN=1.88.0 sh scripts/ci.sh
   git commit -am "chore: release v0.2.0"
   git push origin next
   ```

   Wait for CI to pass on all eight lanes for that commit.

2. **Move `main` to that commit.** A fast-forward keeps `main` and `next` on
   the same commit, so the tag below matches what was tested:

   ```sh
   git switch main && git pull --ff-only
   git merge --ff-only next
   git push origin main
   ```

   From here until step 6, TPM users who update get a version with no binary
   yet; their status line stays empty with a diagnostic until the release
   exists. Do the next steps without a pause.

3. **Tag that commit:**

   ```sh
   git tag -a v0.2.0 -m "v0.2.0"
   git push origin v0.2.0
   ```

4. **Build the release assets** with the `release.yml` workflow on the tag. It
   checks that the tag points at the workflow's commit, reruns CI, builds the
   four targets and attests the assets. It publishes nothing.

   ```sh
   gh workflow run release.yml --ref v0.2.0 -f tag=v0.2.0
   # Wait a few seconds for the run to appear, then take its ID.
   run=$(gh run list --workflow release.yml --branch v0.2.0 --limit 1 --json databaseId -q '.[0].databaseId')
   gh run watch "$run" --exit-status
   gh run download "$run" --name tmux-agent-usage-v0.2.0 --dir release-v0.2.0
   ```

   Verify the downloaded files before using them:

   ```sh
   cd release-v0.2.0
   sha256sum -c SHA256SUMS   # macOS: shasum -a 256 -c SHA256SUMS
   gh attestation verify SHA256SUMS --bundle provenance.sigstore.json \
     --repo ramonmello/tmux-agent-usage --source-ref refs/tags/v0.2.0 \
     --source-digest "$(git rev-parse 'v0.2.0^{commit}')" \
     --signer-workflow ramonmello/tmux-agent-usage/.github/workflows/release.yml \
     --deny-self-hosted-runners
   ```

5. **Publish the crate.** A published version can't be deleted, only yanked.
   Pass the crates.io token through the environment for this one command; never
   store it in a file:

   ```sh
   cargo publish --locked --dry-run
   CARGO_REGISTRY_TOKEN=... cargo publish --locked
   ```

6. **Publish the GitHub release** with all fifteen files from step 4 (four
   archives, their `.sha256` and `.json` files, `SHA256SUMS`, the provenance
   file and `provenance.sigstore.json`). The TPM installer downloads the
   archive and `.sha256` by name. Create it as a draft, review it, then publish:

   ```sh
   gh release create v0.2.0 --draft --verify-tag --title v0.2.0 \
     --notes "See CHANGELOG.md" release-v0.2.0/*
   gh release edit v0.2.0 --draft=false
   ```

   Then install through TPM in a throwaway `HOME` and tmux socket to see what a
   new user sees.

7. **Update the Homebrew formula** in
   [`ramonmello/homebrew-tap`](https://github.com/ramonmello/homebrew-tap):
   the tag's commit, the source archive's SHA-256 and the version, following
   its README.

## Urgent fixes

`main` must never move ahead of `next`, or step 2's fast-forward fails. For a
fix that can't wait for the changes already on `next`, branch from `main`
(`git switch -c fix/... main`) and release it as a patch version from that
branch, with the same steps (fast-forwarding `main` to the fix branch in step
2). Then merge `main` back into `next`.

## Release assets

`scripts/release.py` does the local work the workflow runs:

- `source TAG` checks for a clean checkout at the repository root, with `TAG`
  on `HEAD` and matching `Cargo.toml`.
- `build TAG TARGET OUTPUT` builds one target with `--locked` into a new
  directory: `tmux-agent-usage-vX.Y.Z-TARGET.tar.gz`, its `.sha256` and a JSON
  record of the toolchain, flags and digests.
- `assemble TAG INPUTS OUTPUT` combines all four targets, checks them again and
  writes `SHA256SUMS` and the provenance file.

Each archive holds one regular file, `tmux-agent-usage`. The checker rejects
the wrong architecture, macOS binaries with a deployment target other than
11.0 or non-system libraries, and Linux binaries that aren't static. The TPM
installer depends on these exact asset names; don't change them.
