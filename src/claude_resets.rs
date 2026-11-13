//! Claude Code saved limit resets (the free resets Anthropic grants, used
//! with `/limit-reset`), read from `GET /api/oauth/usage?cedar_ember=1`, the
//! endpoint Claude Code itself reads them from. It is not documented, and it
//! only answers to Claude Code's own user agent, so this sends Claude Code's
//! installed version in it.
//!
//! Only called when the details popup opens, never from `status`, read only,
//! and a good answer is cached for a few minutes (`crate::cache`). The OAuth
//! token is Claude Code's; it is read, never refreshed (refreshing would
//! rotate Claude Code's own token), so an expired token is a failure until
//! Claude Code refreshes it. The token goes to curl on stdin, never in argv.
//!
//! Like the Codex reader, anything uncertain becomes an error state, never a
//! number.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::io::Read;
use std::path::PathBuf;
use std::time::{Duration, UNIX_EPOCH};

const URL: &str = "https://api.anthropic.com/api/oauth/usage?cedar_ember=1&skip_spend=1";
const CACHE: &str = "claude-resets";
const TIMEOUT: Duration = Duration::from_secs(10);
const BACKOFF: &str = "claude-resets-backoff";

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Group {
    pub kind: Kind,
    pub count: u64,
    /// `None` = does not expire.
    pub expires_at: Option<i64>,
    /// Why it cannot be used right now, if it cannot.
    pub blocked: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum Kind {
    /// Clears the weekly limit too.
    Full,
    /// Clears the 5-hour window and what Fable used this week.
    Fable,
    /// Clears only the 5-hour window.
    Session,
    Other,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum ClaudeResets {
    /// Checked; groups sorted by expiry (soonest first). Empty = none.
    Known {
        groups: Vec<Group>,
        cooldown_until: Option<i64>,
    },
    /// Claude answered that resets do not apply to this account.
    Unavailable,
    Failed(String),
    /// A local credential or rate-limit state with an actionable, fixed message.
    Blocked(String),
}

/// Stand-in used only to size the popup before the first answer ever.
pub const PLACEHOLDER: ClaudeResets = ClaudeResets::Unavailable;

#[derive(Deserialize)]
struct Status {
    eligible: bool,
    #[serde(default)]
    grants: Vec<RawGrant>,
    cooldown_until: Option<String>,
}

#[derive(Deserialize)]
struct RawGrant {
    resets_left: u64,
    ends_at: Option<String>,
    #[serde(default)]
    clears: Vec<String>,
    #[serde(default)]
    paused: bool,
    #[serde(default = "yes")]
    usable_now: bool,
    unavailable_reason: Option<String>,
}

fn yes() -> bool {
    true
}

fn cached(value: Value, now: i64) -> Option<ClaudeResets> {
    match serde_json::from_value(value).ok()? {
        ClaudeResets::Known {
            mut groups,
            cooldown_until,
        } => {
            groups.retain(|group: &Group| group.expires_at.is_none_or(|at| at > now));
            Some(ClaudeResets::Known {
                groups,
                cooldown_until: cooldown_until.filter(|at| *at > now),
            })
        }
        ClaudeResets::Unavailable => Some(ClaudeResets::Unavailable),
        _ => None,
    }
}

/// Sizing never reads credentials or starts a process.
pub fn last_known(now: i64) -> Option<ClaudeResets> {
    let source = CredentialSource::current().ok()?;
    source.ensure_supported().ok()?;
    crate::cache::any(CACHE, &source.scope()).and_then(|value| cached(value, now))
}

pub fn load(now: i64) -> ClaudeResets {
    let source = match CredentialSource::current() {
        Ok(source) => source,
        Err(error) => return ClaudeResets::Blocked(error),
    };
    load_from(&source, now, TIMEOUT)
}

#[derive(Default, Serialize, Deserialize)]
struct Backoff {
    attempts: u32,
    retry_at: i64,
    started_at: i64,
}

fn load_from(source: &CredentialSource, now: i64, timeout: Duration) -> ClaudeResets {
    if let Err(error) = source.ensure_supported() {
        return ClaudeResets::Blocked(error);
    }
    let scope = source.scope();
    if let Some(cached) =
        crate::cache::fresh(CACHE, &scope, now).and_then(|value| cached(value, now))
    {
        return cached;
    }
    let backoff: Backoff = crate::cache::any(BACKOFF, &scope)
        .and_then(|value| serde_json::from_value(value).ok())
        .unwrap_or_default();
    if backoff.started_at <= now && now < backoff.retry_at {
        return ClaudeResets::Blocked("rate limited · retry later".into());
    }
    match query(source, now, timeout) {
        Ok(value) => {
            let parsed = parse(&value, now);
            if matches!(
                parsed,
                ClaudeResets::Known { .. } | ClaudeResets::Unavailable
            ) {
                // Never cache opaque provider fields (or the credentials).
                crate::cache::store(
                    CACHE,
                    &scope,
                    now,
                    serde_json::to_value(&parsed).expect("typed answer"),
                );
                crate::cache::store(
                    BACKOFF,
                    &scope,
                    now,
                    serde_json::to_value(Backoff::default()).expect("backoff"),
                );
            }
            parsed
        }
        Err(QueryError::Limited(retry_at)) => {
            let attempts = backoff.attempts.saturating_add(1).min(7);
            let delay = (60_i64 << (attempts - 1)).min(3600);
            let backoff = Backoff {
                attempts,
                retry_at: retry_at
                    .unwrap_or_else(|| now.saturating_add(delay))
                    .max(now.saturating_add(1)),
                started_at: now,
            };
            crate::cache::store(
                BACKOFF,
                &scope,
                now,
                serde_json::to_value(backoff).expect("backoff"),
            );
            ClaudeResets::Blocked("rate limited · retry later".into())
        }
        Err(QueryError::State(state)) => state,
    }
}

/// Turns the `cedar_ember` block into a state.
fn parse(value: &Value, now: i64) -> ClaudeResets {
    if value.get("eligible").and_then(Value::as_bool) == Some(true)
        && !value.get("grants").is_some_and(Value::is_array)
    {
        return ClaudeResets::Failed("unexpected answer format".to_owned());
    }
    let status: Status = match serde_json::from_value(value.clone()) {
        Ok(status) => status,
        Err(_) => return ClaudeResets::Failed("unexpected answer format".to_owned()),
    };
    if !status.eligible {
        return ClaudeResets::Unavailable;
    }

    let mut groups: Vec<Group> = Vec::new();
    for grant in status.grants {
        if grant.resets_left == 0 {
            continue;
        }
        let expires_at = match grant.ends_at.as_deref() {
            None => None,
            Some(text) => match parse_iso(text) {
                Some(at) => Some(at),
                None => return ClaudeResets::Failed("unreadable expiry".to_owned()),
            },
        };
        if expires_at.is_some_and(|at| at <= now) {
            continue;
        }
        let blocked = if grant.paused {
            Some("paused".to_owned())
        } else if !grant.usable_now {
            Some(blocked_reason(grant.unavailable_reason.as_deref()).to_owned())
        } else {
            None
        };
        let kind = kind(&grant.clears);
        match groups.iter_mut().find(|group| {
            group.kind == kind && group.expires_at == expires_at && group.blocked == blocked
        }) {
            Some(group) => {
                let Some(count) = group.count.checked_add(grant.resets_left) else {
                    return ClaudeResets::Failed("reset count overflow".into());
                };
                group.count = count;
            }
            None => groups.push(Group {
                kind,
                count: grant.resets_left,
                expires_at,
                blocked,
            }),
        }
    }
    if groups
        .iter()
        .try_fold(0_u64, |sum, group| sum.checked_add(group.count))
        .is_none()
    {
        return ClaudeResets::Failed("reset count overflow".into());
    }
    groups.sort_by_key(|group| (group.expires_at.unwrap_or(i64::MAX), group.kind));

    let cooldown_until = match status.cooldown_until.as_deref() {
        None => None,
        Some(text) => match parse_iso(text) {
            Some(at) => (at > now).then_some(at),
            None => return ClaudeResets::Failed("unreadable cooldown".into()),
        },
    };
    ClaudeResets::Known {
        groups,
        cooldown_until,
    }
}

fn blocked_reason(reason: Option<&str>) -> &'static str {
    match reason {
        Some("needs_limit") => "usable once you hit a limit",
        Some("cooldown") => "cooling down",
        Some("paused") => "paused",
        _ => "can't use right now",
    }
}

fn kind(clears: &[String]) -> Kind {
    let has = |name: &str| clears.iter().any(|clear| clear == name);
    if has("seven_day") {
        Kind::Full
    } else if has("seven_day_overage_included") {
        Kind::Fable
    } else if has("five_hour") && clears.len() == 1 {
        Kind::Session
    } else {
        Kind::Other
    }
}

/// `2026-12-10T22:00:00+00:00` / `…00.183Z` → unix seconds.
fn parse_iso(text: &str) -> Option<i64> {
    let (date, rest) = text.split_once('T')?;
    let mut date_parts = date.split('-').map(str::parse::<i64>);
    let (year, month, day) = (
        date_parts.next()?.ok()?,
        date_parts.next()?.ok()?,
        date_parts.next()?.ok()?,
    );
    if date_parts.next().is_some() {
        return None;
    }
    let (clock, offset) = if let Some(clock) = rest.strip_suffix('Z') {
        (clock, 0)
    } else {
        let split = rest.rfind(['+', '-'])?;
        let (clock, zone) = rest.split_at(split);
        let sign = if zone.starts_with('-') { -1 } else { 1 };
        let (hours, minutes) = zone[1..].split_once(':')?;
        let (hours, minutes) = (hours.parse::<i64>().ok()?, minutes.parse::<i64>().ok()?);
        if !(0..=23).contains(&hours) || !(0..=59).contains(&minutes) {
            return None;
        }
        (clock, sign * (hours * 3600 + minutes * 60))
    };
    let clock = if let Some((clock, fraction)) = clock.split_once('.') {
        if fraction.is_empty() || !fraction.bytes().all(|byte| byte.is_ascii_digit()) {
            return None;
        }
        clock
    } else {
        clock
    };
    let mut clock_parts = clock.split(':').map(str::parse::<i64>);
    let (hour, minute, second) = (
        clock_parts.next()?.ok()?,
        clock_parts.next()?.ok()?,
        clock_parts.next()?.ok()?,
    );
    if clock_parts.next().is_some()
        || !(0..=9999).contains(&year)
        || !(0..=23).contains(&hour)
        || !(0..=59).contains(&minute)
        || !(0..=59).contains(&second)
        || !(1..=12).contains(&month)
        || !(1..=31).contains(&day)
    {
        return None;
    }
    let leap = year % 4 == 0 && (year % 100 != 0 || year % 400 == 0);
    let days = [
        31,
        if leap { 29 } else { 28 },
        31,
        30,
        31,
        30,
        31,
        31,
        30,
        31,
        30,
        31,
    ];
    if day > days[(month - 1) as usize] {
        return None;
    }
    Some(days_from_civil(year, month, day) * 86_400 + hour * 3600 + minute * 60 + second - offset)
}

/// Days since 1970-01-01 (Howard Hinnant's algorithm).
fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
    let year = if month <= 2 { year - 1 } else { year };
    let era = year.div_euclid(400);
    let year_of_era = year - era * 400;
    let month_index = (month + 9) % 12;
    let day_of_year = (153 * month_index + 2) / 5 + day - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    era * 146_097 + day_of_era - 719_468
}

