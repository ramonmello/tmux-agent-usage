//! Provider I/O boundary. Tests observe attempted accesses, including failed ones.

use std::path::Path;
use std::process::Command;

/// Cache directories and atomic writes are private even under umask 000.
/// Refuse symlink directories and never follow a pre-created temporary file.
pub fn private_dir(path: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::{DirBuilderExt, MetadataExt, PermissionsExt};
    #[cfg(test)]
    record("create_dir_all", path);
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(path)?;
    let metadata = std::fs::symlink_metadata(path)?;
    // SAFETY: geteuid has no arguments or side effects.
    if !metadata.is_dir() || metadata.uid() != unsafe { libc::geteuid() } {
        return Err(std::io::Error::other("cache directory is not private"));
    }
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))
}

pub fn write_private(path: &Path, body: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    use std::sync::atomic::{AtomicUsize, Ordering};
    static NEXT: AtomicUsize = AtomicUsize::new(0);
    let dir = path
        .parent()
        .ok_or_else(|| std::io::Error::other("no cache directory"))?;
    private_dir(dir)?;
    let name = path
        .file_name()
        .ok_or_else(|| std::io::Error::other("no cache name"))?;
    let temporary = dir.join(format!(
        ".{}.{}.{}.tmp",
        name.to_string_lossy(),
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    #[cfg(test)]
    record("write", path);
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(&temporary)?;
    let result = file
        .write_all(body)
        .and_then(|_| std::fs::rename(&temporary, path));
    if result.is_err() {
        let _ = std::fs::remove_file(temporary);
    }
    result
}

#[cfg(not(test))]
pub use std::fs::{metadata, read, read_dir, remove_file};

pub fn open(path: impl AsRef<Path>) -> std::io::Result<std::fs::File> {
    #[cfg(test)]
    record("open", path.as_ref());
    std::fs::File::open(path)
}

pub fn entry_metadata(entry: &std::fs::DirEntry) -> std::io::Result<std::fs::Metadata> {
    #[cfg(test)]
    record("entry_metadata", &entry.path());
    entry.metadata()
}

pub fn command(program: impl AsRef<std::ffi::OsStr>) -> Command {
    #[cfg(test)]
    record("command", Path::new(program.as_ref()));
    Command::new(program)
}

#[cfg(test)]
pub use observed::*;

#[cfg(test)]
mod observed {
    use std::fs;
    use std::io;
    use std::path::{Path, PathBuf};
    use std::sync::Mutex;

    #[derive(Debug)]
    pub struct Access {
        pub operation: &'static str,
        pub target: PathBuf,
    }

    static ACCESSES: Mutex<Option<Vec<Access>>> = Mutex::new(None);

    pub fn start() {
        *ACCESSES.lock().unwrap() = Some(Vec::new());
    }

    pub fn take() -> Vec<Access> {
        ACCESSES.lock().unwrap().take().unwrap()
    }

    pub fn record(operation: &'static str, target: &Path) {
        if let Some(events) = ACCESSES.lock().unwrap().as_mut() {
            events.push(Access {
                operation,
                target: target.to_owned(),
            });
        }
    }

    macro_rules! observe {
        ($name:ident, $output:ty) => {
            pub fn $name(path: impl AsRef<Path>) -> io::Result<$output> {
                record(stringify!($name), path.as_ref());
                fs::$name(path)
            }
        };
    }

    observe!(read, Vec<u8>);
    observe!(read_dir, fs::ReadDir);
    observe!(metadata, fs::Metadata);
    observe!(create_dir_all, ());
    observe!(remove_file, ());

    #[cfg(test)]
    pub fn write(path: impl AsRef<Path>, contents: impl AsRef<[u8]>) -> io::Result<()> {
        record("write", path.as_ref());
        fs::write(path, contents)
    }

    // Test fixture cleanup is outside the observed provider boundary.
    pub use fs::remove_dir_all;
}
