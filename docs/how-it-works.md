# How it works

The status line never queries anything: it reads files the official clients
already produce. Only opening the popup asks the clients for saved resets.

## Percentages

### Claude Code

Claude Code sends its `rate_limits` (5-hour and 7-day windows) only to the
command configured as its `statusLine`. `tmux-agent-usage claude-tap` is that
command: it saves the usage fields from each update and forwards the input,
byte for byte, to the command you wrap after `--`. The wrapped command keeps
its stdout, stderr and exit code even if recording fails.

- Snapshots live in `$XDG_CACHE_HOME/tmux-agent-usage/claude` (default
  `~/.cache/tmux-agent-usage/claude`), one per session, holding only usage
  windows and when they were observed.
- A snapshot is rewritten only when its values change, and the newest change
  wins, so an idle session can't override fresher numbers. Snapshots older
  than eight days are deleted.
- `rate_limits` first appeared in Claude Code
  [2.1.80](https://github.com/anthropics/claude-code/blob/main/CHANGELOG.md#2180)
  and isn't sent for every account; a missing window shows `–`. Spend limits
  are ignored.
- Usage from claude.ai or the desktop app shows up only after Claude Code's
  next reply.

The tap runs whenever Claude Code updates its status line, inside or outside
tmux, until you remove it from `settings.json`. Removing Claude from your tmux
config doesn't stop it. To keep the wrapped command but stop recording:

<!-- test:tap-disabled -->
```sh
tmux-agent-usage claude-tap --no-record -- ccstatusline
```

### Codex

Codex needs no setup. Each `token_count` event in its session logs
(`$CODEX_HOME/sessions`, default `~/.codex/sessions`) carries `rate_limits`.
The plugin reads the end of the most recent logs and keeps only the usage
windows, sorted by length. The logs contain your conversations; the plugin
never outputs or stores them, and never reads `auth.json`. This is a local
client format, not a public API: if it changes, the values show `–` rather
than a wrong number.

## Pace

A percentage alone doesn't say whether you are fine: 60% used is a lot one
hour into a week and very little six days in. So for each window the popup
compares **how much of your limit you used** with **how much of the window has
passed**, and shows the result next to a bar:

```text
Weekly         87%   resets Sun 15/11 12:00
                     (in 3d)
              █████████│████░░
              above pace · runs out ~tomorrow 02:20

Weekly         20%   resets Sat 14/11 12:00
                     (in 2d)
              ███░░░░░░░░│░░░░
              below pace · 71% of the week passed
```

The filled part of the bar is your usage; the `│` marks how far into the
window you are. Fill past the marker means you're using the limit faster than
time is passing; fill behind it means you have room to spare.

| Popup shows | Meaning |
| --- | --- |
| `below pace` | You used less than 85% of the share that has passed (for example 30% used halfway through). At this rate you won't reach the limit, and part of it may go unused. |
| `on pace` | Your usage is within 15% of the time passed (85% to 115% of it). Near the end of a window this can still mean reaching the limit shortly before the reset: 86% used with 84% of the week passed is on pace, yet runs out a few hours early. Beyond 115%, it's also on pace if you'd still reach the reset first. |
| `above pace · runs out ~Thu 14:20` | At your average rate so far, you'll reach the limit at that time, before the window resets. |
| `pace in 41m` | Less than 15% of the window has passed, too little to judge; this says when the judgment starts. |

The rate is your average since the window started: used percentage divided by
time passed. It's a projection, not a prediction of how you'll actually work.
The same rate decides whether your saved resets would be used before they
expire (see below).

## Saved resets

Both providers sometimes grant free limit resets that expire. The popup lists
them and plays your current pace forward to say whether you would use them in
time. They are queried only when the popup opens, never by the status line,
and a successful answer is cached for five minutes in
`$XDG_CACHE_HOME/tmux-agent-usage`. A failed query never turns into zero, and
your percentages stay available.

| Popup shows | Meaning |
| --- | --- |
| `checking…` | The query is running |
| `none` | Checked; you have none |
| `N · expiry unknown` | The count is known, the expiry dates aren't |
| `unavailable` | The client answered without reset information |
| `couldn't check` | The query failed, timed out (10 seconds) or got an unexpected answer |

### Codex

The popup runs the official `codex app-server --listen stdio://`, sends
`initialize`, `initialized` and `account/rateLimits/read`, reads
`rateLimitResetCredits` and stops the process. Codex handles sign-in and token
refresh itself. No listener, login, turn or reset redemption is ever requested.

| Codex version | Saved resets in the popup |
| --- | --- |
| 0.143.0 or later | Count and expiry dates |
| 0.141.0 to 0.142.x | Count only (`N · expiry unknown`) |
| 0.100.0 to 0.140.x | `unavailable` |
| Before 0.100.0 | `couldn't check` (no `--listen`) |

### Claude Code (opt-in)

Claude's saved resets are **off by default**. With
`@agent-usage-claude-resets on` (or `--claude-resets`), the popup sends the
same read-only request Claude Code makes for them:

```text
GET https://api.anthropic.com/api/oauth/usage?cedar_ember=1&skip_spend=1
anthropic-beta: oauth-2025-04-20
User-Agent: claude-cli/<installed version> (external, cli)
Authorization: Bearer <Claude Code's OAuth access token>
```

**This endpoint is undocumented and may change or stop working without
notice.** If its answer changes shape, the popup shows `couldn't check`, never
a wrong number. Use it knowing it isn't an official API.

- The token is Claude Code's own: the `Claude Code-credentials` Keychain item
  on macOS (falling back to `~/.claude/.credentials.json`), and
  `.credentials.json` in `CLAUDE_CONFIG_DIR` or `~/.claude` on Linux.
- It goes to curl through stdin, never in arguments, logs or caches, and is
  never refreshed. An expired token shows `couldn't check` until Claude Code
  refreshes it.
- On macOS, a custom `CLAUDE_CONFIG_DIR` is not supported for this query: its
  Keychain entry name isn't documented, so the popup reports that instead of
  guessing an account.
- A `429` answer is respected: the popup waits for `Retry-After`, or backs off
  from one minute up to an hour, without starting curl again in between.
- The installed version comes from `~/.local/share/claude/versions`, falling
  back to `claude --version`.

Without the option, none of this runs: no credentials, Keychain, curl or cache
for Claude resets.

## Files and permissions

| Path | Contents |
| --- | --- |
| `~/.cache/tmux-agent-usage/claude/` | Claude usage snapshots |
| `~/.cache/tmux-agent-usage/*-resets.json` | Saved-reset answers and Claude backoff state |
| `<plugin>/bin/` | The managed binary, its stable link, `install.log` and the last status errors |

Cache directories are created with mode `0700` and files with `0600`; a cache
directory that is a symlink or owned by someone else is refused. Saved-reset
answers are kept per client configuration directory. Switching accounts in the
same directory can show the previous answer for up to five minutes.

## Managed binary

With TPM and no `@agent-usage-bin`, the plugin reads its version from
`Cargo.toml` and downloads exactly that release's archive for your platform
(macOS arm64 or x86_64, Linux arm64 or x86_64; Linux builds are static musl).
It checks the SHA-256 checksum, the archive contents and the executable's
platform before installing it atomically in `bin/vVERSION/TARGET/`. Nothing
from the download runs before those checks. There is no "latest" download and
no build on your machine.

`bin/tmux-agent-usage` is a link to the version in use. It moves to the new
version whenever the plugin updates, which keeps Claude Code's `statusLine`
path working. Earlier versions stay in `bin/` until you delete them.