// ------------------------------------------------------------ request

#[derive(Deserialize)]
struct Credentials {
    #[serde(rename = "claudeAiOauth")]
    oauth: Option<OAuth>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct OAuth {
    access_token: String,
    /// Milliseconds.
    expires_at: Option<i64>,
}

enum QueryError {
    State(ClaudeResets),
    Limited(Option<i64>),
}

impl From<String> for QueryError {
    fn from(error: String) -> Self {
        Self::State(ClaudeResets::Failed(error))
    }
}

/// Directory choice is resolved before touching any credentials. There is
/// deliberately no guessed service name for an alternative macOS profile.
struct CredentialSource {
    dir: PathBuf,
    alternative: bool,
    macos: bool,
}

impl CredentialSource {
    fn current() -> Result<Self, String> {
        if std::env::var_os("CLAUDE_SECURESTORAGE_CONFIG_DIR").is_some() {
            return Err("unsupported Claude secure-storage configuration".into());
        }
        let alternative = std::env::var_os("CLAUDE_CONFIG_DIR");
        let dir = match &alternative {
            Some(dir) if !dir.is_empty() => PathBuf::from(dir),
            Some(_) => return Err("CLAUDE_CONFIG_DIR is empty".into()),
            None => std::env::var_os("HOME")
                .filter(|home| !home.is_empty())
                .map(|home| PathBuf::from(home).join(".claude"))
                .ok_or("cannot resolve Claude Code configuration")?,
        };
        if !dir.is_absolute() {
            return Err("CLAUDE_CONFIG_DIR must be absolute".into());
        }
        Ok(Self {
            dir,
            alternative: alternative.is_some(),
            macos: cfg!(target_os = "macos"),
        })
    }

