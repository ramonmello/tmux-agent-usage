//! Claude Code usage, captured from the status line payload.
//!
//! Claude Code only exposes subscription usage to its status line command
//! (`rate_limits` on stdin), so `claude-tap` records it per session. A session
//! snapshot is rewritten only when its values change, which keeps an idle
//! session re-rendering stale numbers from overriding a fresher one.

use crate::access as fs;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::time::{Duration, UNIX_EPOCH};

const SNAPSHOT_MAX_AGE: Duration = Duration::from_secs(8 * 24 * 60 * 60);

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Window {
    pub used_percentage: f64,
    pub resets_at: f64,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Limits {
    #[serde(default)]
    pub five_hour: Option<Window>,
    #[serde(default)]
    pub seven_day: Option<Window>,
}

#[derive(Deserialize)]
struct StatusInput {
    session_id: Option<String>,
    rate_limits: Option<Limits>,
}

#[derive(Serialize, Deserialize)]
struct Snapshot {
    observed_at: i64,
    #[serde(flatten)]
    limits: Limits,
}

pub fn snapshots_dir() -> Option<PathBuf> {
    crate::paths::cache_dir().map(|cache| cache.join("claude"))
}

pub fn record(input: &[u8], now: i64) -> Result<(), String> {
    let dir = snapshots_dir().ok_or("cannot resolve cache directory")?;
    fs::private_dir(dir.parent().ok_or("cannot resolve cache root")?)
        .map_err(|_| "cannot create private cache")?;
    record_in(&dir, input, now)
}

/// The limits plus when Claude Code last reported a change.
pub fn load_with_time() -> Option<(Limits, i64)> {
    load_from(&snapshots_dir()?)
}

/// Diagnostic only: distinguish a missing tap snapshot from an empty or
/// incompatible snapshot without changing status output or recording anything.
pub fn snapshot_status(limits: Option<&Limits>) -> &'static str {
    if let Some(limits) = limits {
        return if limits.five_hour.is_some() || limits.seven_day.is_some() {
            "available"
        } else {
            "no_usage"
        };
    }
    let Some(dir) = snapshots_dir() else {
        return "path_unavailable";
    };
    let entries = match fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return "no_snapshots",
        Err(_) => return "unreadable",
    };
    for entry in entries {
        let Ok(entry) = entry else {
            return "unreadable";
        };
        if entry.path().extension().is_some_and(|ext| ext == "json") {
            return "invalid_snapshots";
        }
    }
    "no_snapshots"
}

fn record_in(dir: &Path, input: &[u8], now: i64) -> Result<(), String> {
    let parsed: StatusInput =
        serde_json::from_slice(input).map_err(|error| format!("invalid status JSON: {error}"))?;
    let Some(limits) = parsed.rate_limits else {
        return Ok(());
    };

    let id = sanitize_session_id(parsed.session_id.as_deref().unwrap_or("default"));
    let path = dir.join(format!("{id}.json"));
    if read_snapshot(&path).is_some_and(|previous| previous.limits == limits) {
        return Ok(());
    }

    fs::private_dir(dir).map_err(|error| format!("cannot create {}: {error}", dir.display()))?;
    let snapshot = Snapshot {
        observed_at: now,
        limits,
    };
    let body = serde_json::to_vec(&snapshot).map_err(|error| error.to_string())?;
    fs::write_private(&path, &body).map_err(|error| error.to_string())?;

    prune(dir, now);
    Ok(())
}

/// Picks, for each window, the value from the most recently changed session.
fn load_from(dir: &Path) -> Option<(Limits, i64)> {
    let mut five_hour: Option<(i64, Window)> = None;
    let mut seven_day: Option<(i64, Window)> = None;
    let mut found = false;

    for entry in fs::read_dir(dir).ok()?.flatten() {
        let path = entry.path();
        if path.extension().is_none_or(|extension| extension != "json") {
            continue;
        }
        let Some(snapshot) = read_snapshot(&path) else {
            continue;
        };
        found = true;
        keep_newest(
            &mut five_hour,
            snapshot.observed_at,
            snapshot.limits.five_hour,
        );
        keep_newest(
            &mut seven_day,
            snapshot.observed_at,
            snapshot.limits.seven_day,
        );
    }

    let observed_at = five_hour
        .iter()
        .chain(seven_day.iter())
        .map(|(at, _)| *at)
        .max()
        .unwrap_or(0);
    found.then(|| {
        (
            Limits {
                five_hour: five_hour.map(|(_, window)| window),
                seven_day: seven_day.map(|(_, window)| window),
            },
            observed_at,
        )
    })
}

fn keep_newest(slot: &mut Option<(i64, Window)>, observed_at: i64, window: Option<Window>) {
    let Some(window) = window else { return };
    if slot.is_none_or(|(current, _)| observed_at > current) {
        *slot = Some((observed_at, window));
    }
}

fn read_snapshot(path: &Path) -> Option<Snapshot> {
    serde_json::from_slice(&fs::read(path).ok()?).ok()
}

fn sanitize_session_id(id: &str) -> String {
    let clean: String = id
        .chars()
        .filter(|character| character.is_ascii_alphanumeric() || matches!(character, '-' | '_'))
        .take(64)
        .collect();
    if clean.is_empty() {
        "default".to_owned()
    } else {
        clean
    }
}

fn prune(dir: &Path, now: i64) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    let now = UNIX_EPOCH + Duration::from_secs(now.max(0) as u64);
    for entry in entries.flatten() {
        let stale = fs::entry_metadata(&entry)
            .and_then(|metadata| metadata.modified())
            .ok()
            .and_then(|modified| now.duration_since(modified).ok())
            .is_some_and(|age| age > SNAPSHOT_MAX_AGE);
        if stale {
            let _ = fs::remove_file(entry.path());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "tmux-agent-usage-test-{name}-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&dir);
        dir
    }

    fn payload(session: &str, five: f64, seven: f64) -> Vec<u8> {
        format!(
            r#"{{"session_id":"{session}","model":{{"id":"x"}},"rate_limits":{{
                "five_hour":{{"used_percentage":{five},"resets_at":1793811730}},
                "seven_day":{{"used_percentage":{seven},"resets_at":1794000000}}}}}}"#
        )
        .into_bytes()
    }

    #[test]
    fn ignores_payload_without_rate_limits() {
        let dir = temp_dir("no-limits");
        record_in(&dir, br#"{"session_id":"a"}"#, 10).unwrap();
        assert!(load_from(&dir).is_none());
    }

    #[test]
    fn newest_changed_session_wins() {
        let dir = temp_dir("newest");
        record_in(&dir, &payload("a", 10.0, 1.0), 100).unwrap();
        record_in(&dir, &payload("b", 20.0, 2.0), 200).unwrap();
        // Session "a" re-renders its stale values later: must not win.
        record_in(&dir, &payload("a", 10.0, 1.0), 300).unwrap();

        let (limits, _) = load_from(&dir).unwrap();
        assert_eq!(limits.five_hour.unwrap().used_percentage, 20.0);
        assert_eq!(limits.seven_day.unwrap().used_percentage, 2.0);

        record_in(&dir, &payload("a", 30.0, 3.0), 400).unwrap();
        let (limits, _) = load_from(&dir).unwrap();
        assert_eq!(limits.five_hour.unwrap().used_percentage, 30.0);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn sanitizes_session_ids() {
        assert_eq!(sanitize_session_id("../../etc"), "etc");
        assert_eq!(sanitize_session_id("///"), "default");
        assert_eq!(sanitize_session_id("abc-123_x"), "abc-123_x");
    }
}
