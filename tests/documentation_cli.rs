#[path = "support/documentation.rs"]
mod documentation;
mod support;

use documentation::snippet;
use serde_json::Value;
use std::fs;
use std::io::Write;
use std::os::unix::fs::{PermissionsExt, symlink};
use std::path::Path;
use std::process::{Output, Stdio};
use support::{Sandbox, success};

const BINARY: &str = env!("CARGO_BIN_EXE_tmux-agent-usage");

fn executable(path: &Path, script: &str) {
    fs::write(path, script).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o700)).unwrap();
}

fn script(sandbox: &Sandbox, code: &str, payload: &[u8]) -> Output {
    let mut child = sandbox
        .command("/bin/sh")
        .args(["-eu", "-c", code])
        .env("PATH", sandbox.root.join("bin"))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child.stdin.take().unwrap().write_all(payload).unwrap();
    child.wait_with_output().unwrap()
}

fn setup() -> Sandbox {
    let sandbox = Sandbox::new();
    fs::create_dir(sandbox.root.join("bin")).unwrap();
    symlink(BINARY, sandbox.root.join("bin/tmux-agent-usage")).unwrap();
    // Every possible provider helper fails visibly. Status, inspect and the
    // tap examples must work without running any of these or using the network.
    for program in ["codex", "claude", "security", "curl", "tmux"] {
        executable(
            &sandbox.root.join("bin").join(program),
            "#!/bin/sh\nprintf '%s\\n' \"$0\" >> \"$HOME/forbidden\"\nexit 97\n",
        );
    }
    sandbox
}

fn inspect(sandbox: &Sandbox, expected: &str) -> Value {
    let output = success(
        sandbox
            .command(BINARY)
            .args(["inspect", "--agents", "claude"])
            .env("PATH", sandbox.root.join("bin"))
            .output()
            .unwrap(),
    );
    let value: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(value["claude_snapshot_status"], expected);
    assert!(value.get("codex").is_none());
    assert!(
        !String::from_utf8(output.stdout)
            .unwrap()
            .contains("private-conversation")
    );
    value
}

#[test]
fn documented_claude_settings_record_with_and_without_a_wrapper_and_preserve_failures() {
    for (name, wrapped) in [("tap-only", false), ("tap-wrapped", true)] {
        let sandbox = setup();
        let installed = sandbox.root.join("installed root with spaces");
        fs::create_dir(&installed).unwrap();
        symlink(BINARY, installed.join("tmux-agent-usage")).unwrap();
        executable(
            &sandbox.root.join("bin/ccstatusline"),
            "#!/bin/sh\n/bin/cat\nprintf 'wrapper stderr' >&2\nexit 37\n",
        );
        let settings: Value = serde_json::from_str(&snippet(name, "json")).unwrap();
        let code = settings["statusLine"]["command"].as_str().unwrap().replace(
            "/your/chosen/root/bin/tmux-agent-usage",
            installed.join("tmux-agent-usage").to_str().unwrap(),
        );
        inspect(&sandbox, "no_snapshots");
        assert!(!sandbox.root.join("cache").exists());
        let payload = include_bytes!("fixtures/baseline/tap.json");
        let output = script(&sandbox, &code, payload);
        assert_eq!(output.status.code(), Some(if wrapped { 37 } else { 0 }));
        assert_eq!(
            output.stdout,
            if wrapped { payload.as_slice() } else { b"" }
        );
        assert_eq!(
            output.stderr,
            if wrapped {
                b"wrapper stderr".as_slice()
            } else {
                b""
            }
        );
        let report = inspect(&sandbox, "available");
        assert_eq!(report["claude"]["five_hour"]["used_percentage"], 7.0);
        assert!(!sandbox.root.join("forbidden").exists());

        // Read the documented --no-record command verbatim, using a fresh
        // cache path to prove it forwards even malformed bytes without writes.
        let disabled = Sandbox::new();
        let mut child = disabled
            .command("/bin/sh")
            .args(["-eu", "-c", &snippet("tap-disabled", "sh")])
            .env("PATH", sandbox.root.join("bin"))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let invalid = b"invalid JSON\0\xff\nprivate-conversation";
        child.stdin.take().unwrap().write_all(invalid).unwrap();
        let output = child.wait_with_output().unwrap();
        assert_eq!(output.status.code(), Some(37));
        assert_eq!(output.stdout, invalid);
        assert_eq!(output.stderr, b"wrapper stderr");
        assert!(!disabled.root.join("cache").exists());
    }
}

