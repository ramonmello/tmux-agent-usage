pub const GRUVBOX_FLAGS: &[&str] = &[
    "--popup-text",
    "#fbf1c7",
    "--popup-muted",
    "#a89984",
    "--popup-dim",
    "#504945",
    "--popup-ok",
    "#b8bb26",
    "--popup-high",
    "#fabd2f",
    "--popup-critical",
    "#fb4934",
    "--popup-accent",
    "#83a598",
    "--popup-border",
    "fg=#665c54",
];

pub fn gruvbox_theme() -> crate::popup_theme::PopupTheme {
    let mut args = vec!["popup".to_owned(), "fixture".to_owned()];
    args.extend(GRUVBOX_FLAGS.iter().map(|flag| flag.to_string()));
    match crate::cli::parse(&args).unwrap() {
        crate::cli::Request::Popup { options, .. } => options.theme,
        _ => unreachable!(),
    }
}

use crate::contract_tests::{isolated, root, run, seed};
use crate::popup_theme::PopupTheme;
use crate::{access, cli};
use std::fs;

fn sample(theme: &PopupTheme) -> (String, String, String) {
    let fixture: serde_json::Value =
        serde_json::from_str(include_str!("../fixtures/baseline/popup.json")).unwrap();
    let now = fixture["now"].as_i64().unwrap();
    let limits = (
        serde_json::from_value(fixture["claude"].clone()).unwrap(),
        now - 3600,
    );
    let codex = crate::codex::Status {
        windows: fixture["codex_windows"]
            .as_array()
            .unwrap()
            .iter()
            .map(|window| crate::codex::Window {
                used_percent: window["used_percent"].as_f64().unwrap(),
                resets_at: (now + window["remaining_secs"].as_i64().unwrap()) as f64,
                window_secs: window["window_secs"].as_i64().unwrap(),
            })
            .collect(),
        plan_type: Some("pro".to_owned()),
        credits: Some(serde_json::from_value(fixture["credits"].clone()).unwrap()),
        observed_at: Some(now - 90),
    };
    let claude = theme.claude(
        Some(&limits),
        Some(&crate::claude_resets::ClaudeResets::Unavailable),
        now,
    );
    let codex = theme.codex(
        Some(&codex),
        Some(&crate::resets::SavedResets::Unavailable),
        now,
    );
    (
        claude,
        codex,
        theme.selected_footer(
            &[("Claude Code", Some(now - 3600)), ("Codex", Some(now - 90))],
            now,
        ),
    )
}

fn terminal(text: &str, rows: u16, cols: u16) -> vt100::Parser {
    let mut terminal = vt100::Parser::new(rows, cols, 0);
    terminal.process(text.replace('\n', "\r\n").as_bytes());
    terminal
}

#[test]
fn defaults_are_ansi_and_overrides_preserve_text_with_real_sgr_colors() {
    if !isolated("popup_tests::defaults_are_ansi_and_overrides_preserve_text_with_real_sgr_colors")
    {
        return;
    }
    let default = PopupTheme::default();
    let (claude, codex, footer) = sample(&default);
    let ansi = default.viewport(&[&claude, &codex], &footer, 113, 60);
    assert!(!ansi.contains("38;2") && !ansi.contains("38;5") && !ansi.contains('#'));
    assert!(
        !ansi
            .chars()
            .any(|ch| ('\u{e000}'..='\u{f8ff}').contains(&ch)
                || ('\u{f0000}'..='\u{10ffff}').contains(&ch))
    );
    let rendered = terminal(&ansi, 60, 113);
    let mut colors = std::collections::BTreeSet::new();
    for row in 0..60 {
        for col in 0..113 {
            let cell = rendered.screen().cell(row, col).unwrap();
            match cell.fgcolor() {
                vt100::Color::Default => {}
                vt100::Color::Idx(index) => {
                    assert!(index < 16);
                    colors.insert(index);
                }
                vt100::Color::Rgb(..) => panic!("default emitted RGB"),
            }
            assert_eq!(cell.bgcolor(), vt100::Color::Default);
        }
    }
    // Fixture exercises low, high, critical, below pace, accent and muted.
    assert!(colors.is_superset(&[1, 2, 3, 4, 8].into()));
    let themed = gruvbox_theme();
    let (claude, codex, footer) = sample(&themed);
    let rgb = themed.viewport(&[&claude, &codex], &footer, 113, 60);
    assert_eq!(
        rendered.screen().contents(),
        terminal(&rgb, 60, 113).screen().contents()
    );
    for (color, expected) in [
        ("default", vt100::Color::Default),
        ("terminal", vt100::Color::Default),
        ("red", vt100::Color::Idx(1)),
        ("brightblue", vt100::Color::Idx(12)),
        ("colour0", vt100::Color::Idx(0)),
        ("colour255", vt100::Color::Idx(255)),
        ("#Ab01fF", vt100::Color::Rgb(171, 1, 255)),
    ] {
        let mut theme = PopupTheme::default();
        theme.set("--popup-text", color).unwrap();
        let parser = terminal(&theme.claude_local(None, 0), 5, 80);
        assert_eq!(
            parser.screen().cell(0, 1).unwrap().fgcolor(),
            expected,
            "{color}"
        );
    }
}

