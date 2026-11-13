#!/bin/sh
set -eu

PLUGIN_DIR=$1
. "$PLUGIN_DIR/scripts/plugin-helpers.sh"
[ -n "${TMUX:-}" ] || fail 'load agent-usage.tmux from inside tmux (for example with run-shell)'
SOCKET=${TMUX%%,*}
tmux_cmd() { tmux -S "$SOCKET" "$@"; }
option() {
    value=$(tmux_cmd show-option -gqv "$1")
    printf '%s' "${value:-$2}"
}
diagnostic() {
    tmux_cmd set-option -g @agent-usage-diagnostic "$*"
    tmux_cmd display-message "tmux-agent-usage: $*"
    printf 'tmux-agent-usage: %s\n' "$*" >&2
}

# Canonicalize aliases in a private, short-lived table. Read the complete prefix
# table: table/key filters are not consistent across tmux versions.
key_token() {
    probe="agent-usage-probe-$$"
    tmux_cmd bind-key -T "$probe" "$1" run-shell : || return 1
    token=$(tmux_cmd list-keys | AGENT_USAGE_PROBE="$probe" awk '{for (i = 1; i < NF; i++) if ($i == "-T" && $(i + 1) == ENVIRON["AGENT_USAGE_PROBE"]) {print $(i + 2); exit}}')
    tmux_cmd unbind-key -a -T "$probe"
    [ -n "$token" ] || return 1
    printf '%s' "$token"
}
binding() {
    tmux_cmd list-keys | AGENT_USAGE_KEY_TOKEN="$1" awk '{
        for (i = 1; i < NF; i++) if ($i == "-T" && $(i + 1) == "prefix" && $(i + 2) == ENVIRON["AGENT_USAGE_KEY_TOKEN"]) {
            rest = $0
            for (j = 1; j <= i + 2; j++) {
                printf "%s ", $j
                sub(/^[[:space:]]*[^[:space:]]+[[:space:]]*/, "", rest)
            }
            print rest
            break
        }
    }'
}

# Serialize only the short configuration phase, never a download.
tmux_cmd wait-for -L agent-usage-load
trap 'tmux_cmd wait-for -U agent-usage-load' 0
trap 'exit 1' 1 2 15
tmux_cmd set-option -g @agent-usage-diagnostic ''