#[test]
fn inspect_distinguishes_unrecorded_empty_incompatible_and_unreadable_snapshots() {
    let sandbox = setup();
    inspect(&sandbox, "no_snapshots");
    success(script(
        &sandbox,
        "tmux-agent-usage claude-tap",
        br#"{"session_id":"no-limits"}"#,
    ));
    inspect(&sandbox, "no_snapshots");
    let dir = sandbox.root.join("cache/tmux-agent-usage/claude");
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("unrelated.txt"), b"private-conversation").unwrap();
    inspect(&sandbox, "no_snapshots");
    fs::write(dir.join("bad.json"), b"private-conversation").unwrap();
    inspect(&sandbox, "invalid_snapshots");
    // This is the snapshot written by a tap receiving rate_limits: {}.
    success(script(
        &sandbox,
        "tmux-agent-usage claude-tap",
        br#"{"session_id":"empty","rate_limits":{}}"#,
    ));
    let report = inspect(&sandbox, "no_usage");
    assert!(report["claude"]["five_hour"].is_null());
    assert!(report["claude"]["seven_day"].is_null());
    fs::remove_dir_all(&dir).unwrap();
    fs::write(&dir, b"not a directory").unwrap();
    inspect(&sandbox, "unreadable");
    let codex = success(
        sandbox
            .command(BINARY)
            .args(["inspect", "--agents", "codex"])
            .output()
            .unwrap(),
    );
    let codex: Value = serde_json::from_slice(&codex.stdout).unwrap();
    assert!(codex.get("claude_snapshot_status").is_none());
    let unavailable = success(
        sandbox
            .command(BINARY)
            .args(["inspect", "--agents", "claude"])
            .env_remove("HOME")
            .env_remove("XDG_CACHE_HOME")
            .output()
            .unwrap(),
    );
    let unavailable: Value = serde_json::from_slice(&unavailable.stdout).unwrap();
    assert_eq!(unavailable["claude_snapshot_status"], "path_unavailable");
    assert!(!sandbox.root.join("forbidden").exists());
}

#[test]
fn documented_status_and_inspect_commands_work_offline_without_credentials() {
    let sandbox = setup();
    for line in snippet("status", "sh").lines() {
        let output = success(script(&sandbox, line, b""));
        assert!(output.stderr.is_empty());
        let text = String::from_utf8(output.stdout).unwrap();
        assert!(text.contains('–'));
        assert!(!text.ends_with('\n'));
        assert_eq!(text.contains("#["), line.contains("--style-"));
    }
    for line in snippet("inspect", "sh").lines() {
        let output = success(script(&sandbox, line, b""));
        let report: Value = serde_json::from_slice(&output.stdout).unwrap();
        assert!(report["claude"].is_null() && report["codex"].is_null());
    }
    for command in ["inspect", "details", "popup /dev/test 80 24"] {
        let output = script(
            &sandbox,
            &format!("tmux-agent-usage {command} --agents ''"),
            b"",
        );
        assert_eq!(output.status.code(), Some(2));
        assert!(output.stdout.is_empty());
        assert!(String::from_utf8_lossy(&output.stderr).contains("at least one agent"));
    }
    assert!(!sandbox.root.join("cache").exists());
    assert!(!sandbox.root.join("forbidden").exists());
}

#[test]
fn distribution_examples_use_pinned_cargo_and_the_stable_brew_prefix() {
    let sandbox = setup();
    executable(
        &sandbox.root.join("bin/cargo"),
        "#!/bin/sh\nprintf '%s\\n' \"$@\" > \"$HOME/cargo-args\"\n",
    );
    success(script(&sandbox, &snippet("cargo-registry", "sh"), b""));
    let args = fs::read_to_string(sandbox.root.join("cargo-args")).unwrap();
    let args: Vec<_> = args.lines().collect();
    assert_eq!(args[0], "install");
    assert!(args.contains(&"--locked"));
    let version = args.windows(2).find(|pair| pair[0] == "--version").unwrap()[1];
    assert_eq!(version, env!("CARGO_PKG_VERSION"));
    executable(
        &sandbox.root.join("bin/brew"),
        "#!/bin/sh\nprintf '%s\\n' \"$*\" >> \"$HOME/brew-args\"\nif [ \"$1\" = --prefix ]; then printf '%s\\n' \"$HOME/brew prefix\"; fi\n",
    );
    let output = success(script(&sandbox, &snippet("homebrew", "sh"), b""));
    let path = String::from_utf8(output.stdout).unwrap();
    assert_eq!(
        path.trim(),
        sandbox
            .root
            .join("brew prefix/bin/tmux-agent-usage")
            .to_str()
            .unwrap()
    );
    let commands = fs::read_to_string(sandbox.root.join("brew-args")).unwrap();
    assert_eq!(
        commands.lines().collect::<Vec<_>>(),
        ["install ramonmello/tap/tmux-agent-usage", "--prefix"]
    );
    assert!(!sandbox.root.join("forbidden").exists());
}
