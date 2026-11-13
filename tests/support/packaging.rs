use crate::support::{Sandbox, success};
use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

pub fn tool_command(sandbox: &Sandbox, program: impl AsRef<std::ffi::OsStr>) -> Command {
    let mut command = sandbox.command(program);
    let original_home = PathBuf::from(std::env::var_os("HOME").unwrap());
    // Only toolchain/cache locations are retained. Provider paths and HOME stay
    // in Sandbox; nested Cargo cannot resolve or download a different lockfile.
    command
        .env(
            "CARGO_HOME",
            std::env::var_os("CARGO_HOME").unwrap_or_else(|| original_home.join(".cargo").into()),
        )
        .env(
            "RUSTUP_HOME",
            std::env::var_os("RUSTUP_HOME").unwrap_or_else(|| original_home.join(".rustup").into()),
        )
        .env("CARGO_NET_OFFLINE", "true")
        .env("CARGO_TARGET_DIR", sandbox.root.join("build"));
    // The shell scripts call Cargo by name. Prefer the same Cargo that is
    // running this test, including when the source/toolchain paths have spaces.
    let mut path = vec![Path::new(env!("CARGO")).parent().unwrap().to_path_buf()];
    path.extend(std::env::split_paths(&std::env::var_os("PATH").unwrap()));
    command.env("PATH", std::env::join_paths(path).unwrap());
    command
}

/// Copy what a fresh clone holds: the Cargo package inputs plus the
/// repository-only CI workflows. Build, test and release checks run from this
/// copy, so nothing from the working directory (target/, bin/, caches) leaks in.
pub fn clean_copy(sandbox: &Sandbox, source: &Path, destination: &Path) {
    let output = success(
        tool_command(sandbox, env!("CARGO"))
            .args(["package", "--locked", "--allow-dirty", "--list"])
            .current_dir(source)
            .output()
            .unwrap(),
    );
    let mut list: BTreeSet<String> = String::from_utf8(output.stdout)
        .unwrap()
        .lines()
        .filter(|file| !matches!(*file, ".cargo_vcs_info.json" | "Cargo.toml.orig"))
        .map(str::to_owned)
        .collect();
    let workflows = source.join(".github/workflows");
    for entry in fs::read_dir(&workflows).unwrap() {
        let name = entry.unwrap().file_name().into_string().unwrap();
        if name.ends_with(".yml") {
            list.insert(format!(".github/workflows/{name}"));
        }
    }
    assert!(!destination.exists(), "clean copy destination exists");
    for file in &list {
        let relative = Path::new(file);
        for ancestor in relative
            .ancestors()
            .filter(|path| !path.as_os_str().is_empty())
        {
            let kind = fs::symlink_metadata(source.join(ancestor))
                .unwrap()
                .file_type();
            assert!(!kind.is_symlink(), "symlink in repository path: {file}");
        }
        let target = destination.join(relative);
        fs::create_dir_all(target.parent().unwrap()).unwrap();
        // fs::copy keeps the mode, so scripts stay executable.
        fs::copy(source.join(relative), target).unwrap();
    }
}

pub fn files(root: &Path) -> BTreeSet<String> {
    fn visit(root: &Path, dir: &Path, result: &mut BTreeSet<String>) {
        for entry in fs::read_dir(dir).unwrap() {
            let entry = entry.unwrap();
            let kind = entry.file_type().unwrap();
            assert!(!kind.is_symlink(), "copy contains a symlink");
            if kind.is_dir() {
                visit(root, &entry.path(), result);
            } else {
                assert!(kind.is_file());
                result.insert(
                    entry
                        .path()
                        .strip_prefix(root)
                        .unwrap()
                        .to_str()
                        .unwrap()
                        .into(),
                );
            }
        }
    }
    let mut result = BTreeSet::new();
    visit(root, root, &mut result);
    result
}
