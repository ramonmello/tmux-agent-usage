//! Codex saved rate-limit resets, read through the official `codex app-server`
//! JSON-RPC protocol (`account/rateLimits/read`). Codex handles login and
//! token refresh itself; this only reads.
//!
//! Only called when the details popup opens, never from `status`. A
//! successful answer is cached for a few minutes so repeated clicks do not
//! start Codex again.
//!
//! The protocol separates "unknown" from "zero": `rateLimitResetCredits: null`
//! means unavailable, `credits: null` means only the count is known, and an
//! empty `credits` list means it was checked and there are none. Anything we
//! cannot read with certainty becomes an error state, never a number.

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::time::Duration;

const TIMEOUT: Duration = Duration::from_secs(10);
const CACHE: &str = "codex-resets";

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Credit {
    /// `None` = does not expire.
    pub expires_at: Option<i64>,
    pub title: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum SavedResets {
    /// Checked; `credits` lists the ones with a known expiry. When the
    /// backend capped the list, `count` is larger than `credits.len()`.
    Known { count: u64, credits: Vec<Credit> },
    /// Checked; the count is known but not the details.
    CountOnly { count: u64 },
    /// Codex answered but did not include saved resets.
    Unavailable,
    /// Could not ask or could not understand the answer.
    Failed(String),
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Summary {
    available_count: u64,
    credits: Option<Vec<RawCredit>>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawCredit {
    status: String,
    expires_at: Option<f64>,
}

/// Stand-in used only to size the popup before the real answer arrives.
pub const PLACEHOLDER: SavedResets = SavedResets::CountOnly { count: 1 };

/// The last answer, however old, without asking Codex. Only for sizing the
/// popup: the number of saved resets rarely changes between opens, so a
/// stale answer predicts the final height far better than a placeholder.
fn scope() -> Option<String> {
    crate::paths::cache_scope("codex-stdio-v1", &crate::paths::codex_sessions_dir()?)
}

fn cached(value: Value) -> Option<SavedResets> {
    let saved: SavedResets = serde_json::from_value(value).ok()?;
    matches!(
        saved,
        SavedResets::Known { .. } | SavedResets::CountOnly { .. } | SavedResets::Unavailable
    )
    .then_some(saved)
}

pub fn last_known() -> Option<SavedResets> {
    crate::cache::any(CACHE, &scope()?).and_then(cached)
}

pub fn load(now: i64) -> SavedResets {
    let scope = scope();
    if let Some(cached) = scope
        .as_deref()
        .and_then(|scope| crate::cache::fresh(CACHE, scope, now))
        .and_then(cached)
    {
        return cached;
    }
    match query(TIMEOUT) {
        Ok(value) => {
            let parsed = parse(&value);
            if !matches!(parsed, SavedResets::Failed(_))
                && let Some(scope) = scope.as_deref()
            {
                crate::cache::store(
                    CACHE,
                    scope,
                    now,
                    serde_json::to_value(&parsed).expect("typed answer"),
                );
            }
            parsed
        }
        Err(error) => SavedResets::Failed(error),
    }
}

/// Turns the `rateLimitResetCredits` value into a state.
fn parse(value: &Value) -> SavedResets {
    if value.is_null() {
        return SavedResets::Unavailable;
    }
    let summary: Summary = match serde_json::from_value(value.clone()) {
        Ok(summary) => summary,
        Err(_) => return SavedResets::Failed("unexpected answer format".to_owned()),
    };
    let Some(raw) = summary.credits else {
        return SavedResets::CountOnly {
            count: summary.available_count,
        };
    };
    if raw.iter().any(|credit| {
        credit
            .expires_at
            .is_some_and(|at| !at.is_finite() || at < i64::MIN as f64 || at >= i64::MAX as f64)
    }) {
        return SavedResets::Failed("unreadable expiry".to_owned());
    }
    let mut credits: Vec<Credit> = raw
        .into_iter()
        .filter(|credit| credit.status == "available")
        .map(|credit| Credit {
            expires_at: credit.expires_at.map(|at| at as i64),
            title: None,
        })
        .collect();
    credits.sort_by_key(|credit| credit.expires_at.unwrap_or(i64::MAX));
    // The list may be capped, so the count can be larger, but a list longer
    // than the count means the two disagree: trust neither.
    if credits.len() as u64 > summary.available_count {
        return SavedResets::Failed("count and list disagree".to_owned());
    }
    SavedResets::Known {
        count: summary.available_count,
        credits,
    }
}

/// Asks `codex app-server` and returns `result.rateLimitResetCredits`
/// (`Value::Null` when absent).
fn query(timeout: Duration) -> Result<Value, String> {
    let mut process = crate::process::Process::spawn(
        crate::access::command("codex").args(["app-server", "--listen", "stdio://"]),
        timeout,
    )?;
    talk(&mut process)
}

fn talk(process: &mut crate::process::Process) -> Result<Value, String> {
    send(
        process,
        json!({
            "jsonrpc": "2.0", "id": 1, "method": "initialize",
            "params": {"clientInfo": {
                "name": "tmux-agent-usage", "title": "tmux agent usage",
                "version": env!("CARGO_PKG_VERSION"),
            }},
        }),
    )?;
    let initialized = wait_for(process, 1)?;
    if !initialized.is_object() {
        return Err("unexpected initialize response".into());
    }
    send(process, json!({"jsonrpc": "2.0", "method": "initialized"}))?;
    send(
        process,
        json!({"jsonrpc": "2.0", "id": 2, "method": "account/rateLimits/read"}),
    )?;
    let result = wait_for(process, 2)?;
    let Value::Object(result) = result else {
        return Err("unexpected answer format".to_owned());
    };
    Ok(result
        .get("rateLimitResetCredits")
        .cloned()
        .unwrap_or(Value::Null))
}

fn send(process: &mut crate::process::Process, message: Value) -> Result<(), String> {
    process.write(format!("{message}\n").as_bytes())
}

/// Ignore unrelated notifications/requests. Never service auth refresh,
/// login, turn or consume requests. Provider error bodies are not displayed
/// or persisted: they may contain credentials or terminal control sequences.
fn wait_for(process: &mut crate::process::Process, id: u64) -> Result<Value, String> {
    loop {
        let line = process.line()?.ok_or("codex closed early")?;
        let message: Value =
            serde_json::from_slice(&line).map_err(|_| "unexpected answer format")?;
        if !message.is_object() {
            return Err("unexpected answer format".into());
        }
        if message.get("method").is_some() || message.get("id").and_then(Value::as_u64) != Some(id)
        {
            continue;
        }
        if message.get("error").is_some() {
            return Err("codex rejected the read request".into());
        }
        return message
            .get("result")
            .cloned()
            .ok_or_else(|| "unexpected answer format".into());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn null_is_unavailable() {
        assert_eq!(parse(&Value::Null), SavedResets::Unavailable);
    }

    #[test]
    fn empty_list_is_none() {
        let value = json!({"availableCount": 0, "credits": []});
        assert_eq!(
            parse(&value),
            SavedResets::Known {
                count: 0,
                credits: vec![]
            }
        );
    }

    #[test]
    fn missing_list_is_count_only() {
        let value = json!({"availableCount": 2, "credits": null});
        assert_eq!(parse(&value), SavedResets::CountOnly { count: 2 });
    }

    #[test]
    fn keeps_only_available_sorted_by_expiry() {
        let value = json!({"availableCount": 2, "credits": [
            {"id": "a", "resetType": "codexRateLimits", "status": "available", "grantedAt": 1, "expiresAt": 300, "title": "Full reset", "description": null},
            {"id": "b", "resetType": "codexRateLimits", "status": "redeemed", "grantedAt": 1, "expiresAt": 100, "title": null, "description": null},
            {"id": "c", "resetType": "codexRateLimits", "status": "available", "grantedAt": 1, "expiresAt": 200, "title": null, "description": null},
        ]});
        let SavedResets::Known { count, credits } = parse(&value) else {
            panic!("expected known");
        };
        assert_eq!(count, 2);
        assert_eq!(
            credits.iter().map(|c| c.expires_at).collect::<Vec<_>>(),
            vec![Some(200), Some(300)]
        );
    }

    #[test]
    fn malformed_is_failure_not_zero() {
        assert!(matches!(
            parse(&json!({"availableCount": "two"})),
            SavedResets::Failed(_)
        ));
        assert!(matches!(parse(&json!([])), SavedResets::Failed(_)));
        assert!(matches!(
            parse(&json!({"availableCount": 0, "credits": [
                {"status": "available", "expiresAt": 1}
            ]})),
            SavedResets::Failed(_)
        ));
    }
}

#[cfg(test)]
#[path = "../tests/support/codex_robustness.rs"]
mod robustness;
