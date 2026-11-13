#!/bin/sh
set -eu
umask 077

[ "$#" -eq 1 ] || { echo 'usage: install-release.sh PLUGIN_ROOT' >&2; exit 2; }
PLUGIN_DIR=$(CDPATH= cd -- "$1" && pwd -P)
. "$PLUGIN_DIR/scripts/plugin-helpers.sh"
VERSION=$(plugin_version "$PLUGIN_DIR")
[ -n "$VERSION" ] || fail 'cannot read plugin version from Cargo.toml'
TARGET=$(release_target)
PARENT="$PLUGIN_DIR/bin/v$VERSION"
DESTINATION="$PARENT/$TARGET"
mkdir -p "$PARENT"

# A concurrent loader leaves the owner to finish. A stale lock is deliberately
# not broken: the diagnostic explains recovery without racing another writer.
LOCK="$DESTINATION.lock"
if ! mkdir "$LOCK" 2>/dev/null; then
    [ -d "$LOCK" ] || fail "cannot create install lock $LOCK"
    printf 'tmux-agent-usage: installation already locked at %s; if no installer is running, remove that stale lock and reload\n' "$LOCK" >&2
    exit 0
fi
STAGING=''
trap '[ -z "$STAGING" ] || rm -rf "$STAGING"; rmdir "$LOCK"' 0
trap 'exit 1' 1 2 15

if installed_binary "$DESTINATION" "$TARGET"; then
    link_current "$PLUGIN_DIR" "$VERSION" "$TARGET" || :
    exit 0
fi
[ ! -e "$DESTINATION" ] && [ ! -L "$DESTINATION" ] ||
    fail "invalid managed installation at $DESTINATION; remove it and reload, or set @agent-usage-bin to a Cargo/Homebrew executable"

STAGING=$(mktemp -d "$PARENT/.install-$TARGET.XXXXXX")
ASSET="tmux-agent-usage-v$VERSION-$TARGET.tar.gz"
URL="https://github.com/ramonmello/tmux-agent-usage/releases/download/v$VERSION/$ASSET"
command -v curl >/dev/null 2>&1 || fail 'curl is required; install it or set @agent-usage-bin to a Cargo/Homebrew executable'
for suffix in '' .sha256; do
    curl --fail --location --silent --show-error --connect-timeout 5 --max-time 30 \
        --output "$STAGING/$ASSET$suffix" "$URL$suffix" ||
        fail "download failed for $URL$suffix; reload to retry, or set @agent-usage-bin to a Cargo/Homebrew executable"
done
ACTUAL=$(sha256 "$STAGING/$ASSET")
EXPECTED=$(cat "$STAGING/$ASSET.sha256")
[ "$EXPECTED" = "$ACTUAL  $ASSET" ] || fail "checksum mismatch for $ASSET; no binary installed"

# Only one regular file is allowed. Stream it into a fresh file rather than
# extracting archive paths, permissions or links into the filesystem.
[ "$(tar -tzf "$STAGING/$ASSET")" = tmux-agent-usage ] || fail "invalid archive contents in $ASSET"
[ "$(tar -tvzf "$STAGING/$ASSET" | cut -c1)" = '-' ] || fail "archive member is not a regular file in $ASSET"
tar -xOzf "$STAGING/$ASSET" tmux-agent-usage > "$STAGING/tmux-agent-usage" || fail "cannot unpack $ASSET"
compatible_binary "$STAGING/tmux-agent-usage" "$TARGET" || fail "incompatible executable in $ASSET; no binary installed"
sha256 "$STAGING/tmux-agent-usage" > "$STAGING/binary.sha256"
chmod 755 "$STAGING/tmux-agent-usage"
rm "$STAGING/$ASSET" "$STAGING/$ASSET.sha256"
mv "$STAGING" "$DESTINATION"
STAGING=''
link_current "$PLUGIN_DIR" "$VERSION" "$TARGET" || :
printf 'tmux-agent-usage: installed v%s (%s)\n' "$VERSION" "$TARGET"
