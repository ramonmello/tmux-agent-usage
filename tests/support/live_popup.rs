//! Exercise the real popup through a private tmux server and attached PTY.
use crate::clock::FixedClock;
use crate::contract_tests::{executable, isolated, root, seed};
use crate::test_support::{Sandbox, success};
pub(super) use crate::tmux_tests_support::{Client, Server};
use std::fs::{self, File, FileTimes};
use std::io::Write;
use std::os::unix::fs::OpenOptionsExt;
use std::process::{Child, Stdio};
use std::time::{Duration, Instant, UNIX_EPOCH};

const TEST: &str =
    "live_popup_tests::isolated_tmux_redraws_palettes_handles_input_and_restores_terminal";
const NOW: i64 = 1_794_484_800;

#[test]
fn render_child() {
    if std::env::var("AGENT_USAGE_RENDER_CHILD").as_deref() != Ok("1") {
        return;
    }
    let request = crate::cli::parse(&["details", "--claude-resets"].map(str::to_owned)).unwrap();
    match crate::execute(request, &FixedClock(NOW), &mut std::io::stdout()) {
        Ok(code) => std::process::exit(code.into()),
        Err(error) => {
            eprintln!("fixture render failed: {error}");
            std::process::exit(1);
        }
    }
}

struct Popup(Child);
impl Drop for Popup {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

// Each fake helper opens its FIFO, announces readiness, and then blocks in a
// shell read. The parent releases one response at a time without polling child
// processes or relying on relative startup speed under compiler load.
struct ReleaseGate {
    _reader: File,
    writer: File,
}

impl ReleaseGate {
    fn new(name: &str) -> Self {
        let path = root().join(name);
        let cpath = std::ffi::CString::new(path.as_os_str().as_encoded_bytes()).unwrap();
        assert_eq!(unsafe { libc::mkfifo(cpath.as_ptr(), 0o600) }, 0);
        let reader = File::options()
            .read(true)
            .custom_flags(libc::O_NONBLOCK)
            .open(&path)
            .unwrap();
        let writer = File::options()
            .write(true)
            .custom_flags(libc::O_NONBLOCK)
            .open(&path)
            .unwrap();
        Self {
            _reader: reader,
            writer,
        }
    }

