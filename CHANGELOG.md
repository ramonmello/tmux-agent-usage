# Changelog

All notable changes to this project are documented here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and versions follow
[Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.1.0] - 2026-11-13

First public release.

### Added

- `#{agent_usage_claude}` and `#{agent_usage_codex}` placeholders for the tmux
  status line, styled through `@agent-usage-style-*` options.
- A details popup (`prefix A` by default) with reset times, usage bars, pace
  and saved resets, adapting to the terminal size and the selected agents.
- `claude-tap`, a Claude Code `statusLine` wrapper that records usage and
  passes the input through to your existing command.
- Saved resets for Codex through `codex app-server`, and opt-in saved resets
  for Claude Code through an undocumented endpoint.
- TPM installation that downloads and verifies the release binary for the
  plugin's version, with a stable `bin/tmux-agent-usage` link for Claude Code.
- Installation with Cargo and Homebrew (`ramonmello/tap`).
- Release binaries for macOS (arm64, x86_64) and Linux (arm64, x86_64, static
  musl), with checksums and build provenance.

### Known limitations

- Only macOS arm64 has been used with real accounts. Linux and macOS x86_64
  pass the full test suite in CI but haven't been used day to day; WSL is
  untested.
- Native Windows is not supported.
- On macOS, Claude saved resets don't work with a custom `CLAUDE_CONFIG_DIR`.

[Unreleased]: https://github.com/ramonmello/tmux-agent-usage/compare/v0.1.0...next
[0.1.0]: https://github.com/ramonmello/tmux-agent-usage/releases/tag/v0.1.0
