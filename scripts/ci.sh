#!/bin/sh
# Every check CI runs, with disposable provider paths. Run locally before a push.
set -eu
SCRIPT_DIR=$(CDPATH= cd -- "$(dirname "$0")" && pwd -P)
cd "$SCRIPT_DIR/.."
command -v tmux >/dev/null 2>&1 || { echo 'ci.sh: tmux 3.4+ is required' >&2; exit 1; }
command -v python3 >/dev/null 2>&1 || { echo 'ci.sh: Python 3.9+ is required' >&2; exit 1; }

# Preserve toolchain locations while keeping every provider path disposable.
CARGO_HOME=${CARGO_HOME:-"$HOME/.cargo"}
RUSTUP_HOME=${RUSTUP_HOME:-"$HOME/.rustup"}
export CARGO_HOME RUSTUP_HOME
CI_ROOT=$(mktemp -d "${TMPDIR:-/tmp}/agent-usage-ci.XXXXXX")
trap 'rm -rf "$CI_ROOT"' 0
trap 'exit 1' 1 2 15
HOME="$CI_ROOT/home"
XDG_CACHE_HOME="$CI_ROOT/cache"
XDG_CONFIG_HOME="$CI_ROOT/config"
XDG_DATA_HOME="$CI_ROOT/data"
CODEX_HOME="$CI_ROOT/codex"
CLAUDE_CONFIG_DIR="$CI_ROOT/claude"
TZ=UTC0
export HOME XDG_CACHE_HOME XDG_CONFIG_HOME XDG_DATA_HOME CODEX_HOME CLAUDE_CONFIG_DIR TZ
mkdir -p "$HOME"
unset TMUX TMUX_PANE CARGO_REGISTRY_TOKEN

cargo fmt --check
cargo clippy --locked --all-targets -- -D warnings
cargo test --locked -- --include-ignored
cargo package --locked --list
# This verifies the unpacked package and registry metadata; it never uploads.
cargo publish --locked --dry-run