#[test]
fn invalid_popup_values_and_layout_flags_fail_before_any_access() {
    if !isolated("popup_tests::invalid_popup_values_and_layout_flags_fail_before_any_access") {
        return;
    }
    for command in ["popup", "details"] {
        for flag in [
            "--popup-text",
            "--popup-muted",
            "--popup-dim",
            "--popup-ok",
            "--popup-high",
            "--popup-critical",
            "--popup-accent",
        ] {
            for value in [
                "",
                "fg=red",
                "red,bold",
                "colour256",
                "colour-1",
                "#12345",
                "#00000g",
                "rainbow",
                " red",
                "red\n",
                "red\x1b[0m",
                "#{pane_id}",
            ] {
                let mut args = vec![command];
                if command == "popup" {
                    args.push("fixture");
                }
                args.extend([flag, value]);
                access::start();
                let error = cli::parse(&args.into_iter().map(str::to_owned).collect::<Vec<_>>())
                    .err()
                    .unwrap();
                assert!(
                    error.contains("invalid color") && error.contains(flag),
                    "{error}"
                );
                assert!(access::take().is_empty());
            }
        }
    }
    for args in [
        vec!["popup", "fixture", "--popup-border", "#[fg=red]"],
        vec!["popup", "fixture", "--popup-text"],
        vec!["popup", "fixture", "--popup-layout", "stacked"],
        vec!["details", "--popup-border", "fg=red"],
        vec!["inspect", "--popup-text", "red"],
    ] {
        access::start();
        assert!(cli::parse(&args.into_iter().map(str::to_owned).collect::<Vec<_>>()).is_err());
        assert!(access::take().is_empty());
    }
}

#[test]
fn fake_tmux_preserves_baseline_border_title_and_passes_literal_flags() {
    if !isolated("popup_tests::fake_tmux_preserves_baseline_border_title_and_passes_literal_flags")
    {
        return;
    }
    seed();
    let mut args = vec!["popup", "fixture", "160", "60", "--claude-resets"];
    args.extend(GRUVBOX_FLAGS);
    run(&args);
    let argv = fs::read_to_string(root().join("tmux-args")).unwrap();
    let argv: Vec<_> = argv.lines().collect();
    assert_eq!(
        &argv[..14],
        [
            "display-popup",
            "-c",
            "fixture",
            "-E",
            "-b",
            "rounded",
            "-S",
            "fg=#665c54",
            "-T",
            "#[fg=#fbf1c7] Agent usage ",
            "-w",
            "115",
            "-h",
            "18"
        ]
    );
    let (pairs, _) = GRUVBOX_FLAGS.as_chunks::<2>();
    for pair in pairs.iter().take(7) {
        assert!(argv[14].contains(&format!("'{}' '{}'", pair[0], pair[1])));
    }
    assert!(!argv[14].contains("--popup-border"));
    assert!(argv[14].contains("'--claude-resets'"));
    for (flags, title, border) in [
        (vec![], " Agent usage ", false),
        (vec!["--popup-high", "red"], " Agent usage ", false),
        (
            vec!["--popup-border", "fg=blue,bg=default"],
            " Agent usage ",
            true,
        ),
        (
            vec!["--popup-text", "default"],
            "#[fg=default] Agent usage ",
            false,
        ),
    ] {
        let mut args = vec!["popup", "fixture", "160", "60"];
        args.extend(flags);
        run(&args);
        let argv = fs::read_to_string(root().join("tmux-args")).unwrap();
        let argv: Vec<_> = argv.lines().collect();
        assert_eq!(argv.contains(&"-S"), border);
        assert_eq!(
            argv[argv.iter().position(|arg| *arg == "-T").unwrap() + 1],
            title
        );
        assert!(!argv.contains(&"-s"));
        assert_eq!(
            argv[argv.iter().position(|arg| *arg == "-b").unwrap() + 1],
            "rounded"
        );
    }
}

