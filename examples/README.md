# Status line examples

Two ready-made formats that call the binary directly, so they need
`tmux-agent-usage` on the tmux server's `PATH`. Sourcing a file only defines a
user option; nothing changes in your status line until you place that option
yourself:

```tmux
source-file /path/to/tmux-agent-usage/examples/simple.conf
set -ag status-right ' #{E:@example-agent-usage-simple}'
```

- **`simple.conf`** (`@example-agent-usage-simple`): text labels and plain
  bodies, no colors. Any monospace font works.
- **`gruvbox.conf`** (`@example-agent-usage-gruvbox`): Gruvbox "pills" (rounded, colored blocks)
  labeled `CC` and `CX`. It needs a font with the rounded Powerline caps
  U+E0B6 and U+E0B4, and a truecolor terminal for the exact colors.

For one agent, use `#(tmux-agent-usage status claude)` or
`#(tmux-agent-usage status codex)` in your own format instead. If your status
line gets cut off, raise `status-right-length`.

Fonts are installed on the machine that runs the terminal: your local
computer for SSH, the Windows host for WSL. With TPM, prefer the placeholders
and options in the [configuration reference](../docs/configuration.md); these
examples are for setups without the plugin.
