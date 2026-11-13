mod support;

use std::fs;
use std::os::unix::fs::PermissionsExt;
use support::{Sandbox, success};

const BINARY: &str = env!("CARGO_BIN_EXE_tmux-agent-usage");

fn fake_tmux(sandbox: &Sandbox) {
    let path = sandbox.root.join("tmux");
    fs::write(&path, "#!/bin/sh\nprintf '%s\\0' \"$@\" > \"$HOME/argv\"\nfor arg do child=$arg; done\n/bin/sh -c \"$child --no-wait\" > \"$HOME/details-output\"\n").unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o700)).unwrap();
}

#[test]
fn popup_executes_quoted_child_with_the_same_colors_and_only_selected_agent() {
    let sandbox = Sandbox::new();
    fake_tmux(&sandbox);
    let flags = [
        "--popup-text",
        "#abcdef",
        "--popup-muted",
        "colour123",
        "--popup-dim",
        "brightblue",
        "--popup-ok",
        "green",
        "--popup-high",
        "yellow",
        "--popup-critical",
        "red",
        "--popup-accent",
        "blue",
        "--popup-border",
        "fg=#123456,bg=default,bold",
    ];
    for theme in [&flags[..], &[][..]] {
        success(
            sandbox
                .command(BINARY)
                .args(["popup", "fixture client", "160", "60", "--agents", "claude"])
                .args(theme)
                .env("PATH", &sandbox.root)
                .output()
                .unwrap(),
        );
        let argv = fs::read(sandbox.root.join("argv")).unwrap();
        let argv: Vec<_> = argv
            .split(|byte| *byte == 0)
            .filter(|arg| !arg.is_empty())
            .map(|arg| std::str::from_utf8(arg).unwrap())
            .collect();
        assert_eq!(
            &argv[..6],
            [
                "display-popup",
                "-c",
                "fixture client",
                "-E",
                "-b",
                "rounded"
            ]
        );
        assert_eq!(argv.contains(&"-S"), !theme.is_empty());
        let child = argv.last().unwrap();
        assert!(child.contains("'--agents' 'claude'"));
        assert!(!child.contains("--popup-border") && !child.contains("--claude-resets"));
        let body = fs::read_to_string(sandbox.root.join("details-output")).unwrap();
        assert!(body.contains("Claude Code") && body.contains("No data yet"));
        assert!(!body.contains("Codex") && !body.contains("Saved resets"));
        if theme.is_empty() {
            assert!(!body.contains("38;2") && !body.contains("38;5"));
            assert_eq!(
                argv[argv.iter().position(|arg| *arg == "-T").unwrap() + 1],
                " Agent usage "
            );
        } else {
            assert!(body.contains("\x1b[38;2;171;205;239mClaude Code"));
            assert!(body.contains("\x1b[38;5;123mNo data yet"));
            assert!(body.contains("\x1b[94m"));
            assert_eq!(
                argv[argv.iter().position(|arg| *arg == "-T").unwrap() + 1],
                "#[fg=#abcdef] Agent usage "
            );
            assert_eq!(
                argv[argv.iter().position(|arg| *arg == "-S").unwrap() + 1],
                "fg=#123456,bg=default,bold"
            );
        }
        assert!(body.ends_with("\x1b[?1006l\x1b[?1000l\x1b[?25h"));
        assert!(!sandbox.root.join("cache").exists());
    }
}

#[test]
fn invalid_color_cli_reports_usage_without_opening_tmux_or_creating_cache() {
    let sandbox = Sandbox::new();
    fake_tmux(&sandbox);
    for args in [
        vec!["popup", "fixture", "--popup-text", "red,bold"],
        vec!["popup", "fixture", "--popup-high", "colour256"],
        vec!["details", "--popup-ok", "#12345"],
    ] {
        let output = sandbox
            .command(BINARY)
            .args(&args)
            .env("PATH", &sandbox.root)
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(2));
        assert!(output.stdout.is_empty());
        let diagnostic = String::from_utf8(output.stderr).unwrap();
        assert!(
            diagnostic.contains("invalid color for --popup-")
                && diagnostic.contains("expected one tmux color")
                && diagnostic.contains("usage:")
        );
        assert!(!sandbox.root.join("argv").exists());
        assert!(!sandbox.root.join("cache").exists());
    }
}