    fn ensure_supported(&self) -> Result<(), String> {
        if self.macos && self.alternative {
            return Err("cannot determine Keychain entry for CLAUDE_CONFIG_DIR".into());
        }
        Ok(())
    }

    fn scope(&self) -> String {
        crate::paths::cache_scope(
            &format!("claude-typed-v1:{}:{}", self.macos, self.alternative),
            &self.dir,
        )
        .expect("absolute credential directory")
    }

    fn raw(&self, timeout: Duration) -> Result<String, String> {
        self.ensure_supported()?;
        let mut inaccessible = false;
        if self.macos {
            let process = crate::process::Process::spawn(
                crate::access::command("security").args([
                    "find-generic-password",
                    "-s",
                    "Claude Code-credentials",
                    "-w",
                ]),
                timeout,
            );
            match process.and_then(|process| process.finish()) {
                Ok((status, bytes)) if status.success() => {
                    return String::from_utf8(bytes)
                        .map_err(|_| "unreadable Claude Code credentials".into());
                }
                Ok((status, _)) if status.code() == Some(44) => {} // item not found
                _ => inaccessible = true,
            }
        }
        let file = crate::access::open(self.dir.join(".credentials.json"));
        match file {
            Ok(file) => {
                let mut bytes = Vec::new();
                file.take(65537)
                    .read_to_end(&mut bytes)
                    .map_err(|_| "cannot read Claude Code credentials")?;
                if bytes.len() > 65536 {
                    return Err("unreadable Claude Code credentials".into());
                }
                String::from_utf8(bytes).map_err(|_| "unreadable Claude Code credentials".into())
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound && !inaccessible => {
                Err("no Claude Code sign-in found".into())
            }
            Err(_) => Err("cannot access Claude Code sign-in".into()),
        }
    }
}

fn token(source: &CredentialSource, now: i64, timeout: Duration) -> Result<String, String> {
    let raw = source.raw(timeout)?;
    let credentials: Credentials =
        serde_json::from_str(&raw).map_err(|_| "unreadable Claude Code credentials")?;
    let oauth = credentials.oauth.ok_or("no Claude Code sign-in found")?;
    if oauth.expires_at.is_some_and(|at| at / 1000 <= now) {
        return Err("sign-in expired · open Claude Code".into());
    }
    if oauth.access_token.is_empty()
        || oauth.access_token.len() > 16384
        || !oauth
            .access_token
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"-._~+/=".contains(&byte))
    {
        return Err("unreadable Claude Code credentials".into());
    }
    Ok(oauth.access_token)
}

fn query(source: &CredentialSource, now: i64, timeout: Duration) -> Result<Value, QueryError> {
    let token = token(source, now, timeout)
        .map_err(|error| QueryError::State(ClaudeResets::Blocked(error)))?;
    let version = claude_version(timeout).ok_or_else(|| {
        QueryError::State(ClaudeResets::Failed(
            "cannot find Claude Code's version".into(),
        ))
    })?;
    // Preserve the existing URL, parameters, headers and user-agent verbatim.
    let config = format!(
        "url = \"{URL}\"\nheader = \"Authorization: Bearer {token}\"\nheader = \"anthropic-beta: oauth-2025-04-20\"\nuser-agent = \"claude-cli/{version} (external, cli)\"\n"
    );
    let output = crate::process::output(
        crate::access::command("curl").args([
            "-q",
            "-sS",
            "--max-time",
            "10",
            "--connect-timeout",
            "10",
            "--proto",
            "=https",
            "-K",
            "-",
            "--dump-header",
            "-",
            "-w",
            "\n%{http_code}",
        ]),
        config.as_bytes(),
        timeout,
    )?;
    response(&output, now)
}

fn response(output: &[u8], now: i64) -> Result<Value, QueryError> {
    let malformed = || QueryError::State(ClaudeResets::Failed("unexpected answer format".into()));
    let text = std::str::from_utf8(output)
        .map_err(|_| malformed())?
        .replace("\r\n", "\n");
    let (mut body, code) = text.rsplit_once('\n').ok_or_else(malformed)?;
    if code.len() != 3 || !code.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err(malformed());
    }
    let mut headers = "";
    // Proxy CONNECT and informational blocks precede the final headers.
    while body.starts_with("HTTP/") {
        (headers, body) = body.split_once("\n\n").ok_or_else(malformed)?;
    }
    if headers.is_empty()
        || headers
            .lines()
            .next()
            .and_then(|line| line.split_whitespace().nth(1))
            != Some(code)
    {
        return Err(malformed());
    }
    match code {
        "200" => {}
        "401" => {
            return Err(QueryError::State(ClaudeResets::Blocked(
                "sign-in rejected · open Claude Code".into(),
            )));
        }
        "403" => {
            return Err(QueryError::State(ClaudeResets::Blocked(
                "access forbidden · open Claude Code".into(),
            )));
        }
        "429" => {
            let retry_at = headers
                .lines()
                .filter_map(|line| line.split_once(':'))
                .filter(|(key, _)| key.eq_ignore_ascii_case("Retry-After"))
                .filter_map(|(_, value)| retry_after(value.trim(), now))
                .max();
            return Err(QueryError::Limited(retry_at));
        }
        _ => {
            return Err(QueryError::State(ClaudeResets::Failed(format!(
                "HTTP {code}"
            ))));
        }
    }
    let value: Value = serde_json::from_str(body).map_err(|_| malformed())?;
    value
        .get("cedar_ember")
        .filter(|value| value.is_object())
        .cloned()
        .ok_or_else(|| QueryError::State(ClaudeResets::Failed("answer has no resets".into())))
}