#[test]
fn narrow_short_and_growing_screens_keep_headers_values_and_footer_without_scrolling() {
    if !isolated(
        "popup_tests::narrow_short_and_growing_screens_keep_headers_values_and_footer_without_scrolling",
    ) {
        return;
    }
    let theme = PopupTheme::default();
    let (claude, codex, footer) = sample(&theme);
    for selected in [
        vec![claude.as_str()],
        vec![codex.as_str()],
        vec![claude.as_str(), codex.as_str()],
        vec![codex.as_str(), claude.as_str()],
    ] {
        for (cols, rows) in [
            (113, 60),
            (112, 60),
            (64, 46),
            (54, 46),
            (40, 60),
            (24, 80),
            (113, 10),
            (80, 14),
            (40, 22),
            (24, 8),
        ] {
            let rendered = theme.viewport(&selected, &footer, cols, rows);
            assert!(rendered.split('\n').count() <= rows);
            assert!(
                rendered
                    .lines()
                    .all(|line| crate::details::visible_width(line) <= cols)
            );
            let terminal = terminal(&rendered, rows as u16, cols as u16);
            assert_eq!(terminal.screen().errors(), 0);
            let contents = terminal.screen().contents();
            for section in &selected {
                let title = if section.contains("Claude Code") {
                    "Claude Code"
                } else {
                    "Codex"
                };
                assert!(contents.contains(title), "{cols}x{rows}: {contents}");
            }
            let footer_row = terminal
                .screen()
                .rows(0, cols as u16)
                .nth(rendered.split('\n').count() - 1)
                .unwrap();
            assert!(
                footer_row.contains("close"),
                "footer missing at {cols}x{rows}: {contents}"
            );
            if rows >= 10 && cols >= 40 {
                for section in &selected {
                    for value in if section.contains("Claude Code") {
                        ["7%", "87%"]
                    } else {
                        ["95%", "20%"]
                    } {
                        assert!(
                            contents.contains(value),
                            "essential value missing at {cols}x{rows}: {contents}"
                        );
                    }
                }
                assert!(contents.contains("resets") && contents.contains("Saved resets"));
                if selected.contains(&codex.as_str()) {
                    assert!(contents.contains("12.50"));
                }
            }
            if selected.len() == 2 {
                let left = if selected[0].contains("Claude Code") {
                    "Claude Code"
                } else {
                    "Codex"
                };
                let right = if left == "Codex" {
                    "Claude Code"
                } else {
                    "Codex"
                };
                assert!(contents.find(left).unwrap() < contents.find(right).unwrap());
            }
        }
    }
    // Geometry includes wrapped timestamps and borders at narrow sizes.
    for (cols, rows) in [(160, 60), (80, 60), (40, 60), (20, 10)] {
        let (width, height) = theme.geometry(&[&claude, &codex], &footer, cols, rows);
        assert!(width <= cols && height <= rows);
        let inner = theme.viewport(&[&claude, &codex], &footer, width - 2, height - 2);
        assert!(inner.split('\n').count() <= height - 2);
    }
    // Wide and combining external labels must fit terminal cells, not bytes
    // or Unicode scalar counts, even when there is only one cell available.
    let status = crate::codex::Status {
        windows: vec![],
        plan_type: Some("長い名前 e\u{301}".to_owned()),
        credits: None,
        observed_at: None,
    };
    let column = theme.codex(
        Some(&status),
        Some(&crate::resets::SavedResets::Unavailable),
        0,
    );
    for cols in [1, 2, 9, 16, 24] {
        let rendered = theme.viewport(&[&column], "q/esc", cols, 20);
        assert!(
            rendered
                .lines()
                .all(|line| crate::details::visible_width(line) <= cols)
        );
        assert!(rendered.split('\n').count() <= 20);
    }
}
