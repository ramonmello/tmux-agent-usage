#!/bin/sh
set -eu

if [ "$#" -ne 1 ] || [ -z "$1" ]; then
    echo 'usage: install.sh ROOT' >&2
    exit 2
fi

SCRIPT_DIR=$(CDPATH= cd -- "$(dirname "$0")" && pwd -P)
PLUGIN_DIR=$(CDPATH= cd -- "$SCRIPT_DIR/.." && pwd -P)

exec cargo install --locked --path "$PLUGIN_DIR" --root "$1" --force