fn retry_after(value: &str, now: i64) -> Option<i64> {
    if !value.is_empty() && value.bytes().all(|byte| byte.is_ascii_digit()) {
        return value
            .parse::<u64>()
            .ok()
            .map(|delay| now.saturating_add(i64::try_from(delay).unwrap_or(i64::MAX)));
    }
    let date = httpdate::parse_http_date(value)
        .ok()?
        .duration_since(UNIX_EPOCH)
        .ok()?;
    Some(i64::try_from(date.as_secs()).ok()?.max(now))
}

/// Keep the existing version selection, but bound the subprocess fallback.
fn claude_version(timeout: Duration) -> Option<String> {
    let home = PathBuf::from(std::env::var_os("HOME")?);
    let installed = crate::access::read_dir(home.join(".local/share/claude/versions"))
        .ok()
        .and_then(|entries| {
            entries
                .flatten()
                .filter_map(|entry| entry.file_name().into_string().ok())
                .filter_map(|name| version_key(&name).map(|key| (key, name)))
                .max()
                .map(|(_, name)| name)
        });
    installed.or_else(|| {
        let output = crate::process::output(
            crate::access::command("claude").arg("--version"),
            &[],
            timeout,
        )
        .ok()?;
        let text = std::str::from_utf8(&output).ok()?;
        let version = text.split_whitespace().next()?.to_owned();
        version_key(&version).map(|_| version)
    })
}

