#[path = "support/documentation.rs"]
mod documentation;
mod support;
use support as test_support;
#[allow(dead_code)]
#[path = "support/tmux.rs"]
mod tmux;

use std::collections::BTreeMap;
use std::fs;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;
use support::{EVENT_TIMEOUT, Sandbox, success, wait_for};
use tmux::{Client, Server};

const SOURCE: &str = env!("CARGO_MANIFEST_DIR");
const BINARY: &str = env!("CARGO_BIN_EXE_tmux-agent-usage");

fn executable(path: &Path, bytes: impl AsRef<[u8]>) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, bytes).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
}

fn tool(name: &str) -> PathBuf {
    std::env::split_paths(&std::env::var_os("PATH").unwrap())
        .map(|dir| dir.join(name))
        .find(|path| path.is_file())
        .unwrap_or_else(|| panic!("{name} is required for TPM tests"))
}

fn isolated(name: &str) -> bool {
    if std::env::var("AGENT_USAGE_TPM_CHILD").as_deref() == Ok(name) {
        return true;
    }
    let sandbox = Sandbox::new();
    success(
        sandbox
            .command(std::env::current_exe().unwrap())
            .args(["--exact", name, "--nocapture"])
            .env("AGENT_USAGE_TPM_CHILD", name)
            .env("AGENT_USAGE_REAL_TMUX", tool("tmux"))
            .env("AGENT_USAGE_REAL_CURL", tool("curl"))
            .env(
                "PATH",
                format!("{}:/usr/bin:/bin", sandbox.root.join("fakes").display()),
            )
            .output()
            .unwrap(),
    );
    false
}

fn root() -> PathBuf {
    std::env::var_os("HOME").unwrap().into()
}

// A real loopback HTTP server with controlled bytes, failures and a held first
// request. Only the transport is redirected; the installer still chooses the
// exact versioned GitHub URL, curl limits, archive and checksum itself.
struct Releases {
    url: String,
    files: Arc<Mutex<BTreeMap<String, Vec<u8>>>>,
    requests: Arc<Mutex<Vec<String>>>,
    hold: Arc<AtomicBool>,
    stop: Arc<AtomicBool>,
    worker: Option<thread::JoinHandle<()>>,
}

impl Releases {
    fn new() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let files = Arc::new(Mutex::new(BTreeMap::<String, Vec<u8>>::new()));
        let requests = Arc::new(Mutex::new(Vec::new()));
        let hold = Arc::new(AtomicBool::new(false));
        let stop = Arc::new(AtomicBool::new(false));
        let (thread_files, thread_requests, thread_hold, thread_stop) =
            (files.clone(), requests.clone(), hold.clone(), stop.clone());
        let worker = thread::spawn(move || {
            while !thread_stop.load(Ordering::SeqCst) {
                let (mut stream, _) = match listener.accept() {
                    Ok(connection) => connection,
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(5));
                        continue;
                    }
                    Err(error) => panic!("fake release server: {error}"),
                };
                stream.set_nonblocking(false).unwrap();
                stream.set_read_timeout(Some(EVENT_TIMEOUT)).unwrap();
                let mut request = Vec::new();
                let mut byte = [0];
                while !request.ends_with(b"\r\n\r\n") {
                    assert_eq!(stream.read(&mut byte).unwrap(), 1);
                    request.push(byte[0]);
                }
                let request = String::from_utf8(request).unwrap();
                let path = request.split_whitespace().nth(1).unwrap().to_owned();
                thread_requests.lock().unwrap().push(path.clone());
                while thread_hold.load(Ordering::SeqCst) && !thread_stop.load(Ordering::SeqCst) {
                    thread::sleep(Duration::from_millis(5));
                }
                let body = thread_files.lock().unwrap().get(&path).cloned();
                let status = if body.is_some() {
                    "200 OK"
                } else {
                    "404 Not Found"
                };
                let body = body.unwrap_or_default();
                let header = format!(
                    "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len()
                );
                // Failure tests can terminate curl early; a broken pipe is fine.
                let _ = stream
                    .write_all(header.as_bytes())
                    .and_then(|()| stream.write_all(&body));
            }
        });
        Self {
            url,
            files,
            requests,
            hold,
            stop,
            worker: Some(worker),
        }
    }

    fn asset(&self, version: &str, target: &str, bytes: &[u8], checksum: bool) {
        let name = format!("tmux-agent-usage-v{version}-{target}.tar.gz");
        let path = format!("/v{version}/{name}");
        let digest = if checksum {
            sha256(bytes)
        } else {
            "0".repeat(64)
        };
        let mut files = self.files.lock().unwrap();
        files.insert(path.clone(), bytes.to_vec());
        files.insert(
            format!("{path}.sha256"),
            format!("{digest}  {name}\n").into_bytes(),
        );
    }

    fn count(&self) -> usize {
        self.requests.lock().unwrap().len()
    }
}

