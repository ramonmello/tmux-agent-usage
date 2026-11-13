#!/bin/sh
# Shared POSIX shell helpers. No provider files or active installation paths.

fail() {
    printf 'tmux-agent-usage: %s\n' "$*" >&2
    exit 1
}

plugin_version() {
    sed -n 's/^version = "\([0-9][0-9.]*\)"$/\1/p' "$1/Cargo.toml" | head -n 1
}

release_target() {
    system=$(uname -s)
    machine=$(uname -m)
    case "$system:$machine" in
        Darwin:arm64|Darwin:aarch64) printf '%s' aarch64-apple-darwin ;;
        Darwin:x86_64) printf '%s' x86_64-apple-darwin ;;
        Linux:aarch64|Linux:arm64) printf '%s' aarch64-unknown-linux-musl ;;
        Linux:x86_64|Linux:amd64) printf '%s' x86_64-unknown-linux-musl ;;
        *) fail "no release binary for $system/$machine; install with Cargo into your chosen root and set @agent-usage-bin to that executable" ;;
    esac
}

sha256() {
    if command -v sha256sum >/dev/null 2>&1; then
        sha256sum "$1" | cut -d ' ' -f 1
    elif command -v shasum >/dev/null 2>&1; then
        shasum -a 256 "$1" | cut -d ' ' -f 1
    else
        fail 'install sha256sum or shasum to verify release checksums'
    fi
}

compatible_binary() {
    header=$(od -An -tx1 -N8 "$1" | tr -d ' \n')
    case "$2:$header" in
        x86_64-apple-darwin:cffaedfe07000001|aarch64-apple-darwin:cffaedfe0c000001)
            kind=$(od -An -tx1 -j12 -N4 "$1" | tr -d ' \n')
            [ "$kind" = 02000000 ] ;;
        *-unknown-linux-musl:7f454c46020101??)
            kind=$(od -An -tx1 -j16 -N4 "$1" | tr -d ' \n')
            case "$2:$kind" in
                x86_64-*:02003e00|x86_64-*:03003e00|aarch64-*:0200b700|aarch64-*:0300b700) return 0 ;;
                *) return 1 ;;
            esac ;;
        *) return 1 ;;
    esac
}

installed_binary() {
    [ -f "$1/tmux-agent-usage" ] && [ -x "$1/tmux-agent-usage" ] &&
        [ -f "$1/binary.sha256" ] &&
        compatible_binary "$1/tmux-agent-usage" "$2" &&
        [ "$(sha256 "$1/tmux-agent-usage")" = "$(cat "$1/binary.sha256")" ]
}

# Point the stable PLUGIN_ROOT/bin/tmux-agent-usage at the selected managed
# version, so a Claude Code statusLine can use one path across plugin updates.
# The relative link is replaced atomically; anything else at that path is a
# user's file and is left alone.
link_current() {
    link="$1/bin/tmux-agent-usage"
    target="v$2/$3/tmux-agent-usage"
    if [ -L "$link" ]; then
        [ "$(readlink "$link")" != "$target" ] || return 0
    elif [ -e "$link" ]; then
        printf 'tmux-agent-usage: %s is not a symlink; left unchanged\n' "$link" >&2
        return 1
    fi
    staged="$1/bin/.tmux-agent-usage.$$"
    rm -f "$staged"
    ln -s "$target" "$staged" && mv -f "$staged" "$link"
}

shell_word() {
    printf "'%s'" "$(printf '%s' "$1" | sed "s/'/'\\\\''/g")"
}

# tmux expands formats (and scans parentheses) before the shell sees a #() or
# run-shell command. Encode special bytes, including #, %, quotes and parens,
# so neither tmux nor the shell can interpret an option value as code/format.
# printf is a POSIX shell builtin; this never spawns a provider or installer.
format_word() {
    case "$1" in
        *[!a-zA-Z0-9_./,:=-]*|'')
            octal=$(printf '%s' "$1" | od -An -v -to1 | awk '{for (i = 1; i <= NF; i++) printf "\\%s", $i}')
            printf '"$(printf %s)"' "$(shell_word "$octal")" ;;
        *) shell_word "$1" ;;
    esac
}

replace_literal() {
    remaining=$1
    needle=$2
    replacement=$3
    [ -n "$needle" ] || { printf '%s' "$remaining"; return; }
    while :; do
        case "$remaining" in
            *"$needle"*)
                printf '%s%s' "${remaining%%"$needle"*}" "$replacement"
                remaining=${remaining#*"$needle"} ;;
            *) printf '%s' "$remaining"; return ;;
        esac
    done
}
