mod support;

use std::fs;
use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use support::{Sandbox, success};

const BINARY: &str = env!("CARGO_BIN_EXE_tmux-agent-usage");

const VALID_STYLES: &[&str] = &[
    "fg=default",
    "fg=yellow",
    "fg=red",
    "fg=brightblack",
    "fg=green",
    "fg=#fbf1c7,bg=#3c3836,bold",
    "fg=white,bg=black,nobold",
    "fg=colour220,bg=colour0,bold",
    "fg=#ff0000,bg=#000000,reverse",
    "fg=brightblack,bg=black,dim",
    "fg=green,bg=black,italics",
    "fg=terminal,bg=default,us=colour33,curly-underscore",
    "fg=yellow bold underscore blink",
    "default",
    "none",
];

fn executable(sandbox: &Sandbox, name: &str, script: &str) -> PathBuf {
    let executable = sandbox.root.join(name);
    fs::write(&executable, script).unwrap();
    fs::set_permissions(&executable, fs::Permissions::from_mode(0o700)).unwrap();
    executable
}

#[test]
fn usage_and_unknown_command_exit_codes() {
    let sandbox = Sandbox::new();
    for args in [vec![], vec!["unknown"], vec!["popup"]] {
        let output = sandbox.command(BINARY).args(args).output().unwrap();
        assert_eq!(output.status.code(), Some(2));
        assert!(output.stdout.is_empty());
        assert!(String::from_utf8_lossy(&output.stderr).contains("usage:"));
    }
    for help in ["help", "--help", "-h"] {
        let output = success(sandbox.command(BINARY).arg(help).output().unwrap());
        assert!(String::from_utf8_lossy(&output.stdout).contains("claude-tap"));
        assert!(output.stderr.is_empty());
    }
}

#[test]
fn status_requires_an_agent_and_never_falls_back_to_pills() {
    let sandbox = Sandbox::new();
    let output = sandbox.command(BINARY).arg("status").output().unwrap();
    assert_eq!(output.status.code(), Some(2));
    assert!(output.stdout.is_empty());
    assert!(String::from_utf8_lossy(&output.stderr).contains("status requires an agent"));
    assert!(!sandbox.root.join("cache").exists());
}

#[test]
fn binary_has_no_consumer_logos_brand_colors_or_pill_geometry() {
    let binary = fs::read(BINARY).unwrap();
    for forbidden in [
        "\u{100006}",
        "\u{100007}",
        "#d97757",
        "#000000",
        "\u{e0b6}",
        "\u{e0b4}",
        "#[range=user|",
        "#[range=right]",
    ] {
        assert!(
            !binary
                .windows(forbidden.len())
                .any(|bytes| bytes == forbidden.as_bytes()),
            "consumer presentation embedded in binary: {forbidden:?}"
        );
    }
}

fn tap(sandbox: &Sandbox, payload: &[u8], args: &[&str]) -> std::process::Output {
    let mut child = sandbox
        .command(BINARY)
        .arg("claude-tap")
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child.stdin.take().unwrap().write_all(payload).unwrap();
    child.wait_with_output().unwrap()
}

#[test]
fn tap_preserves_payload_stdout_stderr_and_wrapped_exit() {
    let sandbox = Sandbox::new();
    let wrapped = executable(
        &sandbox,
        "wrapped",
        "#!/bin/sh\n/bin/cat\nprintf 'wrapped stderr' >&2\nexit 37\n",
    );
    let payload = include_bytes!("fixtures/baseline/tap.json");
    let output = tap(&sandbox, payload, &["--", wrapped.to_str().unwrap()]);
    assert_eq!(output.stdout, payload);
    assert_eq!(output.stderr, b"wrapped stderr");
    assert_eq!(output.status.code(), Some(37));
    let snapshot = sandbox
        .root
        .join("cache/tmux-agent-usage/claude/fixture.json");
    let recorded: serde_json::Value = serde_json::from_slice(&fs::read(snapshot).unwrap()).unwrap();
    let input: serde_json::Value = serde_json::from_slice(payload).unwrap();
    for window in ["five_hour", "seven_day"] {
        for field in ["used_percentage", "resets_at"] {
            assert_eq!(
                recorded[window][field].as_f64().unwrap(),
                input["rate_limits"][window][field].as_f64().unwrap()
            );
        }
    }

    // Malformed JSON and a failed cache write must not break the wrapper.
    let invalid = b"not JSON\0\nwith trailing bytes\xff";
    let output = tap(&sandbox, invalid, &["--", wrapped.to_str().unwrap()]);
    assert_eq!(output.stdout, invalid);
    assert_eq!(output.status.code(), Some(37));
    fs::remove_dir_all(sandbox.root.join("cache")).unwrap();
    fs::write(sandbox.root.join("cache"), b"not a directory").unwrap();
    let output = tap(&sandbox, payload, &["--", wrapped.to_str().unwrap()]);
    assert_eq!(output.stdout, payload);
    assert_eq!(output.status.code(), Some(37));
}