impl Drop for Releases {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        self.worker.take().unwrap().join().unwrap();
    }
}

fn sha256(bytes: &[u8]) -> String {
    let path = root().join("digest-input");
    fs::write(&path, bytes).unwrap();
    let output = success(
        Command::new("shasum")
            .args(["-a", "256"])
            .arg(path)
            .output()
            .unwrap(),
    );
    String::from_utf8(output.stdout)
        .unwrap()
        .split_whitespace()
        .next()
        .unwrap()
        .to_owned()
}

fn archive(bytes: &[u8]) -> Vec<u8> {
    let dir = root().join("archive-input");
    fs::create_dir_all(&dir).unwrap();
    executable(&dir.join("tmux-agent-usage"), bytes);
    success(
        Command::new("tar")
            .args(["-czf", "-"])
            .arg("-C")
            .arg(dir)
            .arg("tmux-agent-usage")
            .output()
            .unwrap(),
    )
    .stdout
}

fn native_target() -> &'static str {
    match (std::env::consts::OS, std::env::consts::ARCH) {
        ("macos", "aarch64") => "aarch64-apple-darwin",
        ("macos", "x86_64") => "x86_64-apple-darwin",
        ("linux", "aarch64") => "aarch64-unknown-linux-musl",
        ("linux", "x86_64") => "x86_64-unknown-linux-musl",
        platform => panic!("native TPM fixture needs a supported target: {platform:?}"),
    }
}

struct Plugin {
    dir: PathBuf,
}

struct Loader(Option<Child>);
impl Loader {
    fn finished(&mut self) -> bool {
        self.0.as_mut().unwrap().try_wait().unwrap().is_some()
    }

