# Configuration

The plugin reads its `@agent-usage-*` options once, when it loads. Set them
before the plugin line (or before TPM's initialization), then reload your
config or the plugin to apply a change. The binary itself never reads tmux
options or a configuration file: the plugin turns your options into
command-line flags.

## Placeholders

| Placeholder | Becomes |
| --- | --- |
| `#{agent_usage_claude}` | The Claude body, such as `7% · 2%` (5-hour · 7-day) |
| `#{agent_usage_codex}` | The Codex body, such as `28%` (windows from shortest to longest) |

Put them in the global `status-left` or `status-right`, anywhere in your own
format. Everything around them stays as you wrote it, and reloading replaces
the generated commands in place. Each placeholder selects its agent on its
own; `@agent-usage-agents` only affects the popup.

## Options

| Option | Default | Effect |
| --- | --- | --- |
| `@agent-usage-agents` | `claude codex` | Agents in the popup, in order. One agent gets one column. |
| `@agent-usage-popup-key` | `A` | Prefix-table key that opens the popup; `none` or `off` disables it. |
| `@agent-usage-claude-resets` | `off` | `on` lets the popup ask Claude for saved resets (see [how it works](how-it-works.md#saved-resets)). |
| `@agent-usage-bin` | unset | Unset: the plugin manages its own binary. A path, or a name found on the tmux server's `PATH`, selects an executable you installed. |
| `@agent-usage-style-normal` | `fg=default` | Status text below 70% |
| `@agent-usage-style-high` | `fg=yellow` | Status values from 70% to 89% |
| `@agent-usage-style-critical` | `fg=red` | Status values from 90% |
| `@agent-usage-style-muted` | `fg=brightblack` | Separators, unknown windows and the reset time at high usage |
| `@agent-usage-style-reset-soon` | `fg=green` | Reset time when usage is low but the reset is near: unused quota about to expire |
| `@agent-usage-popup-text` | terminal default | Main text; when set, also the title |
| `@agent-usage-popup-muted` | `brightblack` | Labels, dates and secondary text |
| `@agent-usage-popup-dim` | `brightblack` | Empty part of the bars, rules, the footer and `couldn't check` |
| `@agent-usage-popup-ok` | `green` | Good news, such as `below pace` |
| `@agent-usage-popup-high` | `yellow` | High usage, `above pace` and warnings |
| `@agent-usage-popup-critical` | `red` | Critical usage, including `above pace` at that level |
| `@agent-usage-popup-accent` | `blue` | The bar marker showing how much of the window has passed |
| `@agent-usage-popup-border` | tmux's popup border style | Border style, such as `fg=#665c54` |

Status styles are complete tmux styles (`fg=#fbf1c7,bg=#3c3836,bold`), without
the surrounding `#[...]`. Popup colors take a single color: a name such as
`yellow`, `colour0` to `colour255`, or `#rrggbb`. Exact hex colors need a
truecolor terminal. Invalid values are reported instead of silently replaced.
An empty option uses its default.

The popup's layout is fixed: it adapts to the terminal size and the number of
agents, but has no layout settings. Its border is always rounded.

The plugin also writes a few options of its own: `@agent-usage-diagnostic`
(the last problem it found), `@agent-usage-popup-command` (the command behind
the popup key) and `@agent-usage-internal-*`. Read them, but don't set them.

## Key binding

If the popup key is already bound, the plugin keeps your binding and explains
why in `@agent-usage-diagnostic`. Choose another key or `none`. The plugin only
replaces bindings it created itself, so a key you edited becomes yours.

## Clickable segments

The plugin doesn't touch your mouse bindings. To open the popup by clicking a
segment, mark each one with a mouse range and restore `range=right` after it.
Themes often draw these segments as "pills": rounded, colored blocks, as in
the screenshot. The ranges work with any style:

```tmux
set -g status-right '#[range=user|claude]Claude #{agent_usage_claude}#[range=right] #[range=user|codex]Codex #{agent_usage_codex}#[range=right]'
```

Then merge this into your own `MouseDown1Status` handler:

```tmux
bind -n MouseDown1Status {
  if -F '#{||:#{==:#{mouse_status_range},claude},#{==:#{mouse_status_range},codex}}' {
    run-shell -b '#{E:@agent-usage-popup-command}'
  } {
    switch-client -t =
  }
}
```

Clicks anywhere else keep tmux's default of switching to the clicked window.

## Themed examples

[`examples/`](../examples/README.md) has a plain format and a Gruvbox format
with rounded, colored blocks ("pills"). Sourcing them only defines an
option; you place it in your status line yourself.

Fonts are your theme's business. The plugin needs no icon font, only a
monospace font with `·`, `–` and `↻`. Install fonts on the machine that runs
the terminal: your local computer for SSH, the Windows host for WSL.

## Without TPM

With the binary on your `PATH`, call it from your status line and bind the
popup yourself:

<!-- test:direct-tmux -->
```tmux
set -ag status-right ' Claude #(tmux-agent-usage status claude) | Codex #(tmux-agent-usage status codex)'
bind A run-shell -b "tmux-agent-usage popup '#{client_name}' #{client_width} #{client_height} --agents 'claude codex'"
```

## Command line

```text
tmux-agent-usage status <claude|codex> [--style-ROLE STYLE ...]
tmux-agent-usage popup CLIENT [COLS ROWS] [--agents LIST] [--popup-COLOR COLOR ...] [--popup-border STYLE] [--claude-resets]
tmux-agent-usage details [--no-wait] [--agents LIST] [--popup-COLOR COLOR ...] [--claude-resets]
tmux-agent-usage claude-tap [--no-record] [-- COMMAND [ARGS...]]
tmux-agent-usage inspect [--agents LIST]
tmux-agent-usage help
```

- **`status`** prints one agent's body with no trailing newline. Without style
  flags it is plain text; any `--style-*` flag (`normal`, `high`, `critical`,
  `muted`, `reset-soon`) switches to tmux styles, with the defaults above for
  the roles you omit. It reads only local files and always exits 0 when the
  arguments are valid: missing data is `–`, never an invented `0%`.
- **`popup`** opens the details in a centered tmux popup on `CLIENT`, sized to
  fit `COLS`×`ROWS` (default 80×24).
- **`details`** draws the popup body in the current terminal and waits for a
  key or click; `--no-wait` exits right after drawing.
- **`claude-tap`** records Claude Code's usage, then runs the wrapped command
  with the same input (see [Claude Code setup](../README.md#claude-code)).
- **`inspect`** prints the sources and values found, as diagnostic JSON with no
  stable schema.

`LIST` is one argument with agent names separated by spaces, such as
`--agents 'codex claude'`. Order is kept; an empty list, an unknown name or a
repeated name is an error. Usage errors exit with 2 and runtime failures with 1.

<!-- test:status -->
```sh
tmux-agent-usage status claude
tmux-agent-usage status codex
tmux-agent-usage status codex --style-normal 'fg=default,bg=black,bold'
```

### Status format

Windows are separated by ` · `. Each one is a rounded percentage, optionally
followed by `↻` and the time to reset (`45m`, `1h05m`, `1d4h`):

- From 70% the time to reset is always shown, muted.
- Below 70% it is shown in the reset-soon style when the reset is near: within
  2 hours for the 5-hour window, 2 days for the 7-day window, or the last 2/5
  of any other window.
- A window whose reset time has passed shows `0%` until new data arrives.
- An unknown window, or an agent with no data, shows `–`.