#[test]
fn tap_without_wrapper_and_failed_spawn() {
    let sandbox = Sandbox::new();
    let output = tap(&sandbox, b"invalid JSON", &[]);
    assert!(output.status.success());
    assert!(output.stdout.is_empty());
    let output = tap(&sandbox, b"{}", &["--", "/no-such-fixture-command"]);
    assert_eq!(output.status.code(), Some(1));
    assert!(output.stdout.is_empty());
    assert!(String::from_utf8_lossy(&output.stderr).contains("cannot run"));
}

#[test]
fn installers_pass_the_source_and_explicit_temporary_root_to_fake_cargo() {
    let sandbox = Sandbox::new();
    executable(
        &sandbox,
        "cargo",
        "#!/bin/sh\nprintf '%s\\n' \"$@\" > \"$AGENT_USAGE_TEST_ARGS\"\n",
    );
    let args_file = sandbox.root.join("cargo-args");
    let crate_dir = Path::new(env!("CARGO_MANIFEST_DIR"))
        .canonicalize()
        .unwrap();
    {
        let mut command = sandbox.command("/bin/sh");
        command
            .arg(crate_dir.join("scripts/install.sh"))
            .arg(sandbox.root.join(".local"));
        success(
            command
                .env("PATH", format!("{}:/usr/bin:/bin", sandbox.root.display()))
                .env("AGENT_USAGE_TEST_ARGS", &args_file)
                .output()
                .unwrap(),
        );
        let args = fs::read_to_string(&args_file).unwrap();
        let args: Vec<&str> = args.lines().collect();
        assert_eq!(
            args,
            [
                "install",
                "--locked",
                "--path",
                crate_dir.to_str().unwrap(),
                "--root",
                sandbox.root.join(".local").to_str().unwrap(),
                "--force"
            ]
        );
    }
    assert!(!sandbox.root.join(".local").exists());

    for args in [vec![], vec![""], vec!["one", "two"]] {
        let output = sandbox
            .command("/bin/sh")
            .arg(crate_dir.join("scripts/install.sh"))
            .args(args)
            .env("PATH", format!("{}:/usr/bin:/bin", sandbox.root.display()))
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(2));
        assert_eq!(output.stderr, b"usage: install.sh ROOT\n");
    }
}

struct IsolatedTmux {
    sandbox: Sandbox,
    started: bool,
}

impl IsolatedTmux {
    fn command(&self) -> Command {
        let mut command = self.sandbox.command("tmux");
        command
            .arg("-S")
            .arg(self.sandbox.root.join("socket"))
            .args(["-f", "/dev/null", "-u"]);
        command
    }
}

impl Drop for IsolatedTmux {
    fn drop(&mut self) {
        if self.started {
            let _ = self.command().arg("kill-server").output();
        }
    }
}

#[test]
fn isolated_tmux_accepts_popup_flags_styles_and_status_ranges() {
    let mut tmux = IsolatedTmux {
        sandbox: Sandbox::new(),
        started: false,
    };
    let output = tmux
        .command()
        .args([
            "new-session",
            "-d",
            "-s",
            "baseline",
            "-x",
            "160",
            "-y",
            "60",
            "/bin/sleep 30",
        ])
        .output()
        .expect("tmux is required for the isolated baseline probe");
    success(output); // An installed but unusable tmux must fail the test.
    tmux.started = true;
    let output = success(
        tmux.command()
            .args(["list-commands", "display-popup"])
            .output()
            .unwrap(),
    );
    let syntax = String::from_utf8(output.stdout).unwrap();
    for flag in [
        "-c target-client",
        "-b border-lines",
        "-S border-style",
        "-T title",
        "-w width",
        "-h height",
    ] {
        assert!(syntax.contains(flag), "missing popup flag: {flag}");
    }
    for &style in VALID_STYLES {
        success(
            tmux.command()
                .args(["set-option", "-g", "status-style", style])
                .output()
                .unwrap(),
        );
    }
    success(
        tmux.command()
            .args(["set-option", "-g", "popup-border-style", "fg=#665c54"])
            .output()
            .unwrap(),
    );
    success(
        tmux.command()
            .args(["set-option", "-g", "popup-border-lines", "rounded"])
            .output()
            .unwrap(),
    );
    let config = tmux.sandbox.root.join("probe.conf");
    fs::write(&config, "display-popup -c fixture -E -b rounded -S 'fg=#665c54' -T '#[fg=#fbf1c7] Agent usage ' -w 115 -h 30 true\n").unwrap();
    success(
        tmux.command()
            .args(["source-file", "-n"])
            .arg(config)
            .output()
            .unwrap(),
    );
    let pill = std::str::from_utf8(include_bytes!("goldens/baseline/status-missing.tmux")).unwrap();
    success(
        tmux.command()
            .args(["set-option", "-g", "status-right", pill])
            .output()
            .unwrap(),
    );
    let output = success(
        tmux.command()
            .args(["show-options", "-gv", "status-right"])
            .output()
            .unwrap(),
    );
    assert_eq!(
        String::from_utf8(output.stdout).unwrap(),
        format!("{pill}\n")
    );
    success(tmux.command().arg("kill-server").output().unwrap());
    tmux.started = false;
}