    fn output(mut self) -> Output {
        wait_for("plugin loader exits", || self.finished());
        self.0.take().unwrap().wait_with_output().unwrap()
    }
}
impl Drop for Loader {
    fn drop(&mut self) {
        if let Some(child) = &mut self.0 {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

impl Plugin {
    fn new(name: &str, releases: &Releases) -> Self {
        let dir = root().join(name);
        for file in [
            "agent-usage.tmux",
            "scripts/tmux-load.sh",
            "scripts/install-release.sh",
            "scripts/plugin-helpers.sh",
        ] {
            executable(
                &dir.join(file),
                fs::read(Path::new(SOURCE).join(file)).unwrap(),
            );
        }
        fs::copy(Path::new(SOURCE).join("Cargo.toml"), dir.join("Cargo.toml")).unwrap();
        executable(
            &root().join("fakes/curl"),
            format!(
                r#"#!/bin/sh
[ "$#" -eq 11 ] || exit 81
[ "$1 $2 $3 $4 $5 $6 $7 $8 $9" = '--fail --location --silent --show-error --connect-timeout 5 --max-time 30 --output' ] || exit 82
url=${{11}}
case "$url" in
  https://github.com/ramonmello/tmux-agent-usage/releases/download/v*) ;;
  *) exit 83 ;;
esac
path=${{url#https://github.com/ramonmello/tmux-agent-usage/releases/download}}
exec "$AGENT_USAGE_REAL_CURL" "$1" "$2" "$3" "$4" "$5" "$6" "$7" "$8" "$9" "${{10}}" '{}'$path
"#,
                releases.url
            ),
        );
        executable(
            &root().join("fakes/tmux"),
            r#"#!/bin/sh
if [ "$1" = -S ]; then exec "$AGENT_USAGE_REAL_TMUX" "$@"; fi
exec "$AGENT_USAGE_REAL_TMUX" -S "$HOME/socket" "$@"
"#,
        );
        for name in ["codex", "claude", "security", "cargo"] {
            executable(&root().join("fakes").join(name), b"#!/bin/sh\nprintf 'unexpected provider/build call' >> \"$HOME/forbidden-calls\"\nexit 1\n");
        }
        Self { dir }
    }

    fn installer(&self) -> Command {
        let mut command = Command::new(self.dir.join("scripts/install-release.sh"));
        command.arg(&self.dir);
        command
    }

    fn installed(&self, version: &str) -> PathBuf {
        self.dir.join(format!(
            "bin/v{version}/{}/tmux-agent-usage",
            native_target()
        ))
    }

    fn stable(&self) -> PathBuf {
        self.dir.join("bin/tmux-agent-usage")
    }

    fn relative(&self, version: &str) -> PathBuf {
        PathBuf::from(format!("v{version}/{}/tmux-agent-usage", native_target()))
    }

    fn current(&self) -> Option<PathBuf> {
        fs::read_link(self.stable()).ok()
    }

    fn start_load(&self, server: &Server) -> Loader {
        let log = root().join("load-errors");
        let status = root().join("load-status");
        let child = server
            .command()
            .args([
                "run-shell",
                &format!(
                    "{} 2>{}; printf '%s' $? >{}",
                    shell_word(self.dir.join("agent-usage.tmux").to_str().unwrap()),
                    shell_word(log.to_str().unwrap()),
                    shell_word(status.to_str().unwrap())
                ),
            ])
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        Loader(Some(child))
    }

    fn finish_load(&self, child: Loader) -> String {
        let result = success(child.output());
        assert_eq!(
            fs::read_to_string(root().join("load-status")).unwrap(),
            "0",
            "loader: {}",
            fs::read_to_string(root().join("load-errors")).unwrap()
        );
        String::from_utf8(result.stdout).unwrap()
    }

    fn load(&self, server: &Server) -> String {
        self.finish_load(self.start_load(server))
    }
}

fn shell_word(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

fn server() -> Server {
    let mut server = Server {
        program: std::env::var_os("AGENT_USAGE_REAL_TMUX").unwrap().into(),
        socket: root().join("socket"),
        started: false,
    };
    server.run(&[
        "new-session",
        "-d",
        "-s",
        "plugin",
        "-x",
        "160",
        "-y",
        "50",
        "printf 'ORIGINAL PANE\\n'; /bin/cat",
    ]);
    server.started = true;
    server.run(&["set-option", "-g", "status-interval", "1"]);
    server.run(&["set-option", "-g", "status-left-length", "100"]);
    server.run(&["set-option", "-g", "status-right-length", "100"]);
    server
}

fn option(server: &Server, name: &str) -> String {
    server
        .run(&["show-option", "-gqv", name])
        .trim_end_matches('\n')
        .to_owned()
}

fn binding(server: &Server, table: &str, key: &str) -> String {
    server
        .run(&["list-keys"])
        .lines()
        .filter(|line| {
            let words: Vec<_> = line.split_whitespace().collect();
            words.windows(3).any(|words| words == ["-T", table, key])
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn argv(path: &str) -> Vec<String> {
    fs::read(root().join(path))
        .unwrap()
        .split(|byte| *byte == 0)
        .filter(|word| !word.is_empty())
        .map(|word| String::from_utf8(word.to_vec()).unwrap())
        .collect()
}

fn documented_config(server: &Server, name: &str, replacements: &[(&str, &str)]) {
    let mut config = documentation::snippet(name, "tmux");
    for (from, to) in replacements {
        config = config.replace(from, to);
    }
    let path = root().join(format!("documented-{name}.conf"));
    fs::write(&path, config).unwrap();
    server.run(&["source-file", path.to_str().unwrap()]);
}

#[test]
fn documented_tmux_setups_use_managed_external_direct_and_disabled_modes() {
    if !isolated("documented_tmux_setups_use_managed_external_direct_and_disabled_modes") {
        return;
    }
    let releases = Releases::new();
    releases.asset(
        "0.1.0",
        native_target(),
        &archive(&fs::read(BINARY).unwrap()),
        true,
    );
    let plugin = Plugin::new("documented plugin", &releases);
    let server = server();
    documented_config(&server, "tpm-managed", &[]);
    plugin.load(&server);
    wait_for("documented managed installation", || {
        plugin.installed("0.1.0").exists()
    });
    wait_for("documented managed lock cleanup", || {
        !plugin
            .installed("0.1.0")
            .parent()
            .unwrap()
            .with_extension("lock")
            .exists()
    });
    let mut client = Client::attach_to(&server, "plugin", 50, 160);
    client.until("documented status bodies", |screen| {
        screen.contents().contains("Claude – | Codex –")
    });
    assert!(option(&server, "status-right").contains(" status claude"));
    assert!(option(&server, "status-right").contains(" status codex"));
    let managed_format = option(&server, "status-right");

    let external = root().join("external root/bin/tmux-agent-usage");
    executable(&external, fs::read(BINARY).unwrap());
    documented_config(
        &server,
        "tpm-external",
        &[(
            "/your/chosen/root/bin/tmux-agent-usage",
            external.to_str().unwrap(),
        )],
    );
    documented_config(
        &server,
        "tpm-local",
        &[("/path/to/tmux-agent-usage", plugin.dir.to_str().unwrap())],
    );
    let before = releases.count();
    assert_eq!(
        option(&server, "@agent-usage-bin"),
        external.to_str().unwrap()
    );
    assert_ne!(option(&server, "status-right"), managed_format);
    // Opening the popup must keep working with the old managed binary absent.
    fs::rename(
        plugin.installed("0.1.0"),
        plugin.dir.join("managed-disabled"),
    )
    .unwrap();
    // One selected agent: only its placeholder runs. Opening/closing uses the
    // actual entrypoint key and native binary with no credential lookup.
    server.run(&[
        "set-option",
        "-g",
        "status-right",
        "Claude #{agent_usage_claude}",
    ]);
    server.run(&["set-option", "-g", "@agent-usage-agents", "claude"]);
    plugin.load(&server);
    client.send(b"\x02A");
    client.until("one-agent popup from documented setup", |screen| {
        let text = screen.contents();
        text.contains("Agent usage") && text.contains("Claude Code") && !text.contains("Codex")
    });
    client.send(b"q");
    client.until("one-agent popup closes", |screen| {
        !screen.contents().contains("Agent usage")
    });

    // No selected provider is represented by no status calls and no popup key;
    // an explicitly empty --agents list is separately tested as a usage error.
    server.run(&["set-option", "-g", "status-right", "disabled"]);
    server.run(&["set-option", "-g", "@agent-usage-popup-key", "none"]);
    plugin.load(&server);
    assert_eq!(option(&server, "status-right"), "disabled");
    assert!(binding(&server, "prefix", "A").is_empty());
    assert!(!root().join("forbidden-calls").exists());
    assert_eq!(releases.count(), before);

    // The no-TPM snippet is parsed by real tmux and opens the actual binary.
    executable(
        &root().join("fakes/tmux-agent-usage"),
        fs::read(BINARY).unwrap(),
    );
    documented_config(&server, "direct-tmux", &[]);
    client.until("direct status jobs", |screen| {
        screen.contents().contains("Claude – | Codex –")
    });
    client.send(b"\x02A");
    client.until("direct popup opens", |screen| {
        let text = screen.contents();
        text.contains("Agent usage") && text.contains("Claude Code") && text.contains("Codex")
    });
    // The controlled Codex failure must be displayed, then q closes the popup.
    client.until("direct popup reports provider failure", |screen| {
        screen.contents().contains("couldn't check")
    });
    client.send(b"q");
    client.until("direct popup closes", |screen| {
        !screen.contents().contains("Agent usage")
    });
    server.run(&["detach-client"]);
    assert!(client.child.wait().unwrap().success());
}

#[test]
fn managed_install_is_pinned_atomic_concurrent_and_updates_without_blocking_status() {
    if !isolated("managed_install_is_pinned_atomic_concurrent_and_updates_without_blocking_status")
    {
        return;
    }
    let releases = Releases::new();
    releases.hold.store(true, Ordering::SeqCst);
    let archive = archive(&fs::read(BINARY).unwrap());
    releases.asset("0.1.0", native_target(), &archive, true);
    releases.asset("0.1.1", native_target(), &archive, true);
    let plugin = Plugin::new("plugin layout with spaces", &releases);
    let server = server();
    server.run(&[
        "set-option",
        "-g",
        "status-left",
        "LEFT #{agent_usage_claude} #{session_name} ",
    ]);
    server.run(&[
        "set-option",
        "-g",
        "status-right",
        "RIGHT #{agent_usage_codex}",
    ]);
    server.run(&[
        "bind-key",
        "-n",
        "MouseDown1Status",
        "display-message",
        "original click",
    ]);
    let mouse = binding(&server, "root", "MouseDown1Status");
    let other_key = binding(&server, "prefix", "?");
    let mut loader = plugin.start_load(&server);
    wait_for(
        "loader finishes before the held download is released",
        || loader.finished(),
    );
    plugin.finish_load(loader);
    wait_for("first HTTP request", || releases.count() == 1);
    assert!(releases.hold.load(Ordering::SeqCst));
    assert!(!plugin.installed("0.1.0").exists());
    let mut client = Client::attach_to(&server, "plugin", 50, 160);
    client.until("status works during download", |screen| {
        screen.contents().contains("LEFT  plugin") && screen.contents().contains("RIGHT")
    });
    let mut second = Client::attach_to(&server, "plugin", 50, 160);
    let mut reloads = Vec::new();
    for _ in 0..4 {
        reloads.push(
            server
                .command()
                .args([
                    "run-shell",
                    &shell_word(plugin.dir.join("agent-usage.tmux").to_str().unwrap()),
                ])
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .unwrap(),
        );
    }
    for child in reloads {
        let output = success(child.wait_with_output().unwrap());
        assert!(
            output.stdout.is_empty(),
            "concurrent loader: {:?}",
            output.stdout
        );
        assert!(output.stderr.is_empty());
    }
    let mut installers = Vec::new();
    for _ in 0..4 {
        plugin.load(&server);
        installers.push(plugin.installer().output().unwrap());
    }
    assert_eq!(releases.count(), 1, "concurrent loaders downloaded twice");
    for output in installers {
        success(output);
    }
    releases.hold.store(false, Ordering::SeqCst);
    wait_for("atomic install", || plugin.installed("0.1.0").exists());
    wait_for("install lock cleanup", || {
        !plugin
            .installed("0.1.0")
            .parent()
            .unwrap()
            .with_extension("lock")
            .exists()
    });
    let installed = fs::read(plugin.installed("0.1.0")).unwrap();
    assert_eq!(installed, fs::read(BINARY).unwrap());
    wait_for("stable link to the first version", || {
        plugin.current() == Some(plugin.relative("0.1.0"))
    });
    assert_eq!(fs::read(plugin.stable()).unwrap(), installed);
    client.until("default status body appears", |screen| {
        screen.contents().contains("LEFT – plugin") && screen.contents().contains("RIGHT –")
    });
    second.until("second client shares the same segments", |screen| {
        screen.contents().contains("LEFT – plugin")
    });
    let left = option(&server, "status-left");
    let right = option(&server, "status-right");
    assert_eq!(left.matches(" status claude").count(), 1);
    assert_eq!(right.matches(" status codex").count(), 1);
    for style in [
        "fg=default",
        "fg=yellow",
        "fg=red",
        "fg=brightblack",
        "fg=green",
    ] {
        assert!(left.contains(style));
    }
    let popup = option(&server, "@agent-usage-popup-command");
    assert!(popup.contains("--agents"));
    assert!(!popup.contains("--claude-resets"));
    assert!(
        !popup.contains("--popup-"),
        "default popup must inherit tmux border/title and ANSI colors"
    );
    let original_binding = binding(&server, "prefix", "A");
    for _ in 0..3 {
        plugin.load(&server);
    }
    assert_eq!(option(&server, "status-left"), left);
    assert_eq!(option(&server, "status-right"), right);
    assert_eq!(binding(&server, "prefix", "A"), original_binding);
    assert_eq!(releases.count(), 2);
    // Exercise the native installed binary with the default key. Codex is a
    // controlled failing client; Claude credentials are never requested.
    client.send(b"\x02A");
    client.until("default popup opens", |screen| {
        screen.contents().contains("Agent usage")
            && screen.contents().contains("Claude Code")
            && screen.contents().contains("Codex")
    });
    client.send(b"q");
    client.until("default popup closes", |screen| {
        screen.contents().contains("ORIGINAL PANE") && !screen.contents().contains("Agent usage")
    });
    let manifest = plugin.dir.join("Cargo.toml");
    fs::write(
        &manifest,
        fs::read_to_string(&manifest).unwrap().replacen(
            "version = \"0.1.0\"",
            "version = \"0.1.1\"",
            1,
        ),
    )
    .unwrap();
    plugin.load(&server);
    wait_for("version update", || plugin.installed("0.1.1").exists());
    assert_eq!(fs::read(plugin.installed("0.1.0")).unwrap(), installed);
    assert_eq!(releases.count(), 4);
    wait_for("stable link follows the update", || {
        plugin.current() == Some(plugin.relative("0.1.1"))
    });
    // Selecting an already installed version relinks without downloading.
    for version in ["0.1.0", "0.1.1"] {
        let text = fs::read_to_string(&manifest).unwrap();
        let (_, rest) = text.split_once("version = \"").unwrap();
        let (current, _) = rest.split_once('"').unwrap();
        fs::write(
            &manifest,
            text.replacen(
                &format!("version = \"{current}\""),
                &format!("version = \"{version}\""),
                1,
            ),
        )
        .unwrap();
        plugin.load(&server);
        assert_eq!(plugin.current(), Some(plugin.relative(version)));
    }
    assert_eq!(releases.count(), 4);
    assert_eq!(option(&server, "@agent-usage-diagnostic"), "");
    // A user's own file at the stable path is never replaced.
    fs::remove_file(plugin.stable()).unwrap();
    fs::write(plugin.stable(), b"user file").unwrap();
    plugin.load(&server);
    assert_eq!(fs::read(plugin.stable()).unwrap(), b"user file");
    assert!(option(&server, "@agent-usage-diagnostic").contains("is not a symlink"));
    fs::remove_file(plugin.stable()).unwrap();
    plugin.load(&server);
    assert_eq!(plugin.current(), Some(plugin.relative("0.1.1")));
    assert_ne!(option(&server, "status-left"), left);
    fs::rename(
        plugin.installed("0.1.0"),
        plugin.dir.join("old-version-disabled"),
    )
    .unwrap();
    let updated = option(&server, "status-left").replacen("LEFT", "UPDATED", 1);
    server.run(&["set-option", "-g", "status-left", &updated]);
    client.until("updated command uses the new version", |screen| {
        screen.contents().contains("UPDATED – plugin")
    });
    assert_eq!(
        option(&server, "status-left")
            .matches(" status claude")
            .count(),
        1
    );
    assert_eq!(binding(&server, "prefix", "A"), original_binding);
    assert_eq!(binding(&server, "root", "MouseDown1Status"), mouse);
    assert_eq!(binding(&server, "prefix", "?"), other_key);
    assert!(!root().join(".local").exists());
    assert!(
        releases
            .requests
            .lock()
            .unwrap()
            .iter()
            .all(|path| !path.contains("latest"))
    );
}

#[test]
fn external_executable_quotes_options_and_preserves_user_bindings_on_reload() {
    if !isolated("external_executable_quotes_options_and_preserves_user_bindings_on_reload") {
        return;
    }
    let releases = Releases::new();
    let plugin = Plugin::new("plugin's # (fixture)", &releases);
    let binary = root().join("external bin's #() % executable");
    executable(&binary, b"#!/bin/sh\ncase \"$1\" in status) file=\"$HOME/argv-status-$2\" ;; popup) file=\"$HOME/argv-popup\" ;; esac\nprintf '%s\\0' \"$@\" > \"$file\"\nif [ \"$1\" = status ]; then printf '7%% \\302\\267 2%%'; fi\n");
    let original = fs::read(&binary).unwrap();
    let server = server();
    server.run(&[
        "set-option",
        "-g",
        "@agent-usage-bin",
        binary.to_str().unwrap(),
    ]);
    server.run(&[
        "set-option",
        "-g",
        "status-left",
        "Claude #{agent_usage_claude}",
    ]);
    server.run(&[
        "set-option",
        "-g",
        "status-right",
        "unrelated #{session_name} #(printf kept)",
    ]);
    let right = option(&server, "status-right");
    server.run(&["bind-key", "A", "display-message", "user shortcut"]);
    server.run(&[
        "bind-key",
        "-n",
        "MouseDown1Status",
        "display-message",
        "user mouse",
    ]);
    let original_binding = binding(&server, "prefix", "A");
    let mouse = binding(&server, "root", "MouseDown1Status");
    let literal = "fg=#123456, bold 'double\" # ) $(touch \"$HOME/injected\") #{session_name} %H `touch nope` \\";
    server.run(&["set-option", "-g", "@agent-usage-style-normal", literal]);
    // The plugin must pass on exactly what tmux stores. tmux 3.4 stores a `$`
    // followed by a variable name as `\$` (`$HOME` becomes `\$HOME`; `$(` is
    // kept), and later versions keep it, so compare with the stored value
    // rather than the text that was set.
    let stored = option(&server, "@agent-usage-style-normal");
    assert!(
        stored == literal || stored == literal.replace("$HOME", "\\$HOME"),
        "unexpected stored option: {stored:?}"
    );
    server.run(&["set-option", "-g", "@agent-usage-agents", "codex claude"]);
    server.run(&["set-option", "-g", "@agent-usage-claude-resets", "on"]);
    for (role, value) in [
        ("text", "#fbf1c7"),
        ("muted", "brightblack"),
        ("dim", "colour240"),
        ("ok", "green"),
        ("high", "yellow"),
        ("critical", "red"),
        ("accent", "blue"),
        ("border", "fg=#665c54, bold"),
    ] {
        server.run(&[
            "set-option",
            "-g",
            &format!("@agent-usage-popup-{role}"),
            value,
        ]);
    }
    plugin.load(&server);
    assert_eq!(binding(&server, "prefix", "A"), original_binding);
    assert!(option(&server, "@agent-usage-diagnostic").contains("already bound"));
    server.run(&["set-option", "-g", "@agent-usage-popup-key", "Z"]);
    plugin.load(&server);
    let mut client = Client::attach_to(&server, "plugin", 50, 160);
    client.until("quoted external status", |screen| {
        screen.contents().contains("Claude 7% · 2%")
    });
    assert_eq!(
        argv("argv-status-claude"),
        [
            "status",
            "claude",
            "--style-normal",
            stored.as_str(),
            "--style-high",
            "fg=yellow",
            "--style-critical",
            "fg=red",
            "--style-muted",
            "fg=brightblack",
            "--style-reset-soon",
            "fg=green"
        ]
    );
    assert!(
        !root().join("argv-status-codex").exists(),
        "unused placeholder ran a provider"
    );
    client.send(b"\x02Z");
    let client_name = server
        .run(&["display-message", "-p", "#{client_name}"])
        .trim()
        .to_owned();
    let popup_file = "argv-popup".to_owned();
    wait_for("quoted popup argv", || root().join(&popup_file).exists());
    let args = argv(&popup_file);
    assert_eq!(
        args,
        [
            "popup",
            &client_name,
            "160",
            "50",
            "--agents",
            "codex claude",
            "--claude-resets",
            "--popup-text",
            "#fbf1c7",
            "--popup-muted",
            "brightblack",
            "--popup-dim",
            "colour240",
            "--popup-ok",
            "green",
            "--popup-high",
            "yellow",
            "--popup-critical",
            "red",
            "--popup-accent",
            "blue",
            "--popup-border",
            "fg=#665c54, bold"
        ]
    );
    assert!(!root().join("injected").exists());
    assert!(!root().join("nope").exists());
    let left = option(&server, "status-left");
    let z = binding(&server, "prefix", "Z");
    for _ in 0..3 {
        plugin.load(&server);
    }
    assert_eq!(option(&server, "status-left"), left);
    assert_eq!(option(&server, "status-right"), right);
    assert_eq!(binding(&server, "prefix", "Z"), z);
    server.run(&[
        "set-option",
        "-g",
        "@agent-usage-style-normal",
        "fg=blue, bold",
    ]);
    plugin.load(&server);
    client.until("reload gets new flags without a duplicate segment", |_| {
        argv("argv-status-claude")
            .get(3)
            .is_some_and(|value| value == "fg=blue, bold")
    });
    assert_eq!(
        option(&server, "status-left")
            .matches(" status claude")
            .count(),
        1
    );
    server.run(&["set-option", "-g", "@agent-usage-popup-key", "Y"]);
    plugin.load(&server);
    assert!(binding(&server, "prefix", "Z").is_empty());
    server.run(&["bind-key", "Y", "display-message", "changed by user"]);
    let user_y = binding(&server, "prefix", "Y");
    server.run(&["set-option", "-g", "@agent-usage-popup-key", "none"]);
    plugin.load(&server);
    assert_eq!(binding(&server, "prefix", "Y"), user_y);
    server.run(&["set-option", "-g", "@agent-usage-popup-key", "X"]);
    plugin.load(&server);
    server.run(&["set-option", "-g", "@agent-usage-popup-key", "none"]);
    plugin.load(&server);
    assert!(binding(&server, "prefix", "X").is_empty());
    server.run(&[
        "bind-key",
        "C-M-x",
        "display-message",
        "alias belongs to user",
    ]);
    // tmux lists this key as C-M-x (3.7) or M-C-x (3.4); find whichever it uses.
    let listed = ["C-M-x", "M-C-x"]
        .into_iter()
        .find(|key| !binding(&server, "prefix", key).is_empty())
        .expect("user alias binding is listed");
    let alias_binding = binding(&server, "prefix", listed);
    server.run(&["set-option", "-g", "@agent-usage-popup-key", "M-C-x"]);
    plugin.load(&server);
    assert_eq!(binding(&server, "prefix", listed), alias_binding);
    assert!(option(&server, "@agent-usage-diagnostic").contains("already bound"));
    let manifest = plugin.dir.join("Cargo.toml");
    fs::write(
        &manifest,
        fs::read_to_string(&manifest).unwrap().replacen(
            "version = \"0.1.0\"",
            "version = \"0.1.1\"",
            1,
        ),
    )
    .unwrap();
    plugin.load(&server);
    assert_eq!(binding(&server, "prefix", "A"), original_binding);
    assert_eq!(binding(&server, "root", "MouseDown1Status"), mouse);
    assert_eq!(fs::read(&binary).unwrap(), original);
    assert_eq!(releases.count(), 0);
    assert!(!root().join("forbidden-calls").exists());
    assert!(!plugin.dir.join("bin/v0.1.0").exists());
    assert!(!plugin.dir.join("bin/v0.1.1").exists());
}

#[test]
fn failed_download_has_a_diagnostic_and_keeps_status_responsive_until_retry() {
    if !isolated("failed_download_has_a_diagnostic_and_keeps_status_responsive_until_retry") {
        return;
    }
    let releases = Releases::new();
    let plugin = Plugin::new("offline plugin", &releases);
    let server = server();
    server.run(&[
        "set-option",
        "-g",
        "status-left",
        "RESPONSIVE #{agent_usage_claude}",
    ]);
    server.run(&["set-option", "-g", "@agent-usage-popup-key", "none"]);
    plugin.load(&server);
    wait_for("failure diagnostic", || {
        option(&server, "@agent-usage-diagnostic").contains("managed installation failed")
    });
    let log = fs::read_to_string(plugin.dir.join("bin/install.log")).unwrap();
    assert!(log.contains("download failed for https://github.com/ramonmello/tmux-agent-usage/releases/download/v0.1.0/"));
    assert!(log.contains("@agent-usage-bin"));
    let mut client = Client::attach_to(&server, "plugin", 50, 160);
    client.until("failed install leaves status responsive", |screen| {
        screen.contents().contains("RESPONSIVE") && screen.contents().contains("ORIGINAL PANE")
    });
    assert!(!plugin.installed("0.1.0").exists());
    let count = releases.count();
    // A distinct async job prints an acknowledgement only after the missing
    // binary guard has run. Observe that refresh rather than sleeping for a
    // nominal status interval that may not have elapsed under load.
    let left = option(&server, "status-left");
    let command = left.strip_suffix(')').unwrap();
    server.run(&[
        "set-option",
        "-g",
        "status-left",
        &format!("{command}; printf REFRESHED)"),
    ]);
    client.until(
        "status job refresh finishes without retrying download",
        |screen| screen.contents().contains("RESPONSIVE REFRESHED"),
    );
    assert_eq!(releases.count(), count, "status refresh retried a download");
    server.run(&["set-option", "-g", "status-left", &left]);
    releases.asset(
        "0.1.0",
        native_target(),
        &archive(&fs::read(BINARY).unwrap()),
        true,
    );
    plugin.load(&server);
    wait_for("explicit reload retries", || {
        plugin.installed("0.1.0").exists()
    });
    client.until("body returns after retry", |screen| {
        screen.contents().contains("RESPONSIVE –")
    });
    assert_eq!(releases.count(), count + 2);
    assert!(!root().join("forbidden-calls").exists());
}

fn rejected(plugin: &Plugin, expected: &str) -> Output {
    let output = plugin.installer().output().unwrap();
    assert!(!output.status.success());
    assert!(
        String::from_utf8_lossy(&output.stderr).contains(expected),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(!plugin.installed("0.1.0").exists());
    assert!(!root().join("artifact-executed").exists());
    output
}

#[test]
fn checksum_architecture_archive_and_unsupported_target_fail_without_execution() {
    if !isolated("checksum_architecture_archive_and_unsupported_target_fail_without_execution") {
        return;
    }
    let releases = Releases::new();
    let bytes = fs::read(BINARY).unwrap();
    releases.asset("0.1.0", native_target(), &archive(&bytes), false);
    let plugin = Plugin::new("bad checksum", &releases);
    rejected(&plugin, "checksum mismatch");
    // A malicious script with a valid download checksum still cannot run.
    releases.asset(
        "0.1.0",
        native_target(),
        &archive(b"#!/bin/sh\ntouch \"$HOME/artifact-executed\"\n"),
        true,
    );
    rejected(
        &Plugin::new("script artifact", &releases),
        "incompatible executable",
    );
    let mut wrong_arch = bytes;
    if cfg!(target_os = "macos") {
        wrong_arch[4..8].copy_from_slice(&[0, 0, 0, 0]);
    } else {
        wrong_arch[18..20].copy_from_slice(&[0, 0]);
    }
    releases.asset("0.1.0", native_target(), &archive(&wrong_arch), true);
    rejected(
        &Plugin::new("wrong architecture", &releases),
        "incompatible executable",
    );
    let input = root().join("archive-input");
    fs::write(input.join("unexpected"), b"private path fixture").unwrap();
    let extra_file = success(
        Command::new("tar")
            .args(["-czf", "-", "-C"])
            .arg(&input)
            .args(["tmux-agent-usage", "unexpected"])
            .output()
            .unwrap(),
    )
    .stdout;
    releases.asset("0.1.0", native_target(), &extra_file, true);
    rejected(
        &Plugin::new("extra file", &releases),
        "invalid archive contents",
    );
    fs::remove_file(input.join("tmux-agent-usage")).unwrap();
    std::os::unix::fs::symlink("unexpected", input.join("tmux-agent-usage")).unwrap();
    let link = success(
        Command::new("tar")
            .args(["-czf", "-", "-C"])
            .arg(&input)
            .arg("tmux-agent-usage")
            .output()
            .unwrap(),
    )
    .stdout;
    releases.asset("0.1.0", native_target(), &link, true);
    rejected(
        &Plugin::new("link artifact", &releases),
        "not a regular file",
    );
    let count = releases.count();
    executable(
        &root().join("fakes/uname"),
        b"#!/bin/sh\ncase \"$1\" in -s) echo Linux ;; -m) echo riscv64 ;; esac\n",
    );
    rejected(
        &Plugin::new("unsupported architecture", &releases),
        "install with Cargo",
    );
    assert_eq!(releases.count(), count);
    assert!(!root().join("forbidden-calls").exists());
}
