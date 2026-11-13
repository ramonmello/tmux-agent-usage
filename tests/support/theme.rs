//! Consumer integration: a themed tmux config built from the shipped fixtures.
use crate::cli::Request;
use crate::clock::FixedClock;
use crate::contract_tests::{executable, root, seed};
use crate::live_popup_tests::{Client, Server};
use crate::test_support::{Sandbox, success};
use serde_json::Value;
use std::fs::{self, File, FileTimes};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant, UNIX_EPOCH};

const TEST: &str = "theme_tests::theme_preserves_goldens_selection_mouse_and_bindings";
const EXTERNAL_TEST: &str =
    "theme_tests::external_theme_preserves_goldens_selection_mouse_and_bindings";
const NOW: i64 = 1_794_484_800;
const THEME: &str = include_str!("../fixtures/consumer/theme.conf");
const CONFIG: &str = include_str!("../fixtures/consumer/tmux.conf");
const EXTERNAL_CONFIG: &str = include_str!("../fixtures/consumer/tmux.external.conf");
const REMAP: &str = include_str!("../fixtures/consumer/tmux.remap.conf");
const BASELINE: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/goldens/baseline");

fn consumer_sources() -> (String, String, String) {
    let external = std::env::var("AGENT_USAGE_THEME_EXTERNAL").as_deref() == Ok("1");
    (
        THEME.to_owned(),
        if external { EXTERNAL_CONFIG } else { CONFIG }.to_owned(),
        REMAP.to_owned(),
    )
}

struct Tee(File);
impl Write for Tee {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.0.write_all(bytes)?;
        io::stdout().write_all(bytes)?;
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        self.0.flush()?;
        io::stdout().flush()
    }
}

// The shim changes only the clock and fixture input at the rendering boundary.
// Production has no environment/CLI clock override. Popup/details still use the
// real dispatcher, provider loaders, terminal guard and private tmux server.
#[test]
fn command_child() {
    let Some(prefix) = std::env::var_os("AGENT_USAGE_THEME_COMMAND") else {
        return;
    };
    let prefix = PathBuf::from(prefix);
    let args: Vec<String> = fs::read_to_string(prefix.with_extension("args"))
        .unwrap()
        .lines()
        .map(str::to_owned)
        .collect();
    if std::env::var("AGENT_USAGE_THEME_TMUX").as_deref() == Ok("1") {
        let mut args = args;
        assert_eq!(args[0], "display-popup");
        let program = crate::shell_quote(std::env::current_exe().unwrap().to_str().unwrap());
        let child = args.last_mut().unwrap();
        *child = child.replacen(
            &program,
            &crate::shell_quote(root().join("fakes/tmux-agent-usage").to_str().unwrap()),
            1,
        );
        fs::write(
            root().join("display-popup.json"),
            serde_json::to_vec(&args).unwrap(),
        )
        .unwrap();
        let status = Command::new(std::env::var_os("AGENT_USAGE_REAL_TMUX").unwrap())
            .arg("-S")
            .arg(root().join("theme-socket"))
            .args(args)
            .status()
            .unwrap();
        std::process::exit(status.code().unwrap_or(1));
    }
    let request = crate::cli::parse(&args).unwrap();
    let code = match request {
        Request::Status(agent, styles) => {
            let fixture: Value =
                serde_json::from_str(include_str!("../fixtures/baseline/status.json")).unwrap();
            let case = fs::read_to_string(root().join("status-case")).unwrap();
            let case = fixture["cases"]
                .as_array()
                .unwrap()
                .iter()
                .find(|entry| entry["name"] == case)
                .unwrap();
            let gauges = case[agent.name()].as_array().map(|windows| {
                windows
                    .iter()
                    .map(|window| {
                        (!window.is_null()).then(|| crate::render::Gauge {
                            used_percent: window["used_percent"].as_f64().unwrap(),
                            resets_at: (NOW + window["remaining_secs"].as_i64().unwrap()) as f64,
                            window_secs: window["window_secs"].as_i64().unwrap(),
                        })
                    })
                    .collect::<Vec<_>>()
            });
            fs::write(
                prefix.with_extension("out"),
                crate::render::body(gauges.as_deref(), &styles, NOW),
            )
            .unwrap();
            0
        }
        Request::Details { .. } => {
            let mut output = Tee(File::create(root().join("details.ansi")).unwrap());
            crate::execute(request, &FixedClock(NOW), &mut output).unwrap()
        }
        Request::Popup { .. } => {
            fs::write(
                root().join("popup.json"),
                serde_json::to_vec(&args).unwrap(),
            )
            .unwrap();
            crate::execute(request, &FixedClock(NOW), &mut io::stdout()).unwrap()
        }
        _ => panic!("unexpected theme command: {args:?}"),
    };
    std::process::exit(code.into());
}

