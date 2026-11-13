use super::*;
use crate::robustness_support::{executable, isolated, root};
use std::fs;
use std::os::unix::fs::PermissionsExt;

const NOW: i64 = 1_794_484_800;
const SECRET: &str = "synthetic-secret-token";
// Successful/error-response fixtures check semantics, not process startup speed.
const RESPONSE_TIMEOUT: Duration = crate::test_support::EVENT_TIMEOUT;
const STALL_TIMEOUT: Duration = Duration::from_secs(1);

fn profile(macos: bool, alternative: bool) -> CredentialSource {
    CredentialSource {
        dir: root().join(if alternative {
            "alternative"
        } else {
            ".claude"
        }),
        macos,
        alternative,
    }
}

fn credentials(source: &CredentialSource, expires: Option<i64>) {
    fs::create_dir_all(&source.dir).unwrap();
    fs::write(source.dir.join(".credentials.json"), serde_json::to_vec(&serde_json::json!({
        "claudeAiOauth": {"accessToken": SECRET, "expiresAt": expires, "refreshToken": "never-refresh-this"}
    })).unwrap()).unwrap();
}

fn http(code: u16, headers: &str, body: &str) {
    fs::write(
        root().join("http-response"),
        format!("HTTP/1.1 {code} Fixture\r\n{headers}\r\n{body}\n{code}"),
    )
    .unwrap();
}

fn setup() {
    executable("claude", "#!/bin/sh\nprintf '2.1.0 (Claude Code)\\n'\n");
    executable(
        "curl",
        r##"#!/bin/sh
printf '%s\n' "$@" > "$HOME/curl-argv"
IFS= read -r url
IFS= read -r auth
IFS= read -r beta
IFS= read -r ua
[ "$url" = 'url = "https://api.anthropic.com/api/oauth/usage?cedar_ember=1&skip_spend=1"' ] || exit 90
[ "$auth" = 'header = "Authorization: Bearer synthetic-secret-token"' ] || exit 91
[ "$beta" = 'header = "anthropic-beta: oauth-2025-04-20"' ] || exit 92
[ "$ua" = 'user-agent = "claude-cli/2.1.0 (external, cli)"' ] || exit 93
/bin/cat > /dev/null
printf 'x' >> "$HOME/curl-calls"
/bin/cat "$HOME/http-response"
"##,
    );
}

fn calls() -> usize {
    fs::read(root().join("curl-calls"))
        .unwrap_or_default()
        .len()
}
fn clear_cache() {
    let _ = fs::remove_dir_all(root().join("cache"));
}
fn good() -> &'static str {
    r#"{"cedar_ember":{"eligible":true,"grants":[{"resets_left":2,"ends_at":null,"clears":["five_hour"]}],"access_token":"synthetic-secret-token"},"access_token":"synthetic-secret-token"}"#
}

#[test]
fn credentials_missing_expired_inaccessible_and_alternative_never_fall_back() {
    if !isolated(
        "claude_resets::robustness::credentials_missing_expired_inaccessible_and_alternative_never_fall_back",
    ) {
        return;
    }
    let default = profile(false, false);
    let alternative = profile(false, true);
    credentials(&default, None);
    crate::access::start();
    assert_eq!(
        token(&alternative, NOW, RESPONSE_TIMEOUT).unwrap_err(),
        "no Claude Code sign-in found"
    );
    let events = crate::access::take();
    assert!(
        events
            .iter()
            .all(|event| event.target.starts_with(&alternative.dir))
    );
    credentials(&alternative, Some(NOW * 1000));
    assert!(
        token(&alternative, NOW, RESPONSE_TIMEOUT)
            .unwrap_err()
            .contains("expired")
    );
    fs::remove_file(alternative.dir.join(".credentials.json")).unwrap();
    fs::create_dir(alternative.dir.join(".credentials.json")).unwrap();
    assert!(
        token(&alternative, NOW, RESPONSE_TIMEOUT)
            .unwrap_err()
            .contains("cannot read")
    );
    fs::remove_dir(alternative.dir.join(".credentials.json")).unwrap();
    for malicious in ["token\r\nheader=bad", "token\"", "token\\", "token\0", ""] {
        fs::write(
            alternative.dir.join(".credentials.json"),
            serde_json::to_vec(&serde_json::json!({"claudeAiOauth":{"accessToken":malicious}}))
                .unwrap(),
        )
        .unwrap();
        assert_eq!(
            token(&alternative, NOW, RESPONSE_TIMEOUT).unwrap_err(),
            "unreadable Claude Code credentials"
        );
    }
    fs::write(
        alternative.dir.join(".credentials.json"),
        b"malformed secret payload",
    )
    .unwrap();
    assert_eq!(
        token(&alternative, NOW, RESPONSE_TIMEOUT).unwrap_err(),
        "unreadable Claude Code credentials"
    );
    credentials(&alternative, Some((NOW + 600) * 1000));
    assert_eq!(token(&alternative, NOW, RESPONSE_TIMEOUT).unwrap(), SECRET);
}

