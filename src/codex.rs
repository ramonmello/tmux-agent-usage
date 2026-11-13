//! Codex usage, read from the newest session rollout.
//!
//! Every Codex turn appends a `token_count` event carrying `rate_limits` to
//! `~/.codex/sessions/YYYY/MM/DD/rollout-*.jsonl`. Only the tail of the most
//! recently modified rollouts is read.

use crate::access as fs;
use serde::{Deserialize, Serialize};
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

const DAY_DIRS_TO_SCAN: usize = 3;
const FILES_TO_TRY: usize = 10;
const TAIL_CHUNKS: [u64; 3] = [64 * 1024, 1024 * 1024, 8 * 1024 * 1024];

#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub struct Window {
    pub used_percent: f64,
    pub window_secs: i64,
    pub resets_at: f64,
}

#[derive(Deserialize)]
struct Line {
    payload: Option<Payload>,
}

#[derive(Deserialize)]
struct Payload {
    rate_limits: Option<RateLimits>,
}

#[derive(Deserialize)]
struct RateLimits {
    primary: Option<RawWindow>,
    secondary: Option<RawWindow>,
    plan_type: Option<String>,
    credits: Option<Credits>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Credits {
    pub has_credits: bool,
    pub unlimited: bool,
    pub balance: Option<String>,
}

/// Everything the newest `token_count` event says about the account.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Status {
    /// Sorted from shortest to longest.
    pub windows: Vec<Window>,
    pub plan_type: Option<String>,
    pub credits: Option<Credits>,
    /// When the rollout holding the event was last written.
    pub observed_at: Option<i64>,
}

#[derive(Deserialize)]
struct RawWindow {
    used_percent: f64,
    window_minutes: Option<f64>,
    resets_at: Option<f64>,
}

pub fn load_status() -> Option<Status> {
    let sessions = crate::paths::codex_sessions_dir()?;
    recent_rollouts(&sessions)
        .iter()
        .take(FILES_TO_TRY)
        .find_map(|path| {
            let mut status = last_rate_limits(path)?;
            status.observed_at = fs::metadata(path)
                .and_then(|meta| meta.modified())
                .ok()
                .and_then(|modified| modified.duration_since(SystemTime::UNIX_EPOCH).ok())
                .map(|elapsed| elapsed.as_secs() as i64);
            Some(status)
        })
}

fn recent_rollouts(sessions: &Path) -> Vec<PathBuf> {
    let mut files: Vec<(SystemTime, PathBuf)> = Vec::new();
    let mut day_dirs = 0;

    'scan: for year in sorted_dirs_desc(sessions) {
        for month in sorted_dirs_desc(&year) {
            for day in sorted_dirs_desc(&month) {
                let Ok(entries) = fs::read_dir(&day) else {
                    continue;
                };
                for entry in entries.flatten() {
                    let path = entry.path();
                    if path
                        .extension()
                        .is_some_and(|extension| extension == "jsonl")
                        && let Ok(modified) =
                            fs::entry_metadata(&entry).and_then(|meta| meta.modified())
                    {
                        files.push((modified, path));
                    }
                }
                day_dirs += 1;
                if day_dirs >= DAY_DIRS_TO_SCAN {
                    break 'scan;
                }
            }
        }
    }

    files.sort_by(|left, right| right.0.cmp(&left.0));
    files.into_iter().map(|(_, path)| path).collect()
}

fn sorted_dirs_desc(dir: &Path) -> Vec<PathBuf> {
    let mut dirs: Vec<PathBuf> = fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| fs::metadata(path).is_ok_and(|metadata| metadata.is_dir()))
        .collect();
    dirs.sort_by(|left, right| right.cmp(left));
    dirs
}

fn last_rate_limits(path: &Path) -> Option<Status> {
    let mut file = fs::open(path).ok()?;
    let length = file.metadata().ok()?.len();

    for chunk in TAIL_CHUNKS {
        let start = length.saturating_sub(chunk);
        file.seek(SeekFrom::Start(start)).ok()?;
        let mut buffer = Vec::new();
        file.by_ref()
            .take(length - start)
            .read_to_end(&mut buffer)
            .ok()?;

        let text = String::from_utf8_lossy(&buffer);
        let mut lines = text.lines();
        if start > 0 {
            lines.next(); // partial line
        }
        let found = lines
            .rev()
            .filter(|line| line.contains("\"rate_limits\""))
            .find_map(parse_line);
        if found.is_some() || start == 0 {
            return found;
        }
    }
    None
}

fn parse_line(line: &str) -> Option<Status> {
    let limits = serde_json::from_str::<Line>(line)
        .ok()?
        .payload?
        .rate_limits?;
    let mut windows: Vec<Window> = [limits.primary, limits.secondary]
        .into_iter()
        .flatten()
        .filter_map(|raw| {
            Some(Window {
                used_percent: raw.used_percent,
                window_secs: raw
                    .window_minutes
                    .map_or(0, |minutes| (minutes * 60.0) as i64),
                resets_at: raw.resets_at?,
            })
        })
        .collect();
    windows.sort_by_key(|window| window.window_secs);
    (!windows.is_empty()).then_some(Status {
        windows,
        plan_type: limits.plan_type,
        credits: limits.credits,
        observed_at: None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_token_count_event() {
        let line = r#"{"timestamp":"2026-11-02T09:39:10.922Z","type":"event_msg","payload":{"type":"token_count","info":null,"rate_limits":{"limit_id":"codex","primary":{"used_percent":28.0,"window_minutes":10080,"resets_at":1793811730},"secondary":{"used_percent":4.0,"window_minutes":300,"resets_at":1793700000},"credits":null,"plan_type":"prolite"}}}"#;
        let status = parse_line(line).unwrap();
        assert_eq!(status.plan_type.as_deref(), Some("prolite"));
        let windows = status.windows;
        assert_eq!(windows.len(), 2);
        assert_eq!(windows[0].window_secs, 300 * 60);
        assert_eq!(windows[1].used_percent, 28.0);
    }

    #[test]
    fn ignores_null_rate_limits() {
        let line = r#"{"type":"event_msg","payload":{"type":"token_count","rate_limits":null}}"#;
        assert!(parse_line(line).is_none());
    }

    #[test]
    fn reads_last_event_from_tail() {
        let dir =
            std::env::temp_dir().join(format!("tmux-agent-usage-codex-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("rollout.jsonl");
        let event = |used: u32| {
            format!(
                r#"{{"type":"event_msg","payload":{{"type":"token_count","rate_limits":{{"primary":{{"used_percent":{used},"window_minutes":10080,"resets_at":1793811730}}}}}}}}"#
            )
        };
        let filler = format!(
            r#"{{"type":"response_item","payload":{{"text":"{}"}}}}"#,
            "x".repeat(200)
        );
        let mut body = event(10);
        for _ in 0..2000 {
            body.push('\n');
            body.push_str(&filler);
        }
        body.push('\n');
        body.push_str(&event(42));
        body.push('\n');
        body.push_str(&filler);
        fs::write(&path, body).unwrap();

        let status = last_rate_limits(&path).unwrap();
        assert_eq!(status.windows[0].used_percent, 42.0);
        let _ = fs::remove_dir_all(&dir);
    }
}
