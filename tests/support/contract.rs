use crate::clock::FixedClock;
use crate::test_support::{Sandbox, success};
use crate::{access, cli, execute};
use std::fs::{self, File, FileTimes};
use std::io;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::time::{Duration, UNIX_EPOCH};

const NOW: i64 = 1_794_484_800;

pub(super) fn root() -> PathBuf {
    std::env::var_os("HOME").unwrap().into()
}

pub(super) fn executable(name: &str, script: &str) {
    let path = root().join("fakes").join(name);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(&path, script).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o700)).unwrap();
}

// Each observation runs in a fresh UTC process. No global environment changes
// or simultaneous test cases can hide an access made by a lookup thread.
pub(super) fn isolated(test: &str) -> bool {
    if std::env::var("AGENT_USAGE_CONTRACT_CHILD").as_deref() == Ok(test) {
        executable(
            "tmux",
            "#!/bin/sh\nprintf '%s\\n' \"$@\" > \"$HOME/tmux-args\"\n",
        );
        crate::terminal::test_size((60, 160));
        executable(
            "security",
            "#!/bin/sh\nprintf 'called' >> \"$HOME/security-called\"\nexit 1\n",
        );
        executable("claude", "#!/bin/sh\nprintf '2.1.0 (Claude Code)\\n'\n");
        executable(
            "curl",
            "#!/bin/sh\n/bin/cat > /dev/null\n: > \"$HOME/curl-input\"\nprintf 'HTTP/1.1 200 OK\\r\\n\\r\\n{\"cedar_ember\":{\"eligible\":true,\"grants\":[]}}\\n200'\n",
        );
        executable(
            "codex",
            "#!/bin/sh\nprintf '%s\\n' \"$@\" >> \"$HOME/codex-args\"\nIFS= read -r initialize\nprintf '{\"id\":1,\"result\":{}}\\n'\nIFS= read -r initialized\nIFS= read -r request\n/bin/cat \"$HOME/codex-response\"\n",
        );
        return true;
    }
    let sandbox = Sandbox::new();
    success(
        sandbox
            .command(std::env::current_exe().unwrap())
            .args(["--exact", test, "--nocapture"])
            .env("AGENT_USAGE_CONTRACT_CHILD", test)
            .env_remove("CLAUDE_CONFIG_DIR")
            .env_remove("CLAUDE_SECURESTORAGE_CONFIG_DIR")
            .env("PATH", sandbox.root.join("fakes"))
            .output()
            .expect("run isolated contract test"),
    );
    false
}

pub(super) fn seed() {
    let claude = root().join("cache/tmux-agent-usage/claude");
    fs::create_dir_all(&claude).unwrap();
    fs::write(
        claude.join("fixture.json"),
        serde_json::to_vec(&serde_json::json!({
            "observed_at": NOW - 60,
            "five_hour": {"used_percentage": 7, "resets_at": NOW + 10800},
            "seven_day": {"used_percentage": 2, "resets_at": NOW + 259200},
            "access_token": "never-print-this-token",
            "conversation": "never-print-this-conversation"
        }))
        .unwrap(),
    )
    .unwrap();
    let codex = root().join("codex/sessions/2026/11/12");
    fs::create_dir_all(&codex).unwrap();
    let rollout = codex.join("rollout-fixture.jsonl");
    fs::write(
        &rollout,
        serde_json::to_vec(&serde_json::json!({"payload": {
            "rate_limits": {
                "primary": {"used_percent": 4, "window_minutes": 10080, "resets_at": NOW + 259200},
                "secondary": {"used_percent": 28, "window_minutes": 300, "resets_at": NOW + 10800}
            },
            "access_token": "never-print-this-token",
            "text": "never-print-this-conversation"
        }}))
        .unwrap(),
    )
    .unwrap();
    File::options()
        .write(true)
        .open(rollout)
        .unwrap()
        .set_times(
            FileTimes::new().set_modified(UNIX_EPOCH + Duration::from_secs((NOW - 120) as u64)),
        )
        .unwrap();
    fs::create_dir_all(root().join(".claude")).unwrap();
    fs::write(
        root().join(".claude/.credentials.json"),
        br#"{"claudeAiOauth":{"accessToken":"synthetic-test-token"}}"#,
    )
    .unwrap();
    fs::write(root().join("codex-response"), b"{\"id\":2,\"result\":{\"rateLimitResetCredits\":{\"availableCount\":0,\"credits\":[]}}}\n").unwrap();
}

