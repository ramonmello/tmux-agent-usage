//! Private, atomic five-minute cache. Only typed, allowlisted answers are
//! stored by callers. Scope prevents another client configuration's answer
//! from being reused. Unscoped legacy files cannot satisfy scoped readers.
use crate::access as fs;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::path::PathBuf;

pub const TTL_SECS: i64 = 5 * 60;

#[derive(Serialize, Deserialize)]
struct Cached {
    fetched_at: i64,
    #[serde(default)]
    scope: String,
    value: Value,
}

fn path(name: &str) -> Option<PathBuf> {
    crate::paths::cache_dir().map(|dir| dir.join(format!("{name}.json")))
}

fn read(name: &str, scope: &str) -> Option<Cached> {
    let cached: Cached = serde_json::from_slice(&fs::read(path(name)?).ok()?).ok()?;
    (cached.scope == scope).then_some(cached)
}

pub fn fresh(name: &str, scope: &str, now: i64) -> Option<Value> {
    let cached = read(name, scope)?;
    (0..TTL_SECS)
        .contains(&now.saturating_sub(cached.fetched_at))
        .then_some(cached.value)
}

pub fn any(name: &str, scope: &str) -> Option<Value> {
    read(name, scope).map(|cached| cached.value)
}

pub fn store(name: &str, scope: &str, now: i64, value: Value) {
    let Some(path) = path(name) else { return };
    let cached = Cached {
        fetched_at: now,
        scope: scope.to_owned(),
        value,
    };
    if let Ok(body) = serde_json::to_vec(&cached) {
        let _ = fs::write_private(&path, &body);
    }
}