fn shim(tmux: bool) -> String {
    let program = crate::shell_quote(std::env::current_exe().unwrap().to_str().unwrap());
    format!(
        r##"#!/bin/sh
prefix="$HOME/command-$$"
printf '%s\n' "$@" > "$prefix.args"
export AGENT_USAGE_THEME_COMMAND="$prefix"
export AGENT_USAGE_THEME_TMUX={}
if [ "$1" = status ]; then
  {program} --exact theme_tests::command_child --nocapture > /dev/null
  result=$?
  [ ! -f "$prefix.out" ] || /bin/cat "$prefix.out"
  exit "$result"
fi
if [ "$1" = details ]; then
  exec {program} --exact theme_tests::command_child --nocapture 2>> "$HOME/command-errors"
fi
exec {program} --exact theme_tests::command_child --nocapture > /dev/null 2>> "$HOME/command-errors"
"##,
        if tmux { "1" } else { "0" }
    )
}

fn isolated(test: &str, external: bool) -> bool {
    if std::env::var("AGENT_USAGE_THEME_CHILD").as_deref() == Ok(test) {
        return true;
    }
    let program = std::env::split_paths(&std::env::var_os("PATH").unwrap())
        .map(|path| path.join("tmux"))
        .find(|path| path.is_file())
        .expect("tmux is required for the isolated theme test");
    let sandbox = Sandbox::new();
    success(
        sandbox
            .command(std::env::current_exe().unwrap())
            .args(["--exact", test, "--nocapture"])
            .env("AGENT_USAGE_THEME_CHILD", test)
            .env(
                "AGENT_USAGE_THEME_EXTERNAL",
                if external { "1" } else { "0" },
            )
            .env("AGENT_USAGE_REAL_TMUX", &program)
            .env_remove("CLAUDE_CONFIG_DIR")
            .env_remove("CLAUDE_SECURESTORAGE_CONFIG_DIR")
            .env(
                "PATH",
                format!("{}:/usr/bin:/bin", sandbox.root.join("fakes").display()),
            )
            .output()
            .unwrap(),
    );
    false
}

// display-message intentionally disables async #() jobs. Execute each exact
// command from the loaded theme, then let tmux expand the composed format.
// The attached client separately verifies the real async jobs on screen.
fn expanded(server: &Server, option: &str) -> String {
    let mut format = server
        .run(&[
            "show-option",
            "-gv",
            if option == "@theme-agent-usage" {
                "status-right"
            } else {
                option
            },
        ])
        .trim_end_matches('\n')
        .to_owned();
    if option == "@theme-agent-usage" {
        format = format
            .split(" #[fg=#3c3836,bg=default]")
            .next()
            .unwrap()
            .to_owned();
    }
    while let Some(start) = format.find("#(") {
        let mut depth = 1;
        let end = format[start + 2..]
            .char_indices()
            .find_map(|(i, ch)| {
                match ch {
                    '(' => depth += 1,
                    ')' => depth -= 1,
                    _ => {}
                }
                (depth == 0).then_some(start + 2 + i)
            })
            .unwrap();
        let command = server.run(&["display-message", "-p", &format[start + 2..end]]);
        let output = success(
            Command::new("/bin/sh")
                .args(["-c", command.trim_end_matches('\n')])
                .output()
                .unwrap(),
        );
        assert!(
            output.stderr.is_empty(),
            "status command: {:?}",
            output.stderr
        );
        // display-message runs strftime; job output in a real status redraw is
        // inserted after that pass, so quote literal percentage signs here.
        let body = std::str::from_utf8(&output.stdout)
            .unwrap()
            .replace('%', "%%");
        format.replace_range(start..=end, &body);
    }
    server
        .run(&["display-message", "-p", &format])
        .trim_end_matches('\n')
        .to_owned()
}

fn assert_expanded(server: &Server, expected: &str) {
    assert_eq!(
        expanded(server, "@theme-agent-usage").as_bytes(),
        expected.as_bytes()
    );
}