#[test]
fn public_status_and_usage_errors_have_exact_output_and_exit_codes() {
    let sandbox = Sandbox::new();
    for agent in ["claude", "codex"] {
        let output = success(
            sandbox
                .command(BINARY)
                .args(["status", agent])
                .output()
                .unwrap(),
        );
        assert_eq!(output.stdout, "–".as_bytes());
        assert!(output.stderr.is_empty());
    }
    for &style in VALID_STYLES {
        let output = success(
            sandbox
                .command(BINARY)
                .args(["status", "claude", "--style-normal", style])
                .output()
                .unwrap(),
        );
        assert_eq!(
            output.stdout,
            format!("#[fg=brightblack]–#[{style}]").as_bytes()
        );
        assert!(output.stderr.is_empty());
    }
    for args in [
        vec!["status", "unknown"],
        vec!["status", "claude", "codex"],
        vec!["status", "claude", "--style-normal", "#[fg=red]"],
        vec!["status", "codex", "--style-high", "fg=unknown"],
        vec!["status", "claude", "--style-normal", ""],
        vec!["details", "--agents", ""],
        vec!["details", "--agents", "claude claude"],
        vec!["popup", "fixture", "--agents", ""],
        vec!["inspect", "--agents", ""],
        vec!["claude-tap", "--unknown"],
    ] {
        let output = sandbox.command(BINARY).args(&args).output().unwrap();
        assert_eq!(output.status.code(), Some(2), "args: {args:?}");
        assert!(output.stdout.is_empty(), "args: {args:?}");
        let diagnostic = String::from_utf8_lossy(&output.stderr);
        assert!(diagnostic.contains("tmux-agent-usage:") && diagnostic.contains("usage:"));
        if args.iter().any(|arg| arg.starts_with("--style-")) {
            assert!(diagnostic.contains("invalid style"));
        }
    }
    assert!(!sandbox.root.join("cache").exists());
}

#[test]
fn tap_no_record_preserves_bytes_stdout_stderr_and_nonzero_exit_without_cache() {
    let sandbox = Sandbox::new();
    let wrapped = executable(
        &sandbox,
        "wrapped",
        "#!/bin/sh\n/bin/cat\nprintf 'wrapped stderr' >&2\nexit 37\n",
    );
    for payload in [
        include_bytes!("fixtures/baseline/tap.json").as_slice(),
        b"not JSON\0\nnon-UTF8: \xff".as_slice(),
    ] {
        let output = tap(
            &sandbox,
            payload,
            &["--no-record", "--", wrapped.to_str().unwrap()],
        );
        assert_eq!(output.stdout, payload);
        assert_eq!(output.stderr, b"wrapped stderr");
        assert_eq!(output.status.code(), Some(37));
        assert!(!sandbox.root.join("cache").exists());
    }
    let output = tap(&sandbox, b"{}", &["--no-record"]);
    assert!(output.status.success() && output.stdout.is_empty() && output.stderr.is_empty());
    assert!(!sandbox.root.join("cache").exists());
    let wrapped = executable(
        &sandbox,
        "wrapper-options",
        "#!/bin/sh\n/bin/cat\nprintf '%s\\n' \"$@\"\n",
    );
    let output = tap(
        &sandbox,
        b"payload\n",
        &["--", wrapped.to_str().unwrap(), "--no-record"],
    );
    assert!(output.status.success());
    assert_eq!(output.stdout, b"payload\n--no-record\n");
}