pub(super) fn run(args: &[&str]) -> (u8, String) {
    let args: Vec<String> = args.iter().map(|arg| (*arg).to_owned()).collect();
    let request = cli::parse(&args).unwrap();
    let mut output = Vec::new();
    let code = execute(request, &FixedClock(NOW), &mut output).unwrap();
    (code, String::from_utf8(output).unwrap())
}

fn reset_queries() {
    for name in ["claude-resets.json", "codex-resets.json"] {
        let _ = fs::remove_file(root().join("cache/tmux-agent-usage").join(name));
    }
    for name in ["security-called", "curl-input", "codex-args"] {
        let _ = fs::remove_file(root().join(name));
    }
}

fn assert_accesses(events: &[access::Access], claude: bool, codex: bool, query_claude: bool) {
    assert!(
        !events.is_empty(),
        "observation must see selected local reads"
    );
    for event in events {
        if event.operation == "command" {
            let program = event.target.to_str().unwrap();
            assert!(
                matches!(program, "tmux" | "security" | "claude" | "curl" | "codex"),
                "unexpected process: {events:?}"
            );
            assert!(
                codex || program != "codex",
                "unselected Codex process: {events:?}"
            );
            assert!(
                query_claude || !matches!(program, "security" | "claude" | "curl"),
                "disabled Claude query process: {events:?}"
            );
            continue;
        }
        assert!(
            event.target.starts_with(root()),
            "access outside temporary HOME: {event:?}"
        );
        let target = event.target.strip_prefix(root()).unwrap().to_string_lossy();
        assert!(
            claude || !target.contains("claude"),
            "unselected Claude access: {events:?}"
        );
        assert!(
            codex || !target.contains("codex"),
            "unselected Codex access: {events:?}"
        );
        if !query_claude {
            assert!(
                !target.contains("claude-resets")
                    && !target.ends_with(".credentials.json")
                    && !target.contains(".local/share/claude"),
                "disabled Claude query access: {events:?}"
            );
        }
    }
}

#[test]
fn selection_gates_local_reads_processes_queries_and_cache_writes() {
    if !isolated("contract_tests::selection_gates_local_reads_processes_queries_and_cache_writes") {
        return;
    }
    seed();
    for (list, claude, codex) in [
        ("claude", true, false),
        ("codex", false, true),
        ("claude codex", true, true),
        ("codex claude", true, true),
    ] {
        for command in ["status", "inspect", "popup", "details"] {
            if command == "status" && claude && codex {
                continue;
            } // Each status invocation selects exactly one agent.
            for opt_in in [false, true] {
                if opt_in && matches!(command, "status" | "inspect") {
                    continue;
                }
                reset_queries();
                let mut args = match command {
                    "status" => vec![command, list],
                    "popup" => vec![command, "fixture", "160", "60", "--agents", list],
                    "details" => vec![command, "--no-wait", "--agents", list],
                    _ => vec![command, "--agents", list],
                };
                if opt_in {
                    args.push("--claude-resets");
                }
                access::start();
                let (code, text) = run(&args);
                let events = access::take();
                assert_eq!(code, 0);
                assert_accesses(&events, claude, codex, claude && opt_in);
                assert!(events.iter().any(|event| event.operation
                    == if command == "status" && codex {
                        "open"
                    } else {
                        "read_dir"
                    }));
                if matches!(command, "status" | "inspect") {
                    assert!(
                        events.iter().all(|event| event.operation != "command"
                            && !event.target.to_string_lossy().contains("-resets")),
                        "status/inspect performed a query: {events:?}"
                    );
                }
                if command == "details" {
                    assert_eq!(root().join("codex-args").exists(), codex);
                    assert_eq!(root().join("curl-input").exists(), claude && opt_in);
                    assert_eq!(
                        root()
                            .join("cache/tmux-agent-usage/codex-resets.json")
                            .exists(),
                        codex
                    );
                    assert_eq!(
                        root()
                            .join("cache/tmux-agent-usage/claude-resets.json")
                            .exists(),
                        claude && opt_in
                    );
                    for (name, enabled) in
                        [("codex-resets", codex), ("claude-resets", claude && opt_in)]
                    {
                        assert_eq!(
                            events.iter().any(|event| event.operation == "write"
                                && event.target.to_string_lossy().contains(name)),
                            enabled,
                            "missing positive cache-write observation: {events:?}"
                        );
                    }
                    if claude && !codex && !opt_in {
                        assert!(!text.contains("Saved resets"));
                    }
                    assert_eq!(text.contains("Claude Code"), claude);
                    assert_eq!(text.contains("Codex"), codex);
                }
            }
        }
    }
}

