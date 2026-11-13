use crate::test_support::{Sandbox, success};
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;

pub fn root() -> PathBuf {
    std::env::var_os("HOME").unwrap().into()
}

pub fn isolated(test: &str) -> bool {
    if std::env::var("AGENT_USAGE_ROBUST_CHILD").as_deref() == Ok(test) {
        return true;
    }
    // Each case runs in a fresh process and uses the production I/O path.
    let sandbox = Sandbox::new();
    success(
        sandbox
            .command(std::env::current_exe().unwrap())
            .args(["--exact", test, "--nocapture"])
            .env("AGENT_USAGE_ROBUST_CHILD", test)
            .env_remove("CLAUDE_CONFIG_DIR")
            .env_remove("CLAUDE_SECURESTORAGE_CONFIG_DIR")
            .env("PATH", sandbox.root.join("fakes"))
            .output()
            .expect("run isolated fixture process"),
    );
    false
}

pub fn executable(name: &str, script: &str) {
    let path = root().join("fakes").join(name);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    let body = script
        .strip_prefix("#!/bin/sh\n")
        .expect("fixture scripts start with #!/bin/sh");
    fs::write(
        &path,
        format!("#!/bin/sh\nif [ \"${{AGENT_USAGE_WARM_UP-}}\" = 1 ]; then exit 0; fi\n{body}"),
    )
    .unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).unwrap();
    // macOS checks each new executable on its first exec, and queues those
    // checks: with many tests starting at once that can take seconds. Pay it
    // here with a no-op run, outside the 1s deadlines these fixtures exercise.
    let status = std::process::Command::new(&path)
        .env("AGENT_USAGE_WARM_UP", "1")
        .status()
        .expect("warm up fixture executable");
    assert!(status.success(), "fixture warm-up failed: {name}");
}