fn plain(format: &str) -> String {
    let mut text = String::new();
    let mut rest = format;
    while let Some(start) = rest.find("#[") {
        text.push_str(&rest[..start]);
        rest = &rest[start + 2..];
        rest = &rest[rest.find(']').unwrap() + 1..];
    }
    text.push_str(rest);
    text
}

fn columns(client: &Client, needle: &str) -> u16 {
    (0..160)
        .find(|col| {
            client
                .parser
                .screen()
                .rows(*col, 160 - col)
                .next()
                .unwrap()
                .starts_with(needle)
        })
        .unwrap_or_else(|| {
            panic!(
                "{needle:?} not in status row: {:?}",
                client.parser.screen().rows(0, 160).next().unwrap()
            )
        })
        + 1
}

fn settle_clicks(client: &mut Client) {
    // tmux distinguishes double/triple clicks from MouseDown1Status. Exercise
    // deliberate single clicks without changing the consumer's key bindings.
    let deadline = Instant::now() + Duration::from_millis(600);
    while Instant::now() < deadline {
        client.pump();
    }
}

fn click(client: &mut Client, col: u16) {
    settle_clicks(client);
    client.send(format!("\x1b[<0;{col};1M\x1b[<0;{col};1m").as_bytes());
}

fn open_and_close(server: &Server, client: &mut Client, input: &[u8], agents: &str) {
    let _ = fs::remove_file(root().join("popup.json"));
    let _ = fs::remove_file(root().join("display-popup.json"));
    if input.starts_with(b"\x1b[<") {
        settle_clicks(client);
    }
    client.send(input);
    let opened = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        client.until("theme binding opens the real popup", |screen| {
            let text = screen.contents();
            text.contains("Agent usage")
                && text.contains("Saved resets")
                && !text.contains("checking…")
        })
    }));
    if let Err(error) = opened {
        eprintln!(
            "popup argv: {:?}",
            fs::read_to_string(root().join("popup.json"))
        );
        eprintln!(
            "shim diagnostics: {}",
            fs::read_to_string(root().join("command-errors")).unwrap_or_default()
        );
        std::panic::resume_unwind(error);
    }
    let args: Vec<String> =
        serde_json::from_slice(&fs::read(root().join("popup.json")).unwrap()).unwrap();
    let request = crate::cli::parse(&args).unwrap();
    let Request::Popup { options, .. } = request else {
        unreachable!()
    };
    assert_eq!(
        options
            .agents
            .iter()
            .map(|agent| agent.name())
            .collect::<Vec<_>>()
            .join(" "),
        agents
    );
    assert!(options.claude_resets);
    let envelope: Vec<String> =
        serde_json::from_slice(&fs::read(root().join("display-popup.json")).unwrap()).unwrap();
    assert!(envelope.windows(2).any(|pair| pair == ["-S", "fg=#665c54"]));
    assert!(
        envelope
            .windows(2)
            .any(|pair| pair == ["-T", "#[fg=#fbf1c7] Agent usage "])
    );
    assert!(envelope.windows(2).any(|pair| pair == ["-b", "rounded"]));
    if agents == "claude codex" {
        crate::baseline_goldens::check_popup_goldens(options.theme);
    }
    client.send(b"q");
    client.until("popup closes", |screen| {
        screen.contents().contains("ORIGINAL PANE") && !screen.contents().contains("Agent usage")
    });
    // The real details stream, before tmux terminal encoding, must match the
    // complete baseline frame for these synthetic local values/zero resets.
    if agents == "claude codex" {
        let deadline = Instant::now() + crate::test_support::EVENT_TIMEOUT;
        loop {
            let stream = fs::read_to_string(root().join("details.ansi")).unwrap();
            if let Some(stream) = stream.strip_suffix(crate::terminal::RESTORE) {
                let frame = stream.rsplit("\x1b[H\x1b[2J").next().unwrap();
                assert_eq!(
                    frame.as_bytes(),
                    fs::read(Path::new(BASELINE).join("popup-none-columns.ansi")).unwrap()
                );
                break;
            }
            assert!(Instant::now() < deadline, "details did not finish cleanup");
            client.pump();
        }
    }
    assert_eq!(server.run(&["show-option", "-gv", "prefix"]).trim(), "C-a");
}

