#!/bin/sh
set -eu

PLUGIN_DIR=$(CDPATH= cd -- "$(dirname "$0")" && pwd -P)
exec "$PLUGIN_DIR/scripts/tmux-load.sh" "$PLUGIN_DIR"
