mod support;

use std::fs::{self, File};
use std::io::Write;
use std::os::fd::{AsRawFd, FromRawFd};
use std::os::unix::fs::PermissionsExt;
use std::process::{Child, Output, Stdio};
use std::time::{Duration, Instant};
use support::{EVENT_TIMEOUT, Sandbox, success};

const BINARY: &str = env!("CARGO_BIN_EXE_tmux-agent-usage");
const RESTORE: &[u8] = b"\x1b[?1006l\x1b[?1000l\x1b[?25h";

fn fake(sandbox: &Sandbox, name: &str, script: &str) {
    let path = sandbox.root.join("fakes").join(name);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(&path, script).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o700)).unwrap();
}

struct Running(Option<Child>);
impl Running {
    fn wait(mut self) -> Output {
        let deadline = Instant::now() + EVENT_TIMEOUT;
        loop {
            if self.0.as_mut().unwrap().try_wait().unwrap().is_some() {
                return self.0.take().unwrap().wait_with_output().unwrap();
            }
            assert!(Instant::now() < deadline, "isolated CLI did not exit");
            std::thread::sleep(Duration::from_millis(5));
        }
    }
}
impl Drop for Running {
    fn drop(&mut self) {
        if let Some(child) = &mut self.0 {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

struct Pty {
    master: File,
    slave: File,
}
impl Pty {
    fn new() -> Self {
        let (mut master, mut slave) = (-1, -1);
        let mut size = libc::winsize {
            ws_row: 45,
            ws_col: 100,
            ws_xpixel: 0,
            ws_ypixel: 0,
        };
        assert_eq!(
            unsafe {
                libc::openpty(
                    &mut master,
                    &mut slave,
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                    &mut size,
                )
            },
            0,
            "open isolated pty: {}",
            std::io::Error::last_os_error()
        );
        let pty = Self {
            master: unsafe { File::from_raw_fd(master) },
            slave: unsafe { File::from_raw_fd(slave) },
        };
        let mut attributes = pty.attributes();
        attributes.c_lflag |= libc::ICANON;
        attributes.c_lflag &= !libc::ECHO; // preserve an initially unusual mode
        attributes.c_cc[libc::VMIN] = 7;
        attributes.c_cc[libc::VTIME] = 3;
        assert_eq!(
            unsafe { libc::tcsetattr(pty.slave.as_raw_fd(), libc::TCSANOW, &attributes) },
            0
        );
        pty
    }
    fn attributes(&self) -> libc::termios {
        let mut result = std::mem::MaybeUninit::uninit();
        assert_eq!(
            unsafe { libc::tcgetattr(self.slave.as_raw_fd(), result.as_mut_ptr()) },
            0
        );
        unsafe { result.assume_init() }
    }
    fn assert_restored(&self, original: &libc::termios) {
        let actual = self.attributes();
        assert_eq!(
            (
                actual.c_iflag,
                actual.c_oflag,
                actual.c_cflag,
                actual.c_lflag
            ),
            (
                original.c_iflag,
                original.c_oflag,
                original.c_cflag,
                original.c_lflag
            )
        );
        assert_eq!(actual.c_cc, original.c_cc);
        assert_eq!(unsafe { libc::cfgetispeed(&actual) }, unsafe {
            libc::cfgetispeed(original)
        });
        assert_eq!(unsafe { libc::cfgetospeed(&actual) }, unsafe {
            libc::cfgetospeed(original)
        });
    }
    fn ready(&self, running: &mut Running) {
        let deadline = Instant::now() + EVENT_TIMEOUT;
        loop {
            let raw = self.attributes().c_lflag & libc::ICANON == 0;
            let fd = running
                .0
                .as_ref()
                .unwrap()
                .stdout
                .as_ref()
                .unwrap()
                .as_raw_fd();
            let mut poll = libc::pollfd {
                fd,
                events: libc::POLLIN,
                revents: 0,
            };
            if raw && unsafe { libc::poll(&mut poll, 1, 0) } > 0 {
                return;
            }
            assert!(
                running.0.as_mut().unwrap().try_wait().unwrap().is_none(),
                "details exited before opening"
            );
            assert!(Instant::now() < deadline, "details did not enter raw mode");
            std::thread::sleep(Duration::from_millis(5));
        }
    }
}

#[test]
fn disabled_claude_resets_use_no_credentials_or_helper_processes() {
    let sandbox = Sandbox::new();
    fs::create_dir_all(sandbox.root.join("claude")).unwrap();
    // Reading the credential FIFO would block; the timeout guard makes that a
    // failed test, and also ensures no child survives an assertion failure.
    let path = std::ffi::CString::new(
        sandbox
            .root
            .join("claude/.credentials.json")
            .as_os_str()
            .as_encoded_bytes(),
    )
    .unwrap();
    assert_eq!(unsafe { libc::mkfifo(path.as_ptr(), 0o600) }, 0);
    for name in ["security", "curl", "claude", "codex", "stty"] {
        fake(
            &sandbox,
            name,
            "#!/bin/sh\nprintf called >> \"$HOME/tripped\"\nexit 99\n",
        );
    }
    for args in [
        vec!["status", "claude"],
        vec!["details", "--no-wait", "--agents", "claude"],
    ] {
        let output = success(
            Running(Some(
                sandbox
                    .command(BINARY)
                    .args(&args)
                    .env("PATH", sandbox.root.join("fakes"))
                    .stdin(Stdio::null())
                    .stdout(Stdio::piped())
                    .stderr(Stdio::piped())
                    .spawn()
                    .unwrap(),
            ))
            .wait(),
        );
        assert!(!String::from_utf8_lossy(&output.stdout).contains("Saved resets"));
        assert!(!sandbox.root.join("tripped").exists());
        assert!(!sandbox.root.join("cache").exists());
    }
}

#[test]
fn pty_restores_exact_modes_cursor_and_mouse_on_key_eof_and_signals() {
    for signal in [
        None,
        Some(libc::SIGINT),
        Some(libc::SIGTERM),
        Some(libc::SIGHUP),
        Some(libc::SIGQUIT),
    ] {
        let sandbox = Sandbox::new();
        let mut pty = Pty::new();
        let original = pty.attributes();
        let mut running = Running(Some(
            sandbox
                .command(BINARY)
                .args(["details", "--agents", "claude"])
                .stdin(pty.slave.try_clone().unwrap())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .unwrap(),
        ));
        pty.ready(&mut running);
        if let Some(signal) = signal {
            assert_eq!(
                unsafe { libc::kill(running.0.as_ref().unwrap().id() as libc::pid_t, signal) },
                0
            );
        } else {
            pty.master.write_all(b"q").unwrap();
        }
        let output = running.wait();
        assert_eq!(output.status.success(), signal.is_none());
        assert!(output.stdout.ends_with(RESTORE));
        pty.assert_restored(&original);
    }
    let sandbox = Sandbox::new();
    let output = success(
        sandbox
            .command(BINARY)
            .args(["details", "--agents", "claude"])
            .stdin(Stdio::null())
            .output()
            .unwrap(),
    );
    assert!(output.stdout.ends_with(RESTORE)); // EOF on non-tty stdin
}

#[test]
fn pty_restores_modes_when_stdout_is_broken() {
    let sandbox = Sandbox::new();
    let pty = Pty::new();
    let original = pty.attributes();
    let mut fds = [-1; 2];
    assert_eq!(unsafe { libc::pipe(fds.as_mut_ptr()) }, 0);
    drop(unsafe { File::from_raw_fd(fds[0]) });
    let output = Running(Some(
        sandbox
            .command(BINARY)
            .args(["details", "--agents", "claude"])
            .stdin(pty.slave.try_clone().unwrap())
            .stdout(unsafe { File::from_raw_fd(fds[1]) })
            .stderr(Stdio::piped())
            .spawn()
            .unwrap(),
    ))
    .wait();
    assert!(!output.status.success());
    pty.assert_restored(&original);
}

#[test]
fn signal_cancels_and_reaps_inflight_codex_before_restoring_terminal() {
    let sandbox = Sandbox::new();
    let pty = Pty::new();
    let original = pty.attributes();
    fake(
        &sandbox,
        "codex",
        "#!/bin/sh\nprintf '%s' \"$$\" > \"$HOME/codex-pid\"\nIFS= read -r initialize\nIFS= read -r never_sent\n",
    );
    let mut running = Running(Some(
        sandbox
            .command(BINARY)
            .args(["details", "--agents", "codex"])
            .env("PATH", sandbox.root.join("fakes"))
            .stdin(pty.slave.try_clone().unwrap())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap(),
    ));
    pty.ready(&mut running);
    let deadline = Instant::now() + EVENT_TIMEOUT;
    let pid: libc::pid_t = loop {
        if let Ok(text) = fs::read_to_string(sandbox.root.join("codex-pid"))
            && let Ok(pid) = text.parse()
        {
            break pid;
        }
        assert!(Instant::now() < deadline, "fake Codex did not start");
        std::thread::sleep(Duration::from_millis(5));
    };
    assert_eq!(
        unsafe {
            libc::kill(
                running.0.as_ref().unwrap().id() as libc::pid_t,
                libc::SIGTERM,
            )
        },
        0
    );
    let output = running.wait();
    assert!(!output.status.success());
    assert!(output.stdout.ends_with(RESTORE));
    pty.assert_restored(&original);
    assert_eq!(unsafe { libc::kill(pid, 0) }, -1);
    assert_eq!(
        std::io::Error::last_os_error().raw_os_error(),
        Some(libc::ESRCH)
    );
}

#[test]
fn invalid_configuration_and_unknown_macos_keychain_mapping_are_visible_and_do_not_spawn() {
    let sandbox = Sandbox::new();
    for name in ["security", "curl", "claude"] {
        fake(
            &sandbox,
            name,
            "#!/bin/sh\nprintf called >> \"$HOME/tripped\"\nexit 99\n",
        );
    }
    let mut cases = vec![
        ("", "CLAUDE_CONFIG_DIR is empty"),
        ("relative", "CLAUDE_CONFIG_DIR must be absolute"),
    ];
    let alternative = sandbox.root.join("alternative");
    if cfg!(target_os = "macos") {
        cases.push((
            alternative.to_str().unwrap(),
            "cannot determine Keychain entry for CLAUDE_CONFIG_DIR",
        ));
    }
    for (configuration, expected) in cases {
        let output = success(
            sandbox
                .command(BINARY)
                .args([
                    "details",
                    "--no-wait",
                    "--agents",
                    "claude",
                    "--claude-resets",
                ])
                .env("CLAUDE_CONFIG_DIR", configuration)
                .env_remove("CLAUDE_SECURESTORAGE_CONFIG_DIR")
                .env("PATH", sandbox.root.join("fakes"))
                .output()
                .unwrap(),
        );
        let text = String::from_utf8(output.stdout).unwrap();
        assert!(
            text.contains(expected)
                && text.contains("couldn't check")
                && text.contains("No data yet")
        );
        assert!(!sandbox.root.join("tripped").exists());
    }
}