// The same consumer flow, loaded directly and through TPM as an external
// executable. Two tests so the two loading modes can run in parallel.
#[test]
fn theme_preserves_goldens_selection_mouse_and_bindings() {
    if isolated(TEST, false) {
        theme_flow();
    }
}

#[test]
fn external_theme_preserves_goldens_selection_mouse_and_bindings() {
    if isolated(EXTERNAL_TEST, true) {
        theme_flow();
    }
}

fn theme_flow() {
    // Reuse the synthetic reset clients/credentials; no active client runs.
    seed();
    executable("tmux-agent-usage", &shim(false));
    executable("tmux", &shim(true).replace("prefix=\"$HOME/command-$$\"", "if [ \"$1\" = -S ]; then exec \"$AGENT_USAGE_REAL_TMUX\" \"$@\"; fi\nprefix=\"$HOME/command-$$\""));
    executable("security", "#!/bin/sh\nexit 44\n");
    executable("claude", "#!/bin/sh\nprintf '2.1.0 (Claude Code)\\n'\n");
    executable(
        "curl",
        "#!/bin/sh\n/bin/cat > /dev/null\nprintf 'HTTP/1.1 200 OK\\r\\n\\r\\n{\"cedar_ember\":{\"eligible\":true,\"grants\":[]}}\\n200'\n",
    );
    executable(
        "codex",
        "#!/bin/sh\nIFS= read -r initialize\nprintf '{\"id\":1,\"result\":{}}\\n'\nIFS= read -r initialized\nIFS= read -r request\n/bin/cat \"$HOME/codex-response\"\n",
    );
    let fixture: Value =
        serde_json::from_str(include_str!("../fixtures/baseline/popup.json")).unwrap();
    fs::write(root().join("cache/tmux-agent-usage/claude/fixture.json"), serde_json::to_vec(&serde_json::json!({"observed_at": fixture["claude_observed_at"], "five_hour": fixture["claude"]["five_hour"], "seven_day": fixture["claude"]["seven_day"]})).unwrap()).unwrap();
    let rollout = root().join("codex/sessions/2026/11/12/rollout-fixture.jsonl");
    fs::write(&rollout, serde_json::to_vec(&serde_json::json!({"payload":{"rate_limits":{"primary":{"used_percent":95,"window_minutes":300,"resets_at":NOW+4800},"secondary":{"used_percent":20,"window_minutes":10080,"resets_at":NOW+172800},"plan_type":"pro","credits":fixture["credits"]}}})).unwrap()).unwrap();
    File::options()
        .write(true)
        .open(rollout)
        .unwrap()
        .set_times(
            FileTimes::new().set_modified(UNIX_EPOCH + Duration::from_secs((NOW - 90) as u64)),
        )
        .unwrap();
    fs::write(root().join("status-case"), "normal").unwrap();
    let (theme_source, config_source, remap_source) = consumer_sources();
    let theme = root().join("theme.conf");
    fs::write(&theme, theme_source).unwrap();
    let external = std::env::var("AGENT_USAGE_THEME_EXTERNAL").as_deref() == Ok("1");
    let plugin = root().join(if external {
        "plugins/tmux-agent-usage"
    } else {
        "consumer plugin"
    });
    for file in [
        "agent-usage.tmux",
        "Cargo.toml",
        "scripts/tmux-load.sh",
        "scripts/install-release.sh",
        "scripts/plugin-helpers.sh",
    ] {
        let destination = plugin.join(file);
        fs::create_dir_all(destination.parent().unwrap()).unwrap();
        fs::copy(
            Path::new(env!("CARGO_MANIFEST_DIR")).join(file),
            destination,
        )
        .unwrap();
    }
    // Replace TPM's dispatch only: no clone, updater, network or active server.
    // The same plugin entrypoint, external executable and full visual/input
    // assertions run through the migrated loading location.
    let tpm = root().join("plugins/tpm/tpm");
    if external {
        fs::create_dir_all(tpm.parent().unwrap()).unwrap();
        fs::write(
            &tpm,
            format!(
                "#!/bin/sh\nexec {}\n",
                crate::shell_quote(plugin.join("agent-usage.tmux").to_str().unwrap())
            ),
        )
        .unwrap();
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&tpm, fs::Permissions::from_mode(0o755)).unwrap();
    }
    let remap = root().join("tmux.remap.conf");
    fs::write(&remap, remap_source).unwrap();
    let config = config_source
        .replace(
            "source-file ~/.config/tmux/tmux.remap.conf",
            &format!(
                "source-file {}",
                crate::shell_quote(remap.to_str().unwrap())
            ),
        )
        .replace(
            "source-file ~/.config/tmux/theme.conf",
            &format!(
                "source-file {}",
                crate::shell_quote(theme.to_str().unwrap())
            ),
        )
        .replace(
            "run '~/.config/tmux/plugins/tpm/tpm'",
            &if external {
                format!(
                    "run {}",
                    crate::shell_quote(&crate::shell_quote(tpm.to_str().unwrap()))
                )
            } else {
                "# TPM is not started in an isolated consumer test.".to_owned()
            },
        )
        .replace(
            "run '~/.config/tmux/agent-usage/agent-usage.tmux'",
            &format!(
                "run {}",
                crate::shell_quote(&crate::shell_quote(
                    plugin.join("agent-usage.tmux").to_str().unwrap()
                ))
            ),
        );
    let config_file = root().join("tmux.conf");
    fs::write(&config_file, config).unwrap();
    let mut server = Server {
        program: std::env::var_os("AGENT_USAGE_REAL_TMUX").unwrap().into(),
        socket: root().join("theme-socket"),
        started: false,
    };
    server.run(&[
        "new-session",
        "-d",
        "-s",
        "theme",
        "-x",
        "160",
        "-y",
        "60",
        "printf 'ORIGINAL PANE\\n'; /bin/cat",
    ]);
    server.started = true;
    let unrelated = server.run(&["list-keys"]);
    server.run(&["source-file", config_file.to_str().unwrap()]);
    let unrelated_key = |bindings: &str| {
        bindings
            .lines()
            .find(|line| {
                line.split_whitespace()
                    .collect::<Vec<_>>()
                    .windows(3)
                    .any(|words| words == ["-T", "prefix", "?"])
            })
            .unwrap()
            .to_owned()
    };
    assert_eq!(
        unrelated_key(&server.run(&["list-keys"])),
        unrelated_key(&unrelated)
    );
    // Disable unrelated async window-label lookup; it is outside this item.
    server.run(&[
        "set-option",
        "-g",
        "window-status-current-format",
        " #I:#W ",
    ]);
    server.run(&["set-option", "-g", "window-status-format", " #I:#W "]);
    server.run(&["set-option", "-g", "status-interval", "1"]);
    let mut client = Client::attach_to(&server, "theme", 60, 160);
    client.until("theme status appears", |screen| {
        screen.contents().contains("7%")
    });
    let left = server.run(&["show-option", "-gv", "status-left"]);
    let right = server.run(&["show-option", "-gv", "status-right"]);
    for name in ["simple", "gruvbox"] {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join(format!("examples/{name}.conf"));
        server.run(&["source-file", path.to_str().unwrap()]);
        assert_eq!(server.run(&["show-option", "-gv", "status-left"]), left);
        assert_eq!(server.run(&["show-option", "-gv", "status-right"]), right);
        let body = expanded(&server, &format!("@example-agent-usage-{name}"));
        if name == "simple" {
            assert_eq!(body, "Claude 7% · 2% | Codex 28% · 4%");
            assert!(!body.contains("#["));
            server.run(&[
                "set-option",
                "-g",
                "status-right",
                "#{E:@example-agent-usage-simple}",
            ]);
            client.until("plain example renders through real tmux jobs", |screen| {
                screen.rows(0, 160).next().unwrap().contains(&body)
            });
            server.run(&[
                "set-option",
                "-g",
                "status-right",
                right.trim_end_matches('\n'),
            ]);
            assert!(
                !body
                    .chars()
                    .any(|ch| ('\u{e000}'..='\u{f8ff}').contains(&ch)
                        || ('\u{f0000}'..='\u{10ffff}').contains(&ch))
            );
        } else {
            assert!(
                body.contains("CC ") && body.contains("CX ") && body.contains("#[fg=#fbf1c7]7%")
            );
            assert!(body.contains('\u{e0b6}') && body.contains('\u{e0b4}'));
        }
    }
    let fixture: Value =
        serde_json::from_str(include_str!("../fixtures/baseline/status.json")).unwrap();
    for case in fixture["cases"].as_array().unwrap() {
        let name = case["name"].as_str().unwrap();
        fs::write(root().join("status-case"), name).unwrap();
        let expected =
            fs::read_to_string(Path::new(BASELINE).join(format!("status-{name}.tmux"))).unwrap();
        assert_expanded(&server, &expected);
        if matches!(name, "normal" | "reset-soon" | "missing")
            && let Some(dir) = std::env::var_os("AGENT_USAGE_CAPTURE_DIR")
        {
            fs::create_dir_all(&dir).unwrap();
            fs::write(
                Path::new(&dir).join(format!("simple-{name}.txt")),
                expanded(&server, "@example-agent-usage-simple"),
            )
            .unwrap();
        }
        let text = plain(&expected);
        client.until("real async status jobs match the fixture", |screen| {
            screen.rows(0, 160).next().unwrap().contains(&text)
        });
    }
    fs::write(root().join("status-case"), "normal").unwrap();
    let normal = fs::read_to_string(Path::new(BASELINE).join("status-normal.tmux")).unwrap();
    assert_expanded(&server, &normal);
    let pills: Vec<&str> = normal.split("#[range=right] ").collect();
    let claude = format!("{}#[range=right]", pills[0]);
    let codex = pills[1];
    let loaded_pills: Vec<&str> = right
        .trim_end_matches('\n')
        .splitn(2, "#[range=right] ")
        .collect();
    let claude_format = format!("{}#[range=right]", loaded_pills[0]);
    let (codex_format, date_format) = loaded_pills[1].split_once("#[range=right]").unwrap();
    let codex_format = format!("{codex_format}#[range=right]");
    for (agents, format, expected) in [
        ("claude", claude_format.clone(), claude.clone()),
        ("codex", codex_format.clone(), codex.to_owned()),
        (
            "codex claude",
            format!("{codex_format} {claude_format}"),
            format!("{codex} {claude}"),
        ),
        (
            "claude codex",
            format!("{claude_format} {codex_format}"),
            normal.clone(),
        ),
    ] {
        server.run(&[
            "set-option",
            "-g",
            "status-right",
            &format!("{format}{date_format}"),
        ]);
        server.run(&["set-option", "-g", "@agent-usage-agents", agents]);
        server.run(&[
            "run-shell",
            &crate::shell_quote(plugin.join("agent-usage.tmux").to_str().unwrap()),
        ]);
        assert_expanded(&server, &expected);
        let text = plain(&expected);
        client.until(
            "selected status pills have no omitted-agent residue",
            |screen| {
                let row = screen.rows(0, 160).next().unwrap();
                row.contains(&text)
                    && (agents.contains("claude") || !row.contains('\u{100006}'))
                    && (agents.contains("codex") || !row.contains('\u{100007}'))
            },
        );
        open_and_close(&server, &mut client, b"\x01A", agents);
    }
    client.until("normal pills return", |screen| {
        let row = screen.rows(0, 160).next().unwrap();
        row.contains("7%") && row.contains("28%")
    });
    for logo in ["\u{100006}", "\u{100007}"] {
        let col = columns(&client, logo);
        open_and_close(
            &server,
            &mut client,
            format!("\x1b[<0;{col};1M\x1b[<0;{col};1m").as_bytes(),
            "claude codex",
        );
    }
    let popup_before = fs::read(root().join("popup.json")).unwrap();
    let date_text = server.run(&["display-message", "-p", "%a %d %b"]);
    let date = columns(&client, date_text.trim());
    click(&mut client, date);
    // Status clicks outside user ranges must retain switch-client -t =.
    server.run(&["new-window", "-d", "-t", "theme", "-n", "other", "/bin/cat"]);
    client.until("second window appears", |screen| {
        screen.rows(0, 160).next().unwrap().contains("other")
    });
    let other = columns(&client, "other");
    click(&mut client, other);
    client.until("window click keeps default action", |screen| {
        !screen.contents().contains("ORIGINAL PANE")
    });
    assert_eq!(
        server
            .run(&["display-message", "-p", "#{window_name}"])
            .trim(),
        "other"
    );
    assert_eq!(fs::read(root().join("popup.json")).unwrap(), popup_before);
    client.send(b"\x01H");
    client.until(
        "unrelated prefix H still selects the previous window",
        |screen| screen.contents().contains("ORIGINAL PANE"),
    );
    assert_eq!(
        server
            .run(&["show-option", "-gv", "@agent-usage-agents"])
            .trim(),
        "claude codex"
    );
    server.run(&["detach-client"]);
    assert!(client.child.wait().unwrap().success());
}