#[test]
fn invalid_arguments_are_rejected_before_any_access() {
    if !isolated("contract_tests::invalid_arguments_are_rejected_before_any_access") {
        return;
    }
    let mut cases = vec![
        vec!["status"],
        vec!["status", ""],
        vec!["status", "unknown"],
        vec!["status", "claude", "codex"],
        vec!["status", "claude", "--agents", "codex"],
        vec!["status", "claude", "--style-normal"],
        vec!["status", "claude", "--unknown", "fg=red"],
        vec!["popup"],
        vec!["popup", ""],
        vec!["popup", "fixture", "0", "24"],
        vec!["popup", "fixture", "80"],
        vec!["popup", "fixture", "bad", "24"],
        vec!["details", "surplus"],
        vec!["inspect", "--claude-resets"],
        vec!["claude-tap", "--unknown"],
        vec!["help", "surplus"],
    ];
    for list in [
        "",
        "  ",
        "claude claude",
        "codex codex",
        "other",
        "claude other",
    ] {
        for command in ["popup", "details", "inspect"] {
            let mut args = vec![command];
            if command == "popup" {
                args.push("fixture");
            }
            args.extend(["--agents", list]);
            cases.push(args);
        }
    }
    for style in [
        "",
        "fg=",
        "bg=",
        "fg=rainbow",
        "fg=colour256",
        "fg=#12345",
        "fg=#gg0000",
        "fg=red,unknown",
        "fg=red,,bold",
        "#[fg=red]",
        "fg=red]broken",
        "fg=red\n",
        "fg=red\x1b",
        "fg=#{pane_id}",
        "fg=red;touch x",
    ] {
        cases.push(vec!["status", "claude", "--style-normal", style]);
    }
    for args in cases {
        let args: Vec<String> = args.iter().map(|arg| (*arg).to_owned()).collect();
        access::start();
        assert!(
            cli::parse(&args).is_err(),
            "accepted invalid args: {args:?}"
        );
        assert!(access::take().is_empty());
    }
}

#[test]
fn order_and_single_column_geometry_follow_selection() {
    if !isolated("contract_tests::order_and_single_column_geometry_follow_selection") {
        return;
    }
    seed();
    assert_eq!(
        run(&["details", "--no-wait"]).1,
        run(&["details", "--no-wait", "--agents", "claude codex"]).1
    );
    for list in ["claude codex", "codex claude", "claude", "codex"] {
        let status: Vec<String> = list
            .split_whitespace()
            .map(|agent| run(&["status", agent]).1)
            .collect();
        let expected = if list.starts_with("claude") {
            vec!["7% · 2%", "28% · 4%"]
        } else {
            vec!["28% · 4%", "7% · 2%"]
        };
        assert_eq!(status, expected[..list.split_whitespace().count()]);
        for size in [(60, 160), (100, 80)] {
            crate::terminal::test_size(size);
            let (_, text) = run(&["details", "--no-wait", "--agents", list]);
            let frame = text.rsplit("\x1b[H\x1b[2J").next().unwrap();
            let titles: Vec<&str> = list
                .split_whitespace()
                .map(|agent| {
                    if agent == "claude" {
                        "Claude Code"
                    } else {
                        "Codex"
                    }
                })
                .collect();
            for pair in titles.windows(2) {
                assert!(frame.find(pair[0]).unwrap() < frame.find(pair[1]).unwrap());
            }
            let footer = frame.lines().last().unwrap();
            for pair in titles.windows(2) {
                assert!(footer.find(pair[0]).unwrap() < footer.find(pair[1]).unwrap());
            }
            if titles.len() == 1 {
                assert!(!frame.contains("│\x1b[0m  "));
            }
        }
        run(&[
            "popup",
            "fixture",
            "160",
            "60",
            "--agents",
            list,
            "--claude-resets",
        ]);
        let args = fs::read_to_string(root().join("tmux-args")).unwrap();
        let args: Vec<&str> = args.lines().collect();
        assert_eq!(
            args[args.iter().position(|arg| *arg == "-w").unwrap() + 1],
            if list.contains(' ') { "115" } else { "56" }
        );
        assert!(
            args.last()
                .unwrap()
                .ends_with(&format!("'details' '--agents' '{list}' '--claude-resets'"))
        );
        run(&["popup", "fixture", "20", "10", "--agents", list]);
        let args = fs::read_to_string(root().join("tmux-args")).unwrap();
        let args: Vec<&str> = args.lines().collect();
        assert_eq!(
            args[args.iter().position(|arg| *arg == "-w").unwrap() + 1],
            "20"
        );
        assert_eq!(
            args[args.iter().position(|arg| *arg == "-h").unwrap() + 1],
            "10"
        );
    }
}