#[test]
fn unknown_keychain_mapping_fails_before_any_access_and_default_is_exact() {
    if !isolated(
        "claude_resets::robustness::unknown_keychain_mapping_fails_before_any_access_and_default_is_exact",
    ) {
        return;
    }
    let alternative = profile(true, true);
    credentials(&alternative, None);
    crate::access::start();
    let saved = load_from(&alternative, NOW, RESPONSE_TIMEOUT);
    assert!(crate::access::take().is_empty());
    let text = crate::popup_theme::PopupTheme::default().claude(None, Some(&saved), NOW);
    assert!(text.contains("cannot determine Keychain entry for CLAUDE_CONFIG_DIR"));
    assert!(!text.contains(SECRET));
    let default = profile(true, false);
    fs::write(
        root().join("keychain-fixture"),
        serde_json::to_vec(&serde_json::json!({"claudeAiOauth":{"accessToken":SECRET}})).unwrap(),
    )
    .unwrap();
    executable(
        "security",
        "#!/bin/sh\nprintf '%s\\n' \"$@\" > \"$HOME/security-argv\"\n/bin/cat \"$HOME/keychain-fixture\"\n",
    );
    assert_eq!(token(&default, NOW, RESPONSE_TIMEOUT).unwrap(), SECRET);
    let argv = fs::read_to_string(root().join("security-argv")).unwrap();
    assert_eq!(
        argv,
        "find-generic-password\n-s\nClaude Code-credentials\n-w\n"
    );
    assert!(!argv.contains(SECRET));
    executable("security", "#!/bin/sh\nexit 44\n");
    assert!(
        token(&default, NOW, RESPONSE_TIMEOUT)
            .unwrap_err()
            .contains("no Claude Code sign-in")
    );
    executable("security", "#!/bin/sh\nexit 1\n");
    assert!(
        token(&default, NOW, RESPONSE_TIMEOUT)
            .unwrap_err()
            .contains("cannot access")
    );
    credentials(&default, None); // official file fallback stays in the same configuration
    assert_eq!(token(&default, NOW, RESPONSE_TIMEOUT).unwrap(), SECRET);
}

#[test]
fn http_failures_never_become_zero_or_cache_a_secret_and_local_usage_survives() {
    if !isolated(
        "claude_resets::robustness::http_failures_never_become_zero_or_cache_a_secret_and_local_usage_survives",
    ) {
        return;
    }
    let source = profile(false, false);
    credentials(&source, None);
    setup();
    let local = (
        crate::claude::Limits {
            five_hour: Some(crate::claude::Window {
                used_percentage: 7.0,
                resets_at: (NOW + 3600) as f64,
            }),
            seven_day: None,
        },
        NOW,
    );
    for (code, body, expected) in [
        (401, SECRET, "sign-in rejected"),
        (403, SECRET, "access forbidden"),
        (500, SECRET, "HTTP 500"),
        (200, "malformed", "unexpected answer format"),
        (200, "{}", "answer has no resets"),
        (
            200,
            r#"{"cedar_ember":{"eligible":"yes"}}"#,
            "unexpected answer format",
        ),
    ] {
        clear_cache();
        http(code, "", body);
        let saved = load_from(&source, NOW, RESPONSE_TIMEOUT);
        assert!(matches!(
            saved,
            ClaudeResets::Failed(_) | ClaudeResets::Blocked(_)
        ));
        assert!(
            format!("{saved:?}").contains(expected),
            "expected {expected}: {saved:?}"
        );
        assert!(!format!("{saved:?}").contains(SECRET));
        let text =
            crate::popup_theme::PopupTheme::default().claude(Some(&local), Some(&saved), NOW);
        assert!(text.contains("7%") && text.contains("couldn't check"));
        assert!(!text.contains(SECRET));
        assert!(
            !root()
                .join("cache/tmux-agent-usage/claude-resets.json")
                .exists()
        );
    }
    for bad in [
        b"HTTP/1.1 200 OK\n\n{}\n200\x1b[2J".as_slice(),
        b"no headers\n200",
        b"\xff\n200",
    ] {
        fs::write(root().join("http-response"), bad).unwrap();
        assert!(matches!(
            load_from(&source, NOW, RESPONSE_TIMEOUT),
            ClaudeResets::Failed(_)
        ));
    }
    assert!(
        !fs::read_to_string(root().join("curl-argv"))
            .unwrap()
            .contains(SECRET)
    );
    assert!(
        fs::read_to_string(root().join("curl-argv"))
            .unwrap()
            .starts_with("-q\n")
    );
}

