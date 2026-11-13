# Troubleshooting

Start with these. They read only local files and never query anything:

<!-- test:inspect -->
```sh
tmux-agent-usage inspect --agents claude
tmux-agent-usage inspect --agents codex
```

```sh
tmux show -gv @agent-usage-diagnostic
```

The diagnostic holds the last problem the plugin found while loading. It is
empty when everything went fine.

## The status line shows nothing

The plugin is installing its binary, or the install failed. Status bodies stay
empty during a download and never retry it on their own.

- Check `@agent-usage-diagnostic` and `bin/install.log` in the plugin
  directory (for TPM, `~/.tmux/plugins/tmux-agent-usage/bin/install.log`). The
  log names the URL and whether the download, checksum or archive failed.
- Reload your config to retry after fixing the cause.
- No release for your platform? Install with Cargo and set `@agent-usage-bin`.
- "installation already locked": an installer was killed mid-way. If none is
  running, delete the `bin/vVERSION/TARGET.lock` directory it names and reload.
- "invalid managed installation": delete that `bin/vVERSION/TARGET` directory
  and reload.
- With `@agent-usage-bin`, the path must be executable, and a bare name must be
  on the **tmux server's** `PATH`, which can differ from your shell's.

The last error from each status call is in `bin/status-claude.log` and
`bin/status-codex.log`.

## The status line shows `–`

The binary works, but found no data for that agent.

**Claude:** look at `claude_snapshot_status` in `inspect --agents claude`:

| Value | Meaning |
| --- | --- |
| `no_snapshots` | The tap hasn't recorded anything. Check the [`statusLine` setup](../README.md#claude-code) and send a message in Claude Code. tmux and Claude Code must use the same `HOME` and `XDG_CACHE_HOME`. |
| `no_usage` | Snapshots exist, but Claude Code sent no supported window. Your account or client version may not provide them (2.1.80+ is required). |
| `available` | Data was found. If the status line still shows `–`, the window may be missing for your account. |
| `invalid_snapshots` | Snapshot files exist but can't be read. Check permissions, or delete `~/.cache/tmux-agent-usage/claude/` and let the tap rewrite it. |
| `unreadable`, `path_unavailable` | The snapshot directory can't be read, or `HOME` isn't set. |

A Claude Code update without usage writes no snapshot, so `no_snapshots`
doesn't prove the tap never ran.

**Codex:** `inspect --agents codex` shows the sessions directory it read. Use
Codex once in the same environment; its next token-count event carries the
usage. Set `CODEX_HOME` the same way for tmux and Codex if you moved it.

## Claude stopped updating after an upgrade

The `statusLine` command must point at a path that survives upgrades. For TPM
installs that is `bin/tmux-agent-usage` in the plugin directory, a link the
plugin moves to each new version. Don't point it at a versioned
`bin/v0.1.0/...` path, or at a Homebrew `Cellar` path.

If the diagnostic says `bin/tmux-agent-usage is not a symlink`, something else
is at that path. The plugin leaves it alone; remove it and reload to get the
link back.

## The popup says `couldn't check`

Saved resets couldn't be read; percentages are unaffected.

- **Codex:** `codex` must be on the tmux server's `PATH`, signed in, and
  version 0.100.0 or later (0.143.0+ for expiry dates). See the
  [version table](how-it-works.md#codex-1).
- **Claude** (only with `@agent-usage-claude-resets on`): Claude Code must be
  signed in with a current token; send a message in Claude Code to refresh it.
  On macOS a custom `CLAUDE_CONFIG_DIR` isn't supported for this query. The
  endpoint is undocumented, so a change on Anthropic's side also ends here.

Don't sign in through this plugin; sign in with the clients themselves.

## The popup key does nothing

If `@agent-usage-diagnostic` says `prefix A is already bound`, the key was
taken and the plugin kept your binding. This happens when upgrading from a
config that bound the popup by hand:

```sh
tmux unbind -T prefix A
tmux source-file ~/.tmux.conf
```

Remove the old `bind` line from your config too, or pick another key with
`@agent-usage-popup-key`. A reload never removes bindings, so restarting the
tmux server also clears an old one.

## Display problems

- **Cut-off status line:** raise `status-right-length` (or
  `status-left-length`).
- **"Enlarge terminal for details" in the popup:** the terminal is too short
  for every detail. Enlarge it; headers, percentages and the footer always stay.
- **Missing icons or caps:** your theme's font is missing on the machine
  running the terminal. Try the plain [example](../examples/README.md) to
  confirm the data is fine.
- **Wrong colors:** exact `#rrggbb` colors need a truecolor terminal and a
  matching tmux `terminal-features` setting.

## Reporting a problem

Open an issue with the plugin version, your OS, tmux and client versions, the
symptom and the relevant `inspect` fields or diagnostic. **Never post tokens,
credential files, session logs, conversations or full Claude Code payloads.**
`inspect` has no tokens, but it shows local paths; edit them if you prefer.
