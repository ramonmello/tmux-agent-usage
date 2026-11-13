use super::*;
use crate::robustness_support::{executable, isolated, root};
use std::fs;

const RESPONSE_TIMEOUT: Duration = crate::test_support::EVENT_TIMEOUT;

fn reaped(pid: libc::pid_t) {
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
    assert_eq!(
        std::io::Error::last_os_error().raw_os_error(),
        Some(libc::ESRCH)
    );
}

fn setup(suffix: &str) {
    executable(
        "codex",
        &format!(
            r##"#!/bin/sh
printf '%s' "$$" > "$HOME/codex-pid"
printf '%s\n' "$@" > "$HOME/codex-argv"
IFS= read -r initialize
printf '%s\n' "$initialize" > "$HOME/rpc-requests"
printf '{{"id":99,"method":"account/chatgptAuthTokens/refresh","params":{{}}}}\n'
printf '{{"id":1,"result":{{"userAgent":"fixture"}}}}\n'
IFS= read -r initialized
IFS= read -r request
printf '%s\n%s\n' "$initialized" "$request" >> "$HOME/rpc-requests"
{suffix}
exec /bin/sleep 300
"##
        ),
    );
}

fn pid() -> libc::pid_t {
    fs::read_to_string(root().join("codex-pid"))
        .unwrap()
        .parse()
        .unwrap()
}

#[test]
fn official_stdio_contract_only_sends_initialize_and_read_and_always_reaps() {
    if !isolated(
        "resets::robustness::official_stdio_contract_only_sends_initialize_and_read_and_always_reaps",
    ) {
        return;
    }
    setup("/bin/cat \"$HOME/codex-response\"");
    for (response, expected) in [
        (
            r#"{"id":2,"result":{"rateLimitResetCredits":{"availableCount":2,"credits":null}}}"#,
            SavedResets::CountOnly { count: 2 },
        ),
        (
            r#"{"id":2,"result":{"rateLimits":{"primary":null}}}"#,
            SavedResets::Unavailable,
        ),
        (
            r#"{"id":2,"result":{"rateLimitResetCredits":null}}"#,
            SavedResets::Unavailable,
        ),
        (
            r#"{"id":2,"result":{"rateLimitResetCredits":{"availableCount":0,"credits":[]}}}"#,
            SavedResets::Known {
                count: 0,
                credits: vec![],
            },
        ),
    ] {
        fs::write(root().join("codex-response"), format!("{response}\n")).unwrap();
        assert_eq!(parse(&query(RESPONSE_TIMEOUT).unwrap()), expected);
        reaped(pid());
        assert_eq!(
            fs::read_to_string(root().join("codex-argv")).unwrap(),
            "app-server\n--listen\nstdio://\n"
        );
        let requests: Vec<Value> = fs::read_to_string(root().join("rpc-requests"))
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        assert_eq!(
            requests
                .iter()
                .map(|request| request["method"].as_str().unwrap())
                .collect::<Vec<_>>(),
            ["initialize", "initialized", "account/rateLimits/read"]
        );
    }
}

#[test]
fn codex_timeout_early_exit_malformed_and_provider_errors_reap_and_redact() {
    if !isolated(
        "resets::robustness::codex_timeout_early_exit_malformed_and_provider_errors_reap_and_redact",
    ) {
        return;
    }
    for (suffix, timeout) in [
        ("exit 0", RESPONSE_TIMEOUT),
        ("printf 'malformed\\n'", RESPONSE_TIMEOUT),
        (
            "printf '{\"id\":2,\"error\":{\"message\":\"secret-fixture-token\\\\u001b[2J\"}}}\\n'",
            RESPONSE_TIMEOUT,
        ),
        (":", Duration::from_secs(1)),
    ] {
        setup(suffix);
        let error = query(timeout).unwrap_err();
        assert!(!error.contains("secret-fixture-token") && !error.contains('\x1b'));
        assert!(
            root().join("codex-pid").exists(),
            "suffix={suffix}, error={error}, PATH={:?}",
            std::env::var_os("PATH")
        );
        reaped(pid());
    }
    executable(
        "codex",
        "#!/bin/sh\nprintf '%s' \"$$\" > \"$HOME/codex-pid\"\nexec /bin/sleep 300\n",
    );
    assert!(
        query(Duration::from_secs(1))
            .unwrap_err()
            .contains("too long")
    );
    reaped(pid());
}

#[test]
fn helper_backpressure_and_oversized_output_are_bounded_and_reaped() {
    if !isolated(
        "resets::robustness::helper_backpressure_and_oversized_output_are_bounded_and_reaped",
    ) {
        return;
    }
    executable("blocked", "#!/bin/sh\nexec /bin/sleep 300\n");
    let mut process = crate::process::Process::spawn(
        &mut crate::access::command("blocked"),
        Duration::from_secs(1),
    )
    .unwrap();
    let id = process.id();
    assert!(
        process
            .write(&vec![b'x'; 2 * 1024 * 1024])
            .unwrap_err()
            .contains("too long")
    );
    drop(process);
    reaped(id);
    executable(
        "flood",
        "#!/bin/sh\npayload=xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx\nfor step in 1 2 3 4 5 6 7 8; do payload=$payload$payload; done\nwhile :; do printf '%s' \"$payload\"; done\n",
    );
    let mut process =
        crate::process::Process::spawn(&mut crate::access::command("flood"), RESPONSE_TIMEOUT)
            .unwrap();
    let id = process.id();
    assert!(process.line().unwrap_err().contains("too large"));
    drop(process);
    reaped(id);
}

#[test]
fn typed_codex_cache_discards_opaque_fields_and_is_scoped_to_codex_home() {
    if !isolated(
        "resets::robustness::typed_codex_cache_discards_opaque_fields_and_is_scoped_to_codex_home",
    ) {
        return;
    }
    setup("/bin/cat \"$HOME/codex-response\"");
    fs::write(root().join("codex-response"), "{\"id\":2,\"result\":{\"rateLimitResetCredits\":{\"availableCount\":1,\"credits\":[{\"status\":\"available\",\"expiresAt\":null,\"title\":\"secret-fixture-token\",\"accessToken\":\"secret-fixture-token\"}],\"accessToken\":\"secret-fixture-token\"}}}\n").unwrap();
    assert!(matches!(load(1000), SavedResets::Known { count: 1, .. }));
    reaped(pid());
    let cached =
        fs::read_to_string(root().join("cache/tmux-agent-usage/codex-resets.json")).unwrap();
    assert!(!cached.contains("secret-fixture-token") && !cached.contains("accessToken"));
    assert!(crate::cache::fresh(CACHE, "different-codex-home", 1001).is_none());
}

#[test]
fn out_of_range_saved_expiry_is_unknown_instead_of_a_saturated_timestamp() {
    assert!(matches!(
        parse(
            &serde_json::json!({"availableCount":1,"credits":[{"status":"available","expiresAt":1e300}]})
        ),
        SavedResets::Failed(_)
    ));
}