#[test]
fn valid_cache_has_five_minute_ttl_is_private_scoped_and_allowlisted() {
    if !isolated(
        "claude_resets::robustness::valid_cache_has_five_minute_ttl_is_private_scoped_and_allowlisted",
    ) {
        return;
    }
    let source = profile(false, false);
    credentials(&source, None);
    setup();
    http(200, "", good());
    // This is an isolated, single-test process. Restore umask immediately.
    let old = unsafe { libc::umask(0) };
    let saved = load_from(&source, NOW, RESPONSE_TIMEOUT);
    unsafe { libc::umask(old) };
    assert!(matches!(saved, ClaudeResets::Known { .. }), "{saved:?}");
    assert_eq!(calls(), 1);
    assert_eq!(load_from(&source, NOW + 299, RESPONSE_TIMEOUT), saved);
    assert_eq!(calls(), 1);
    assert_eq!(load_from(&source, NOW + 300, RESPONSE_TIMEOUT), saved);
    assert_eq!(calls(), 2);
    let cache = root().join("cache/tmux-agent-usage");
    assert_eq!(
        fs::metadata(&cache).unwrap().permissions().mode() & 0o777,
        0o700
    );
    for file in fs::read_dir(&cache).unwrap() {
        let file = file.unwrap();
        assert_eq!(file.metadata().unwrap().permissions().mode() & 0o777, 0o600);
        let text = fs::read_to_string(file.path()).unwrap();
        assert!(
            !text.contains(SECRET)
                && !text.contains("access_token")
                && !text.contains("refreshToken")
        );
    }
    let other = profile(false, true);
    credentials(&other, None);
    load_from(&other, NOW + 300, RESPONSE_TIMEOUT);
    assert_eq!(calls(), 3);
    // A malformed successful body is never reused as a valid cache hit.
    clear_cache();
    http(
        200,
        "",
        r#"{"cedar_ember":{"eligible":true,"grants":[{"resets_left":1,"ends_at":"bad date"}]}}"#,
    );
    assert!(matches!(
        load_from(&source, NOW, RESPONSE_TIMEOUT),
        ClaudeResets::Failed(_)
    ));
    assert!(matches!(
        load_from(&source, NOW, RESPONSE_TIMEOUT),
        ClaudeResets::Failed(_)
    ));
    assert_eq!(calls(), 5);
}

#[test]
fn retry_after_seconds_http_date_and_exponential_backoff_survive_reopening() {
    if !isolated(
        "claude_resets::robustness::retry_after_seconds_http_date_and_exponential_backoff_survive_reopening",
    ) {
        return;
    }
    let source = profile(false, false);
    credentials(&source, None);
    setup();
    for retry in [
        "600".to_owned(),
        httpdate::fmt_http_date(UNIX_EPOCH + Duration::from_secs((NOW + 600) as u64)),
    ] {
        clear_cache();
        let before = calls();
        http(429, &format!("rEtRy-AfTeR: {retry}\r\n"), SECRET);
        assert!(matches!(
            load_from(&source, NOW, RESPONSE_TIMEOUT),
            ClaudeResets::Blocked(_)
        ));
        crate::access::start();
        load_from(&source, NOW + 599, RESPONSE_TIMEOUT);
        let events = crate::access::take();
        assert!(
            events
                .iter()
                .all(|event| event.operation != "command" && event.operation != "open")
        );
        assert_eq!(calls(), before + 1);
        http(429, "Retry-After: malformed\r\n", SECRET);
        load_from(&source, NOW + 600, RESPONSE_TIMEOUT);
        assert_eq!(calls(), before + 2);
        load_from(&source, NOW + 719, RESPONSE_TIMEOUT);
        assert_eq!(calls(), before + 2);
        http(200, "", good());
        load_from(&source, NOW + 720, RESPONSE_TIMEOUT);
        assert_eq!(calls(), before + 3);
    }
    clear_cache();
    let before = calls();
    http(429, "", SECRET);
    load_from(&source, NOW, RESPONSE_TIMEOUT);
    load_from(&source, NOW + 59, RESPONSE_TIMEOUT);
    assert_eq!(calls(), before + 1);
    http(200, "", good());
    load_from(&source, NOW + 60, RESPONSE_TIMEOUT);
    assert_eq!(calls(), before + 2);
}

