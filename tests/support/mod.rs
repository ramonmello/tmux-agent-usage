use std::fs;
use std::path::PathBuf;
use std::process::{Command, Output};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

static NEXT_SANDBOX: AtomicUsize = AtomicUsize::new(0);

#[allow(dead_code)]
pub const EVENT_TIMEOUT: Duration = Duration::from_secs(30);

#[allow(dead_code)]
pub fn wait_for(description: &str, mut predicate: impl FnMut() -> bool) {
    let deadline = Instant::now() + EVENT_TIMEOUT;
    while !predicate() {
        assert!(Instant::now() < deadline, "timeout: {description}");
        std::thread::sleep(Duration::from_millis(10));
    }
}

pub struct Sandbox {
    pub root: PathBuf,
}

impl Sandbox {
    pub fn new() -> Self {
        let root = std::env::temp_dir().join(format!(
            "agent-usage-baseline-{}-{}",
            std::process::id(),
            NEXT_SANDBOX.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&root).expect("create isolated test directory");
        Self { root }
    }

    pub fn command(&self, program: impl AsRef<std::ffi::OsStr>) -> Command {
        let mut command = Command::new(program);
        command
            .env("HOME", &self.root)
            .env("XDG_CACHE_HOME", self.root.join("cache"))
            .env("XDG_CONFIG_HOME", self.root.join("config"))
            .env("XDG_DATA_HOME", self.root.join("data"))
            .env("CODEX_HOME", self.root.join("codex"))
            .env("CLAUDE_CONFIG_DIR", self.root.join("claude"))
            .env("TZ", "UTC0")
            .env("SHELL", "/bin/sh")
            .env_remove("TMUX")
            .env_remove("TMUX_PANE");
        command
    }
}

impl Drop for Sandbox {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

pub fn success(output: Output) -> Output {
    assert!(
        output.status.success(),
        "command failed: {}\nstdout: {}\nstderr: {}",
        output.status,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    output
}