fn version_key(text: &str) -> Option<(u64, u64, u64)> {
    let mut parts = text.split('.').map(str::parse::<u64>);
    let key = (
        parts.next()?.ok()?,
        parts.next()?.ok()?,
        parts.next()?.ok()?,
    );
    parts.next().is_none().then_some(key)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const NOW: i64 = 1_794_400_000; // 2026-11-11

    fn grant(left: u64, ends: &str, clears: &[&str], usable: bool) -> Value {
        json!({"resets_left": left, "ends_at": ends, "clears": clears,
               "paused": false, "usable_now": usable, "unavailable_reason": null})
    }

    #[test]
    fn iso_dates() {
        assert_eq!(parse_iso("1970-01-01T00:00:00+00:00"), Some(0));
        assert_eq!(parse_iso("2026-12-10T22:00:00+00:00"), Some(1_796_940_000));
        assert_eq!(
            parse_iso("2026-11-13T15:00:00.183317+00:00"),
            Some(1_794_582_000)
        );
        assert_eq!(parse_iso("2026-11-13T12:00:00-03:00"), Some(1_794_582_000));
        assert_eq!(parse_iso("2026-11-13T15:00:00Z"), Some(1_794_582_000));
        assert_eq!(parse_iso("nope"), None);
    }

    #[test]
    fn groups_by_kind_and_expiry() {
        let fable = ["five_hour", "seven_day_overage_included"];
        let value = json!({"eligible": true, "cooldown_until": null, "grants": [
            grant(0, "2027-03-09T07:59:00+00:00", &fable, false),
            grant(1, "2027-03-09T07:59:00+00:00", &fable, true),
            grant(1, "2027-03-09T07:59:00+00:00", &fable, true),
            grant(1, "2026-12-10T22:00:00+00:00",
                  &["five_hour", "seven_day", "seven_day_overage_included"], true),
        ]});
        let ClaudeResets::Known { groups, .. } = parse(&value, NOW) else {
            panic!("expected known");
        };
        assert_eq!(groups.len(), 2);
        assert_eq!((groups[0].kind, groups[0].count), (Kind::Full, 1));
        assert_eq!((groups[1].kind, groups[1].count), (Kind::Fable, 2));
    }

    #[test]
    fn ineligible_and_malformed() {
        assert_eq!(
            parse(&json!({"eligible": false, "grants": []}), NOW),
            ClaudeResets::Unavailable
        );
        assert!(matches!(
            parse(&json!({"grants": []}), NOW),
            ClaudeResets::Failed(_)
        ));
        assert!(matches!(
            parse(
                &json!({"eligible": true, "grants": [grant(1, "soon", &[], true)]}),
                NOW
            ),
            ClaudeResets::Failed(_)
        ));
        assert_eq!(
            parse(&json!({"eligible": true, "grants": []}), NOW),
            ClaudeResets::Known {
                groups: vec![],
                cooldown_until: None
            }
        );
    }

    #[test]
    fn blocked_and_expired() {
        let value = json!({"eligible": true, "cooldown_until": "2026-11-12T12:00:00Z", "grants": [
            grant(1, "2026-01-01T00:00:00Z", &["five_hour"], true),
            {"resets_left": 1, "ends_at": null, "clears": ["five_hour"],
             "usable_now": false, "unavailable_reason": "blocked_five_hour"},
        ]});
        let ClaudeResets::Known {
            groups,
            cooldown_until,
        } = parse(&value, NOW)
        else {
            panic!("expected known");
        };
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].kind, Kind::Session);
        assert_eq!(groups[0].expires_at, None);
        assert_eq!(groups[0].blocked.as_deref(), Some("can't use right now"));
        assert!(cooldown_until.is_some());
    }

    #[test]
    fn versions() {
        assert_eq!(version_key("2.1.322"), Some((2, 1, 322)));
        assert!(version_key("2.1.322") > version_key("2.1.99"));
        assert_eq!(version_key("latest"), None);
    }
}

#[cfg(test)]
#[path = "../tests/support/claude_robustness.rs"]
mod robustness;
