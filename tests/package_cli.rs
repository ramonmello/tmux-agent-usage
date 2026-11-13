#[path = "support/documentation.rs"]
mod documentation;
#[path = "support/packaging.rs"]
mod packaging;
mod support;

use packaging::{clean_copy, files, tool_command};
use std::collections::BTreeSet;
use std::fs;
use std::path::Path;
use std::process::Command;
use support::{Sandbox, success};

const SOURCE: &str = env!("CARGO_MANIFEST_DIR");

fn cargo(sandbox: &Sandbox, source: &Path) -> Command {
    let mut command = tool_command(sandbox, env!("CARGO"));
    command.current_dir(source);
    command
}

fn listed_files(sandbox: &Sandbox, source: &Path) -> BTreeSet<String> {
    let output = success(
        cargo(sandbox, source)
            .args(["package", "--locked", "--offline", "--list"])
            .output()
            .unwrap(),
    );
    String::from_utf8(output.stdout)
        .unwrap()
        .lines()
        .map(str::to_owned)
        .collect()
}

#[test]
#[ignore = "slow: rebuilds and reruns the whole suite from a clean copy; scripts/ci.sh runs it"]
fn clean_copy_packages_builds_tests_and_installs_from_a_fresh_root() {
    let sandbox = Sandbox::new();
    let source = sandbox.root.join("independent source");
    clean_copy(&sandbox, Path::new(SOURCE), &source);
    let source = source.canonicalize().unwrap();
    let inventory = files(&source);
    assert!(!source.join(".git").exists());
    assert!(!source.join("target").exists());
    assert_eq!(
        fs::read(source.join("Cargo.lock")).unwrap(),
        fs::read(Path::new(SOURCE).join("Cargo.lock")).unwrap()
    );

    let output = success(
        cargo(&sandbox, &source)
            .args([
                "metadata",
                "--locked",
                "--offline",
                "--no-deps",
                "--format-version",
                "1",
            ])
            .output()
            .unwrap(),
    );
    let metadata: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    let package = &metadata["packages"][0];
    assert_eq!(package["name"], "tmux-agent-usage");
    assert_eq!(package["version"], "0.1.0");
    assert_eq!(package["rust_version"], "1.88");
    assert_eq!(package["license"], "MIT");
    assert_eq!(package["authors"], serde_json::json!(["Ramon Mello"]));
    assert_eq!(
        package["repository"],
        "https://github.com/ramonmello/tmux-agent-usage"
    );
    assert_eq!(package["readme"], "README.md");
    assert!(package["publish"].is_null());
    assert!(
        package["dependencies"]
            .as_array()
            .unwrap()
            .iter()
            .all(|dependency| dependency["path"].is_null())
    );
    for target in package["targets"].as_array().unwrap() {
        assert!(Path::new(target["src_path"].as_str().unwrap()).starts_with(&source));
    }

    success(
        cargo(&sandbox, &source)
            .args(["build", "--locked", "--offline"])
            .output()
            .unwrap(),
    );
    // Run every binary and integration-test target from the clean copy.
    // Only this packaging orchestrator is excluded to avoid infinite recursion;
    // no production, golden, tmux, provider or script test case is filtered out.
    let mut tests = cargo(&sandbox, &source);
    tests.args(["test", "--locked", "--offline"]);
    for target in package["targets"].as_array().unwrap() {
        let name = target["name"].as_str().unwrap();
        match target["kind"][0].as_str().unwrap() {
            "bin" => {
                tests.args(["--bin", name]);
            }
            "test" if name != env!("CARGO_CRATE_NAME") => {
                tests.args(["--test", name]);
            }
            _ => {}
        }
    }
    // Slow release checks are #[ignore]d for the local loop, not here.
    tests.args(["--", "--include-ignored"]);
    success(tests.output().unwrap());
    let listed = listed_files(&sandbox, &source);
    assert!(!listed.contains(".cargo_vcs_info.json"));
    let mut source_list = listed.clone();
    assert!(source_list.remove("Cargo.toml.orig"));
    assert!(source_list.is_subset(&inventory));
    for file in inventory.difference(&source_list) {
        assert!(
            file.starts_with(".github/workflows/"),
            "unexpected repository-only file: {file}"
        );
    }
    success(
        cargo(&sandbox, &source)
            .args(["package", "--locked", "--offline"])
            .output()
            .unwrap(),
    );

    let name = format!("tmux-agent-usage-{}", env!("CARGO_PKG_VERSION"));
    let packaged = sandbox.root.join("build/package").join(&name);
    let archive = sandbox
        .root
        .join("build/package")
        .join(format!("{name}.crate"));
    let output = success(
        sandbox
            .command("tar")
            .arg("-tzf")
            .arg(&archive)
            .output()
            .unwrap(),
    );
    let archived: BTreeSet<_> = String::from_utf8(output.stdout)
        .unwrap()
        .lines()
        .filter(|path| !path.ends_with('/'))
        .map(|path| path.strip_prefix(&format!("{name}/")).unwrap().to_owned())
        .collect();
    assert_eq!(archived, listed);
    assert!(!packaged.join(".git").exists());

    let installed = success(
        tool_command(&sandbox, "/bin/sh")
            .args(["-eu", "-c", &documentation::snippet("source-install", "sh")])
            .current_dir(&packaged)
            .env("TMPDIR", &sandbox.root)
            .output()
            .unwrap(),
    );
    let output = String::from_utf8(installed.stdout).unwrap();
    let binary = Path::new(output.lines().last().unwrap());
    assert!(
        binary.starts_with(&sandbox.root),
        "installation escaped test root: {}",
        binary.display()
    );
    let installation = binary.parent().unwrap().parent().unwrap();
    let first_binary = fs::read(binary).unwrap();
    // Execute the documented update against the same root, with Cargo real and
    // offline. The installed binary must remain executable and unchanged.
    success(
        tool_command(&sandbox, "/bin/sh")
            .args(["-eu", "-c", &documentation::snippet("source-update", "sh")])
            .current_dir(&packaged)
            .env("install_root", installation)
            .output()
            .unwrap(),
    );
    assert_eq!(fs::read(binary).unwrap(), first_binary);
    let help = success(sandbox.command(binary).arg("help").output().unwrap());
    assert!(String::from_utf8_lossy(&help.stdout).contains("claude-tap"));
    for agent in ["claude", "codex"] {
        let output = success(
            sandbox
                .command(binary)
                .args(["status", agent])
                .output()
                .unwrap(),
        );
        assert_eq!(output.stdout, "–".as_bytes());
    }
    assert!(!sandbox.root.join(".local").exists());
    assert!(!sandbox.root.join("cache").exists());
    assert_eq!(files(&source), inventory, "Cargo changed the copied source");
}
