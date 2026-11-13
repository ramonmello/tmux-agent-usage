// In all your ways submit to him, and he will make your paths straight. — Proverbs 3:6

use std::env;
use std::path::PathBuf;

fn non_empty_var(name: &str) -> Option<PathBuf> {
    env::var_os(name)
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
}

fn home() -> Option<PathBuf> {
    non_empty_var("HOME")
}

pub fn cache_dir() -> Option<PathBuf> {
    non_empty_var("XDG_CACHE_HOME")
        .or_else(|| home().map(|home| home.join(".cache")))
        .map(|cache| cache.join("tmux-agent-usage"))
}

pub fn codex_sessions_dir() -> Option<PathBuf> {
    non_empty_var("CODEX_HOME")
        .or_else(|| home().map(|home| home.join(".codex")))
        .map(|codex| codex.join("sessions"))
}

/// Lossless directory identity for private caches, including non-UTF-8 Unix
/// paths. Resolve relative paths against the working directory without reading
/// the client's files or following symlinks.
pub fn cache_scope(prefix: &str, path: &std::path::Path) -> Option<String> {
    use std::fmt::Write;
    let path = std::path::absolute(path).ok()?;
    let mut scope = format!("{prefix}:");
    for byte in path.as_os_str().as_encoded_bytes() {
        let _ = write!(scope, "{byte:02x}");
    }
    Some(scope)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::ffi::OsStringExt;

    #[test]
    fn cache_identity_keeps_non_utf8_directories_distinct_and_resolves_relative_paths() {
        let a = PathBuf::from(std::ffi::OsString::from_vec(b"/fixture/\xff".to_vec()));
        let b = PathBuf::from(std::ffi::OsString::from_vec(b"/fixture/\xfe".to_vec()));
        assert_ne!(cache_scope("fixture", &a), cache_scope("fixture", &b));
        assert_eq!(
            cache_scope("fixture", std::path::Path::new("relative")),
            cache_scope(
                "fixture",
                &std::env::current_dir().unwrap().join("relative")
            )
        );
    }
}
