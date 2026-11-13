#[path = "support/packaging.rs"]
mod packaging;
mod support;

use packaging::{clean_copy, files, tool_command};
use serde_json::Value;
use std::fs;
use std::path::Path;
use support::{Sandbox, success};

#[test]
#[ignore = "slow: release build from a clean copy; scripts/ci.sh runs it"]
fn native_release_and_scripts_verify_tags_abi_checksums_and_installer_compatibility() {
    let sandbox = Sandbox::new();
    let source = sandbox.root.join("tagged clean source");
    clean_copy(&sandbox, Path::new(env!("CARGO_MANIFEST_DIR")), &source);
    let git = |arguments: &[&str]| {
        success(
            sandbox
                .command("git")
                .current_dir(&source)
                .env("GIT_CONFIG_NOSYSTEM", "1")
                .env("GIT_CONFIG_GLOBAL", "/dev/null")
                .args([
                    "-c",
                    "user.name=Release tests",
                    "-c",
                    "user.email=tests@example.invalid",
                    "-c",
                    "commit.gpgsign=false",
                    "-c",
                    "tag.gpgsign=false",
                ])
                .args(arguments)
                .output()
                .unwrap(),
        )
    };
    git(&["init", "-q"]);
    git(&["add", "."]);
    git(&["commit", "-qm", "Synthetic release fixture"]);
    let tag = format!("v{}", env!("CARGO_PKG_VERSION"));
    git(&["tag", &tag]);
    let target = match (std::env::consts::OS, std::env::consts::ARCH) {
        ("macos", "aarch64") => "aarch64-apple-darwin",
        ("macos", "x86_64") => "x86_64-apple-darwin",
        ("linux", "aarch64") => "aarch64-unknown-linux-musl",
        ("linux", "x86_64") => "x86_64-unknown-linux-musl",
        platform => panic!("unsupported native release test platform: {platform:?}"),
    };
    let assets = sandbox.root.join("native assets");
    success(
        tool_command(&sandbox, "python3")
            .arg(source.join("scripts/release.py"))
            .args(["build", &tag, target])
            .arg(&assets)
            .output()
            .unwrap(),
    );
    let extracted = sandbox.root.join("unpacked");
    fs::create_dir(&extracted).unwrap();
    success(
        sandbox
            .command("tar")
            .arg("-xzf")
            .arg(assets.join(format!("tmux-agent-usage-{tag}-{target}.tar.gz")))
            .arg("-C")
            .arg(&extracted)
            .output()
            .unwrap(),
    );
    let binary = extracted.join("tmux-agent-usage");
    let help = success(sandbox.command(&binary).arg("help").output().unwrap());
    assert!(String::from_utf8_lossy(&help.stdout).contains("claude-tap"));
    for agent in ["claude", "codex"] {
        let output = success(
            sandbox
                .command(&binary)
                .args(["status", agent])
                .output()
                .unwrap(),
        );
        assert_eq!(output.stdout, "–".as_bytes());
    }
    assert!(git(&["status", "--porcelain"]).stdout.is_empty());
    success(
        tool_command(&sandbox, "python3")
            .arg(Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/release_scripts.py"))
            .env("AGENT_USAGE_TEST_BINARY", &binary)
            .env("PYTHONDONTWRITEBYTECODE", "1")
            .output()
            .unwrap(),
    );
}

#[test]
fn workflows_cover_native_targets_and_cannot_publish_a_release_or_crate() {
    let sandbox = Sandbox::new();
    let source = Path::new(env!("CARGO_MANIFEST_DIR"));
    let independent = sandbox.root.join("clean copy");
    clean_copy(&sandbox, source, &independent);
    let inventory = files(&independent);
    for file in [
        "scripts/ci.sh",
        "scripts/release.py",
        ".github/workflows/ci.yml",
        ".github/workflows/release.yml",
    ] {
        assert!(inventory.contains(file), "missing release asset {file}");
        assert_eq!(
            fs::read(independent.join(file)).unwrap(),
            fs::read(source.join(file)).unwrap()
        );
    }
    let read = |name| -> Value {
        serde_json::from_slice(&fs::read(independent.join(".github/workflows").join(name)).unwrap())
            .unwrap()
    };
    // JSON is a YAML subset accepted by Actions. Parsing the complete workflows
    // here keeps syntax and structural validation inside cargo test.
    let ci = read("ci.yml");
    let release = read("release.yml");
    let platforms = &ci["jobs"]["checks"]["strategy"]["matrix"]["platform"];
    assert_eq!(
        platforms,
        &serde_json::json!([
            {"runner": "macos-15", "target": "aarch64-apple-darwin"},
            {"runner": "macos-15-intel", "target": "x86_64-apple-darwin"},
            {"runner": "ubuntu-24.04", "target": "x86_64-unknown-linux-musl"},
            {"runner": "ubuntu-24.04-arm", "target": "aarch64-unknown-linux-musl"},
        ])
    );
    assert_eq!(
        &release["jobs"]["build"]["strategy"]["matrix"]["platform"],
        platforms
    );
    assert_eq!(
        ci["jobs"]["checks"]["strategy"]["matrix"]["toolchain"],
        serde_json::json!(["1.88.0", "stable"])
    );
    assert_eq!(
        release["jobs"]["checks"]["uses"],
        "./.github/workflows/ci.yml"
    );
    let source_guard = release["jobs"]["source"]["steps"][1]["run"]
        .as_str()
        .unwrap();
    assert!(source_guard.contains("test \"$WORKFLOW_REF\" = \"refs/tags/$RELEASE_TAG\""));
    assert!(source_guard.contains("test \"$commit\" = \"$WORKFLOW_SHA\""));
    assert_eq!(release["on"].as_object().unwrap().len(), 1);
    assert!(release["on"]["workflow_dispatch"]["inputs"]["tag"]["required"] == true);
    assert_eq!(
        release["permissions"],
        serde_json::json!({"contents": "read"})
    );
    assert_eq!(
        release["jobs"]["assemble"]["permissions"],
        serde_json::json!({"contents": "read", "id-token": "write", "attestations": "write"})
    );
    assert_eq!(
        release["jobs"]["build"]["needs"],
        serde_json::json!(["source", "checks"])
    );
    for workflow in [&ci, &release] {
        let serialized = workflow.to_string();
        for forbidden in [
            "secrets.",
            "cargo login",
            "gh release",
            "git push",
            "continue-on-error",
            "--ignore",
            "pull_request_target",
        ] {
            assert!(
                !serialized.contains(forbidden),
                "workflow contains {forbidden}"
            );
        }
        for job in workflow["jobs"].as_object().unwrap().values() {
            if let Some(steps) = job["steps"].as_array() {
                for step in steps {
                    if let Some(action) = step["uses"].as_str() {
                        let (_, revision) = action.split_once('@').unwrap();
                        assert_eq!(revision.len(), 40, "unpinned action {action}");
                        assert!(revision.bytes().all(|byte| byte.is_ascii_hexdigit()));
                        if action.starts_with("actions/checkout@") {
                            assert_eq!(step["with"]["persist-credentials"], false);
                        }
                    }
                }
            }
        }
    }
}