#[test]
fn status_preserves_semantic_styles_and_plain_text() {
    if !isolated("contract_tests::status_preserves_semantic_styles_and_plain_text") {
        return;
    }
    seed();
    let flags = [
        "--style-normal",
        "fg=white,bg=black,nobold",
        "--style-high",
        "fg=colour220,bg=colour0,bold",
        "--style-critical",
        "fg=#ff0000,bg=#000000,reverse",
        "--style-muted",
        "fg=brightblack,bg=black,dim",
        "--style-reset-soon",
        "fg=green,bg=black,italics",
    ];
    let normal = "#[fg=white,bg=black,nobold]";
    let high = "#[fg=colour220,bg=colour0,bold]";
    let critical = "#[fg=#ff0000,bg=#000000,reverse]";
    let muted = "#[fg=brightblack,bg=black,dim]";
    let soon = "#[fg=green,bg=black,italics]";
    let mut args = vec!["status", "claude"];
    args.extend(flags);
    assert_eq!(
        run(&args).1,
        format!("{normal}7%{muted} · {normal}2%{normal}")
    );
    let path = root().join("cache/tmux-agent-usage/claude/fixture.json");
    for (five, seven, expected, plain) in [
        (
            serde_json::json!({"used_percentage":70,"resets_at":NOW+10800}),
            serde_json::json!({"used_percentage":95,"resets_at":NOW+259200}),
            format!("{high}70% {muted}↻3h{muted} · {critical}95% {muted}↻3d{normal}"),
            "70% ↻3h · 95% ↻3d",
        ),
        (
            serde_json::json!({"used_percentage":20,"resets_at":NOW+2400}),
            serde_json::Value::Null,
            format!("{normal}20% {soon}↻40m{muted} · {muted}–{normal}"),
            "20% ↻40m · –",
        ),
        (
            serde_json::json!({"used_percentage":95,"resets_at":NOW}),
            serde_json::json!({"used_percentage":0,"resets_at":NOW+259200}),
            format!("{normal}0%{muted} · {normal}0%{normal}"),
            "0% · 0%",
        ),
    ] {
        fs::write(
            &path,
            serde_json::to_vec(
                &serde_json::json!({"observed_at":NOW,"five_hour":five,"seven_day":seven}),
            )
            .unwrap(),
        )
        .unwrap();
        assert_eq!(run(&args).1, expected);
        let text = run(&["status", "claude"]).1;
        assert_eq!(text, plain);
        assert!(!text.contains("#[") && !text.ends_with('\n'));
    }
    // Supplying any one flag enables documented defaults for omitted roles.
    fs::write(&path, b"malformed JSON").unwrap();
    assert_eq!(
        run(&["status", "claude", "--style-normal", "fg=blue"]).1,
        "#[fg=brightblack]–#[fg=blue]"
    );
    assert_eq!(run(&["status", "claude"]).1, "–");
}