    fn release(&mut self) {
        self.writer.write_all(b"release\n").unwrap();
    }
}

fn reset_queries() {
    for file in [
        "cache/tmux-agent-usage/claude-resets.json",
        "cache/tmux-agent-usage/codex-resets.json",
        "codex-ready",
        "curl-ready",
        "release-codex",
        "release-claude",
        "terminal-before",
        "terminal-after",
        "terminal-cleanup-ready",
    ] {
        let _ = fs::remove_file(root().join(file));
    }
}

fn close(client: &mut Client, input: &[u8]) {
    client.send(input);
    client.until("popup closes and reveals the original pane", |screen| {
        screen.contents().contains("ORIGINAL PANE") && !screen.contents().contains("Agent usage")
    });
    client.until("popup terminal cleanup completes", |_| {
        root().join("terminal-cleanup-ready").exists()
    });
    assert_eq!(
        fs::read(root().join("terminal-before")).unwrap(),
        fs::read(root().join("terminal-after")).unwrap()
    );
    assert!(!client.parser.screen().hide_cursor());
    assert_eq!(
        client.parser.screen().mouse_protocol_mode(),
        vt100::MouseProtocolMode::None
    );
}

#[test]
fn isolated_tmux_redraws_palettes_handles_input_and_restores_terminal() {
    if std::env::var("AGENT_USAGE_CONTRACT_CHILD").as_deref() != Ok(TEST) {
        let program = std::env::split_paths(&std::env::var_os("PATH").unwrap())
            .map(|dir| dir.join("tmux"))
            .find(|file| file.is_file())
            .expect("tmux is required for the isolated popup test");
        let sandbox = Sandbox::new();
        let output = success(
            sandbox
                .command(std::env::current_exe().unwrap())
                .args(["--exact", TEST, "--nocapture"])
                .env("AGENT_USAGE_CONTRACT_CHILD", TEST)
                .env("AGENT_USAGE_REAL_TMUX", program)
                .env_remove("CLAUDE_CONFIG_DIR")
                .env_remove("CLAUDE_SECURESTORAGE_CONFIG_DIR")
                .env("PATH", sandbox.root.join("fakes"))
                .output()
                .unwrap(),
        );
        if std::env::var_os("AGENT_USAGE_TRACE_TESTS").is_some() {
            eprint!("{}", String::from_utf8_lossy(&output.stderr));
        }
        return;
    }
    assert!(isolated(TEST));
    seed();
    // The final answer grows beyond the initial checking line, exercising
    // redraw and fitting with reset entries and advice still present.
    let response = serde_json::json!({"id": 2, "result": {"rateLimitResetCredits": {
        "availableCount": 3, "credits": [
            {"status": "available", "expiresAt": NOW + 3600},
            {"status": "available", "expiresAt": NOW + 21 * 86400}
        ]
    }}});
    fs::write(root().join("codex-response"), format!("{response}\n")).unwrap();
    let fixture: serde_json::Value =
        serde_json::from_str(include_str!("../fixtures/baseline/popup.json")).unwrap();
    fs::write(root().join("cache/tmux-agent-usage/claude/fixture.json"), serde_json::to_vec(&serde_json::json!({ "observed_at": NOW - 3600, "five_hour": fixture["claude"]["five_hour"], "seven_day": fixture["claude"]["seven_day"] })).unwrap()).unwrap();
    fs::write(root().join("codex/sessions/2026/11/12/rollout-fixture.jsonl"), serde_json::to_vec(&serde_json::json!({"payload":{"rate_limits":{"primary":{"used_percent":95,"window_minutes":300,"resets_at":NOW+4800},"secondary":{"used_percent":20,"window_minutes":10080,"resets_at":NOW+172800},"plan_type":"pro","credits":{"has_credits":true,"unlimited":false,"balance":"12.50"}}}})).unwrap()).unwrap();
    File::options()
        .write(true)
        .open(root().join("codex/sessions/2026/11/12/rollout-fixture.jsonl"))
        .unwrap()
        .set_times(
            FileTimes::new().set_modified(UNIX_EPOCH + Duration::from_secs((NOW - 90) as u64)),
        )
        .unwrap();
    executable(
        "codex",
        r#"#!/bin/sh
IFS= read -r initialize
printf '{"id":1,"result":{}}\n'
IFS= read -r initialized
IFS= read -r request
exec 3< "$HOME/release-codex"
: > "$HOME/codex-ready"
IFS= read -r release <&3
[ "$release" = release ] || exit 1
/bin/cat "$HOME/codex-response"
"#,
    );
    executable(
        "curl",
        r#"#!/bin/sh
/bin/cat > /dev/null
exec 3< "$HOME/release-claude"
: > "$HOME/curl-ready"
IFS= read -r release <&3
[ "$release" = release ] || exit 1
printf 'HTTP/1.1 200 OK\r\n\r\n{"cedar_ember":{"eligible":true,"grants":[]}}\n200'
"#,
    );
    let wrapper = root().join("fakes/render-popup");
    let executable_path = crate::shell_quote(std::env::current_exe().unwrap().to_str().unwrap());
    executable(
        "render-popup",
        &format!(
            "#!/bin/sh\n/bin/stty -g > \"$HOME/terminal-before\"\nAGENT_USAGE_RENDER_CHILD=1 {executable_path} --exact live_popup_tests::render_child --nocapture\nresult=$?\n/bin/stty -g > \"$HOME/terminal-after\"\n: > \"$HOME/terminal-cleanup-ready\"\nexit \"$result\"\n"
        ),
    );
    let mut server = Server {
        program: std::env::var_os("AGENT_USAGE_REAL_TMUX").unwrap().into(),
        socket: root().join("popup-socket"),
        started: false,
    };
    server.run(&[
        "new-session",
        "-d",
        "-s",
        "popup-test",
        "-x",
        "120",
        "-y",
        "36",
        "printf 'ORIGINAL PANE\\n'; /bin/cat",
    ]);
    server.started = true;
    server.run(&["set-option", "-g", "status", "off"]);
    server.run(&["set-option", "-g", "mouse", "off"]);
    server.run(&["set-option", "-g", "popup-style", "fg=default,bg=default"]);
    server.run(&["set-option", "-g", "popup-border-style", "fg=magenta"]);
    let mut client = Client::attach(&server);
    client.until("client attaches", |screen| {
        screen.contents().contains("ORIGINAL PANE")
    });
    let target = server
        .run(&["list-clients", "-F", "#{client_name}"])
        .trim()
        .to_owned();
    for (index, name, width, height, input) in [
        (0, "default-dark", "115", "30", b"q".as_slice()),
        (1, "default-light", "115", "30", b"\x1b".as_slice()),
        (2, "narrow", "42", "26", b"\x1b[<0;60;18M".as_slice()),
        (3, "short", "115", "12", b"x".as_slice()),
    ] {
        reset_queries();
        let mut claude = ReleaseGate::new("release-claude");
        let mut codex = ReleaseGate::new("release-codex");
        let mut popup = server.command();
        popup
            .args([
                "display-popup",
                "-c",
                &target,
                "-E",
                "-b",
                "rounded",
                "-T",
                " Agent usage ",
                "-w",
                width,
                "-h",
                height,
            ])
            .arg(&wrapper)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped());
        let mut opened = Popup(popup.spawn().unwrap());
        client.until("both columns load independently", |screen| {
            screen.contents().matches("checking…").count() == 2
        });
        assert!(
            opened.0.try_wait().unwrap().is_none(),
            "popup exited during loading"
        );
        assert!(client.parser.screen().hide_cursor());
        client.until("both reset helpers are ready for release", |_| {
            root().join("curl-ready").exists() && root().join("codex-ready").exists()
        });
        assert_eq!(
            client
                .parser
                .screen()
                .contents()
                .matches("checking…")
                .count(),
            2
        );
        claude.release();
        client.until("Claude finishes before Codex", |screen| {
            let text = screen.contents();
            text.contains("none") && text.matches("checking…").count() == 1
        });
        codex.release();
        client.until("both columns finish", |screen| {
            let text = screen.contents();
            text.matches("Saved resets").count() == 2
                && !text.contains("checking…")
                && text.contains("close")
        });
        let text = client.parser.screen().contents();
        for value in [
            "Claude Code",
            "Codex",
            "7%",
            "87%",
            "95%",
            "20%",
            "Saved resets",
            "12.50",
            "close",
        ] {
            assert!(text.contains(value), "{name}: {text}");
        }
        assert!(text.contains("Agent usage"));
        assert!(!text.contains("couldn't check") && !text.contains("unavailable"));
        // The unstyled title inherits the configured tmux border color.
        let title = (0..36)
            .find_map(|row| {
                let text = client
                    .parser
                    .screen()
                    .rows(0, 120)
                    .nth(row as usize)
                    .unwrap();
                text.find("Agent usage").map(|col| (row, col as u16))
            })
            .unwrap();
        assert_eq!(
            client
                .parser
                .screen()
                .cell(title.0, title.1)
                .unwrap()
                .fgcolor(),
            vt100::Color::Idx(5)
        );
        client.capture(name);
        if index == 0 {
            for event in [
                b"\x1b[<64;60;18M".as_slice(),
                b"\x1b[<32;60;18M".as_slice(),
                b"\x1b[<0;60;18m".as_slice(),
            ] {
                client.send(event);
                let deadline = Instant::now() + Duration::from_millis(80);
                while Instant::now() < deadline {
                    client.pump();
                }
                assert!(
                    opened.0.try_wait().unwrap().is_none(),
                    "wheel/motion/release closed popup"
                );
                assert!(client.parser.screen().contents().contains("Agent usage"));
            }
        }
        close(&mut client, input);
        assert!(opened.0.wait().unwrap().success());
    }
    server.run(&["detach-client", "-t", &target]);
    assert!(client.child.wait().unwrap().success());
}