#[test]
fn query_helpers_timeout_without_leaking_or_refreshing_credentials() {
    if !isolated(
        "claude_resets::robustness::query_helpers_timeout_without_leaking_or_refreshing_credentials",
    ) {
        return;
    }
    let source = profile(false, false);
    credentials(&source, None);
    setup();
    let before = fs::read(source.dir.join(".credentials.json")).unwrap();
    for helper in ["curl", "claude", "security"] {
        setup();
        executable(
            helper,
            "#!/bin/sh\nprintf '%s' \"$$\" > \"$HOME/helper-pid\"\nexec /bin/sleep 300\n",
        );
        let macos = helper == "security";
        let source = CredentialSource {
            macos,
            ..profile(false, false)
        };
        if macos {
            fs::remove_file(source.dir.join(".credentials.json")).unwrap();
        }
        let saved = load_from(&source, NOW, STALL_TIMEOUT);
        assert!(matches!(
            saved,
            ClaudeResets::Failed(_) | ClaudeResets::Blocked(_)
        ));
        assert!(!format!("{saved:?}").contains(SECRET));
        assert!(
            root().join("helper-pid").exists(),
            "helper={helper}, saved={saved:?}"
        );
        let pid: libc::pid_t = fs::read_to_string(root().join("helper-pid"))
            .unwrap()
            .parse()
            .unwrap();
        let mut status = 0;
        assert_eq!(
            unsafe { libc::waitpid(pid, &mut status, libc::WNOHANG) },
            -1
        );
        assert_eq!(
            std::io::Error::last_os_error().raw_os_error(),
            Some(libc::ECHILD)
        );
        assert_eq!(unsafe { libc::kill(pid, 0) }, -1);
        if macos {
            credentials(&source, None);
        }
    }
    assert_eq!(
        fs::read(source.dir.join(".credentials.json")).unwrap(),
        before
    );
}

#[test]
fn cached_expiry_is_rechecked_and_invalid_dates_counts_and_cooldowns_fail() {
    let cached_value = serde_json::to_value(ClaudeResets::Known {
        groups: vec![Group {
            kind: Kind::Session,
            count: 1,
            expires_at: Some(NOW + 60),
            blocked: None,
        }],
        cooldown_until: Some(NOW + 30),
    })
    .unwrap();
    assert!(
        matches!(cached(cached_value.clone(), NOW + 59), Some(ClaudeResets::Known { groups, cooldown_until: None }) if groups.len() == 1)
    );
    assert!(
        matches!(cached(cached_value, NOW + 60), Some(ClaudeResets::Known { groups, cooldown_until: None }) if groups.is_empty())
    );
    for date in [
        "2026-02-30T12:00:00Z",
        "2026-01-01T24:00:00Z",
        "2026-01-01T12:00:00.badZ",
        "2026-01-01T12:00:00+99:00",
    ] {
        assert!(parse_iso(date).is_none());
    }
    assert!(matches!(
        parse(
            &serde_json::json!({"eligible":true,"cooldown_until":"bad"}),
            NOW
        ),
        ClaudeResets::Failed(_)
    ));
    assert!(matches!(
        parse(
            &serde_json::json!({"eligible":true,"grants":[{"resets_left":u64::MAX},{"resets_left":1}]}),
            NOW
        ),
        ClaudeResets::Failed(_)
    ));
}

#[test]
fn eligible_response_without_grants_is_unknown_instead_of_zero() {
    assert!(matches!(
        parse(&serde_json::json!({"eligible":true}), NOW),
        ClaudeResets::Failed(_)
    ));
    assert!(matches!(
        parse(&serde_json::json!({"eligible":true,"grants":null}), NOW),
        ClaudeResets::Failed(_)
    ));
    assert_eq!(
        parse(&serde_json::json!({"eligible":false}), NOW),
        ClaudeResets::Unavailable
    );
}

#[test]
fn snapshot_and_query_writes_stay_private_and_refuse_symlink_directories() {
    if !isolated(
        "claude_resets::robustness::snapshot_and_query_writes_stay_private_and_refuse_symlink_directories",
    ) {
        return;
    }
    let old = unsafe { libc::umask(0) };
    crate::claude::record(include_bytes!("../fixtures/baseline/tap.json"), NOW).unwrap();
    unsafe { libc::umask(old) };
    let cache = root().join("cache/tmux-agent-usage");
    for dir in [&cache, &cache.join("claude")] {
        assert_eq!(
            fs::metadata(dir).unwrap().permissions().mode() & 0o777,
            0o700
        );
    }
    assert_eq!(
        fs::metadata(cache.join("claude/fixture.json"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o600
    );
    let external = root().join("fixture-target");
    fs::create_dir(&external).unwrap();
    let link = cache.join("symlink-fixture");
    std::os::unix::fs::symlink(&external, &link).unwrap();
    assert!(crate::access::write_private(&link.join("answer.json"), b"fixture").is_err());
    assert!(!external.join("answer.json").exists());
    let answer = cache.join("old.json");
    fs::write(&answer, b"old").unwrap();
    fs::set_permissions(&answer, fs::Permissions::from_mode(0o666)).unwrap();
    crate::access::write_private(&answer, b"new").unwrap();
    assert_eq!(
        fs::metadata(answer).unwrap().permissions().mode() & 0o777,
        0o600
    );
}