#[test]
fn missing_failed_and_checked_zero_remain_distinct_and_inspect_is_redacted() {
    if !isolated(
        "contract_tests::missing_failed_and_checked_zero_remain_distinct_and_inspect_is_redacted",
    ) {
        return;
    }
    seed();
    let (_, inspect) = run(&["inspect"]);
    assert!(!inspect.contains("never-print") && !inspect.contains("synthetic-test-token"));
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&inspect).unwrap()["now"],
        NOW
    );
    let (_, zero) = run(&["details", "--agents", "codex", "--no-wait"]);
    assert!(zero.contains("none") && !zero.contains("couldn't check"));
    reset_queries();
    fs::write(
        root().join("codex-response"),
        b"{\"id\":2,\"error\":{\"message\":\"synthetic failure\"}}\n",
    )
    .unwrap();
    let (_, failed) = run(&["details", "--agents", "codex", "--no-wait"]);
    assert!(failed.contains("couldn't check") && failed.contains("28%"));
    assert!(
        !root()
            .join("cache/tmux-agent-usage/codex-resets.json")
            .exists()
    );
    fs::remove_dir_all(root().join("codex/sessions")).unwrap();
    assert_eq!(run(&["status", "codex"]).1, "–");
    let (_, missing) = run(&["details", "--agents", "codex", "--no-wait"]);
    assert!(missing.contains("No usage data yet"));
    let (_, inspect) = run(&["inspect", "--agents", "codex"]);
    let inspect: serde_json::Value = serde_json::from_str(&inspect).unwrap();
    assert!(inspect["codex"].is_null() && inspect.get("claude").is_none());
}

#[test]
fn tap_clock_and_no_record_control_all_snapshot_accesses() {
    if !isolated("contract_tests::tap_clock_and_no_record_control_all_snapshot_accesses") {
        return;
    }
    let payload = include_bytes!("../fixtures/baseline/tap.json");
    access::start();
    assert_eq!(
        crate::run_claude_tap(false, &[], &FixedClock(NOW), &mut payload.as_slice()).unwrap(),
        0
    );
    assert!(access::take().is_empty());
    assert!(!root().join("cache").exists());
    crate::run_claude_tap(true, &[], &FixedClock(NOW), &mut payload.as_slice()).unwrap();
    let path = root().join("cache/tmux-agent-usage/claude/fixture.json");
    let snapshot: serde_json::Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    assert_eq!(snapshot["observed_at"], NOW);
    let stale = path.with_file_name("stale.json");
    fs::write(&stale, b"{}").unwrap();
    File::options()
        .write(true)
        .open(&stale)
        .unwrap()
        .set_times(
            FileTimes::new()
                .set_modified(UNIX_EPOCH + Duration::from_secs((NOW - 9 * 86400) as u64)),
        )
        .unwrap();
    access::start();
    crate::run_claude_tap(false, &[], &FixedClock(NOW + 60), &mut payload.as_slice()).unwrap();
    assert!(access::take().is_empty());
    assert!(stale.exists());
    let mut changed: serde_json::Value = serde_json::from_slice(payload).unwrap();
    changed["rate_limits"]["five_hour"]["used_percentage"] = 8.into();
    let changed = serde_json::to_vec(&changed).unwrap();
    crate::run_claude_tap(true, &[], &FixedClock(NOW + 60), &mut changed.as_slice()).unwrap();
    assert!(!stale.exists());
    let snapshot: serde_json::Value = serde_json::from_slice(&fs::read(path).unwrap()).unwrap();
    assert_eq!(snapshot["observed_at"], NOW + 60);
    struct BrokenInput;
    impl io::Read for BrokenInput {
        fn read(&mut self, _: &mut [u8]) -> io::Result<usize> {
            Err(io::Error::other("synthetic input failure"))
        }
    }
    access::start();
    assert!(crate::run_claude_tap(true, &[], &FixedClock(NOW), &mut BrokenInput).is_err());
    assert!(access::take().is_empty());
}

#[test]
fn injected_clock_controls_query_cache_expiration() {
    if !isolated("contract_tests::injected_clock_controls_query_cache_expiration") {
        return;
    }
    seed();
    run(&["details", "--agents", "codex", "--no-wait"]);
    for (elapsed, queried) in [(299, false), (300, true)] {
        let request =
            cli::parse(&["details", "--agents", "codex", "--no-wait"].map(str::to_owned)).unwrap();
        access::start();
        execute(request, &FixedClock(NOW + elapsed), &mut Vec::new()).unwrap();
        let events = access::take();
        assert_eq!(
            events.iter().any(|event| event.operation == "command"
                && event.target == std::path::Path::new("codex")),
            queried,
            "cache expiration at {elapsed}s: {events:?}"
        );
        assert_eq!(
            events.iter().any(|event| event.operation == "write"),
            queried
        );
    }
    let cached: serde_json::Value = serde_json::from_slice(
        &fs::read(root().join("cache/tmux-agent-usage/codex-resets.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(cached["fetched_at"], NOW + 300);
}