EXTERNAL=$(option @agent-usage-bin '')
if [ -n "$EXTERNAL" ]; then
    case "$EXTERNAL" in
        */*) BINARY=$EXTERNAL ;;
        *) BINARY=$(command -v "$EXTERNAL" || :) ;;
    esac
    if [ -z "$BINARY" ] || [ ! -f "$BINARY" ] || [ ! -x "$BINARY" ]; then
        diagnostic "@agent-usage-bin is not an executable: $EXTERNAL; install it or correct the option and reload"
        exit 1
    fi
else
    VERSION=$(plugin_version "$PLUGIN_DIR")
    if ! TARGET=$(release_target); then
        diagnostic 'unsupported release target; install with Cargo and set @agent-usage-bin, then reload'
        exit 1
    fi
    DESTINATION="$PLUGIN_DIR/bin/v$VERSION/$TARGET"
    BINARY="$DESTINATION/tmux-agent-usage"
    if installed_binary "$DESTINATION" "$TARGET"; then
        link_current "$PLUGIN_DIR" "$VERSION" "$TARGET" ||
            diagnostic "$PLUGIN_DIR/bin/tmux-agent-usage is not a symlink; remove it and reload to restore the stable path"
    else
        if [ -e "$DESTINATION" ] || [ -L "$DESTINATION" ]; then
            diagnostic "invalid managed installation at $DESTINATION; remove it and reload or set @agent-usage-bin"
            exit 1
        fi
        mkdir -p "$PLUGIN_DIR/bin"
        # Redirect every inherited descriptor: loading is prompt even while a
        # slow/offline release server is reached. Status jobs only check -x.
        (
            umask 077
            log="$PLUGIN_DIR/bin/.install-$$.log"
            if "$PLUGIN_DIR/scripts/install-release.sh" "$PLUGIN_DIR" > "$log" 2>&1; then
                mv "$log" "$PLUGIN_DIR/bin/install.log"
                tmux_cmd refresh-client -S || :
            else
                mv "$log" "$PLUGIN_DIR/bin/install.log"
                diagnostic "managed installation failed; see $PLUGIN_DIR/bin/install.log; reload to retry or set @agent-usage-bin"
            fi
        ) </dev/null >/dev/null 2>&1 &
    fi
fi
mkdir -p "$PLUGIN_DIR/bin"

STATUS_FLAGS=''
for role in normal high critical muted reset-soon; do
    case "$role" in
        normal) default=fg=default ;;
        high) default=fg=yellow ;;
        critical) default=fg=red ;;
        muted) default=fg=brightblack ;;
        reset-soon) default=fg=green ;;
    esac
    style=$(option "@agent-usage-style-$role" "$default")
    STATUS_FLAGS="$STATUS_FLAGS --style-$role $(format_word "$style")"
done

for agent in claude codex; do
    # Errors stay in a bounded per-agent log, never in the status body. The absent
    # managed executable returns immediately while installation is in progress.
    COMMAND="#(if [ -x $(format_word "$BINARY") ]; then $(format_word "$BINARY") status $agent$STATUS_FLAGS 2>$(format_word "$PLUGIN_DIR/bin/status-$agent.log"); fi)"
    OLD=$(tmux_cmd show-option -gqv "@agent-usage-internal-$agent")
    for side in status-left status-right; do
        BEFORE=$(tmux_cmd show-option -gv "$side")
        AFTER=$(replace_literal "$BEFORE" "$OLD" "$COMMAND")
        AFTER=$(replace_literal "$AFTER" "#{agent_usage_$agent}" "$COMMAND")
        if [ "$AFTER" != "$BEFORE" ]; then
            tmux_cmd set-option -g "$side" "$AFTER"
        fi
    done
    tmux_cmd set-option -g "@agent-usage-internal-$agent" "$COMMAND"
done

AGENTS=$(option @agent-usage-agents 'claude codex')
POPUP="$(format_word "$BINARY") popup #{q:client_name} #{client_width} #{client_height} --agents $(format_word "$AGENTS")"
RESETS=$(option @agent-usage-claude-resets off)
case "$RESETS" in
    on) POPUP="$POPUP --claude-resets" ;;
    off) : ;;
    *) diagnostic '@agent-usage-claude-resets must be on or off'; exit 1 ;;
esac
for role in text muted dim ok high critical accent border; do
    color=$(option "@agent-usage-popup-$role" '')
    if [ -n "$color" ]; then
        POPUP="$POPUP --popup-$role $(format_word "$color")"
    fi
done
tmux_cmd set-option -g @agent-usage-popup-command "$POPUP"

KEY=$(option @agent-usage-popup-key A)
OLD_KEY=$(tmux_cmd show-option -gqv @agent-usage-internal-key)
OLD_BINDING=$(tmux_cmd show-option -gqv @agent-usage-internal-binding)
if [ -n "$OLD_KEY" ]; then
    OLD_TOKEN=$(key_token "$OLD_KEY")
    CURRENT=$(binding "$OLD_TOKEN")
    if [ "$CURRENT" = "$OLD_BINDING" ] && [ "$KEY" != "$OLD_KEY" ]; then
        tmux_cmd unbind-key -T prefix "$OLD_KEY"
    fi
fi
tmux_cmd set-option -gu @agent-usage-internal-key
tmux_cmd set-option -gu @agent-usage-internal-binding
case "$KEY" in
    none|off) exit 0 ;;
esac
if ! KEY_TOKEN=$(key_token "$KEY"); then
    diagnostic "invalid @agent-usage-popup-key: $KEY; choose a tmux key name or none"
    exit 1
fi
CURRENT=$(binding "$KEY_TOKEN")
if [ -n "$CURRENT" ] && { [ "$KEY" != "$OLD_KEY" ] || [ "$CURRENT" != "$OLD_BINDING" ]; }; then
    diagnostic "prefix $KEY is already bound; preserved it. Choose @agent-usage-popup-key or set it to none"
    exit 0
fi
tmux_cmd bind-key -T prefix "$KEY" run-shell -b '#{E:@agent-usage-popup-command}'
tmux_cmd set-option -g @agent-usage-internal-key "$KEY"
tmux_cmd set-option -g @agent-usage-internal-binding "$(binding "$KEY_TOKEN")"
