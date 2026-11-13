use crate::test_support::{Sandbox, success};
use crate::{claude, claude_resets, codex, details, resets};
use serde::Deserialize;
use std::fs;
use std::path::Path;

const GOLDENS: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/goldens/baseline");

// Set TZ before a fresh process starts. Changing the parent environment is
// unsafe in a threaded test harness and can leave libc's timezone cache stale.
fn in_utc_child(test: &str) -> bool {
    if std::env::var("AGENT_USAGE_GOLDEN_CHILD").as_deref() == Ok(test) {
        return true;
    }
    let sandbox = Sandbox::new();
    success(
        sandbox
            .command(std::env::current_exe().unwrap())
            .args(["--exact", test, "--nocapture"])
            .env("AGENT_USAGE_GOLDEN_CHILD", test)
            .output()
            .expect("run golden test in an isolated UTC process"),
    );
    false
}

fn golden(name: &str, actual: &str) {
    let file = Path::new(GOLDENS).join(name);
    if std::env::var("AGENT_USAGE_UPDATE_GOLDENS").as_deref() == Ok("1") {
        fs::write(&file, actual).expect("write explicitly requested golden");
    }
    let expected = fs::read_to_string(&file).expect("read committed golden");
    assert_eq!(actual.as_bytes(), expected.as_bytes(), "golden: {name}");
}

#[derive(Deserialize)]
struct GaugeInput {
    used_percent: f64,
    remaining_secs: i64,
    window_secs: i64,
}

#[derive(Deserialize)]
struct GroupInput {
    kind: String,
    count: u64,
    expires_at: Option<i64>,
    blocked: Option<String>,
}

#[derive(Deserialize)]
struct CreditInput {
    expires_at: Option<i64>,
    title: Option<String>,
}

#[derive(Deserialize)]
struct ResetCase {
    name: String,
    state: String,
    #[serde(default)]
    claude_groups: Vec<GroupInput>,
    cooldown_until: Option<i64>,
    #[serde(default)]
    codex_count: u64,
    #[serde(default)]
    codex_credits: Vec<CreditInput>,
}

impl ResetCase {
    fn saved(
        &self,
    ) -> (
        Option<claude_resets::ClaudeResets>,
        Option<resets::SavedResets>,
    ) {
        use claude_resets::{ClaudeResets, Group, Kind};
        use resets::{Credit, SavedResets};
        match self.state.as_str() {
            "loading" => (None, None),
            "unavailable" => (
                Some(ClaudeResets::Unavailable),
                Some(SavedResets::Unavailable),
            ),
            "failed" => (
                Some(ClaudeResets::Failed("fixture: request timed out".into())),
                Some(SavedResets::Failed("fixture: client closed early".into())),
            ),
            "count-only" => (
                Some(ClaudeResets::Unavailable),
                Some(SavedResets::CountOnly {
                    count: self.codex_count,
                }),
            ),
            "known" => (
                Some(ClaudeResets::Known {
                    groups: self
                        .claude_groups
                        .iter()
                        .map(|group| Group {
                            kind: match group.kind.as_str() {
                                "full" => Kind::Full,
                                "fable" => Kind::Fable,
                                "session" => Kind::Session,
                                other => panic!("unknown fixture reset kind: {other}"),
                            },
                            count: group.count,
                            expires_at: group.expires_at,
                            blocked: group.blocked.clone(),
                        })
                        .collect(),
                    cooldown_until: self.cooldown_until,
                }),
                Some(SavedResets::Known {
                    count: self.codex_count,
                    credits: self
                        .codex_credits
                        .iter()
                        .map(|credit| Credit {
                            expires_at: credit.expires_at,
                            title: credit.title.clone(),
                        })
                        .collect(),
                }),
            ),
            other => panic!("unknown fixture reset state: {other}"),
        }
    }
}

#[derive(Deserialize)]
struct PopupFixture {
    now: i64,
    claude: claude::Limits,
    claude_observed_at: i64,
    codex_windows: Vec<GaugeInput>,
    codex_observed_at: i64,
    plan_type: String,
    credits: codex::Credits,
    cases: Vec<ResetCase>,
}

#[test]
fn popup_goldens() {
    if !in_utc_child("baseline_goldens::popup_goldens") {
        return;
    }
    check_popup_goldens(crate::popup_tests::gruvbox_theme());
}

pub(super) fn check_popup_goldens(theme: crate::popup_theme::PopupTheme) {
    let fixture: PopupFixture =
        serde_json::from_str(include_str!("../fixtures/baseline/popup.json")).unwrap();
    assert_eq!(
        crate::localtime::date_time(fixture.now, fixture.now),
        "today 12:00",
        "goldens require a fresh process in UTC"
    );
    let claude = (fixture.claude, fixture.claude_observed_at);
    let codex = codex::Status {
        windows: fixture
            .codex_windows
            .iter()
            .map(|input| codex::Window {
                used_percent: input.used_percent,
                resets_at: (fixture.now + input.remaining_secs) as f64,
                window_secs: input.window_secs,
            })
            .collect(),
        plan_type: Some(fixture.plan_type),
        credits: Some(fixture.credits),
        observed_at: Some(fixture.codex_observed_at),
    };
    let footer = theme.selected_footer(
        &[
            ("Claude Code", Some(claude.1)),
            ("Codex", codex.observed_at),
        ],
        fixture.now,
    );
    for case in fixture.cases {
        let (claude_saved, codex_saved) = case.saved();
        let left = theme.claude(Some(&claude), claude_saved.as_ref(), fixture.now);
        let right = theme.codex(Some(&codex), codex_saved.as_ref(), fixture.now);
        for (layout, cols) in [
            ("columns", details::side_by_side_width()),
            ("stacked", details::side_by_side_width() - 1),
        ] {
            let screen = theme.viewport(&[&left, &right], &footer, cols, usize::MAX);
            golden(&format!("popup-{}-{layout}.ansi", case.name), &screen);
            if case.name == "known" {
                golden(
                    &format!("popup-known-{layout}-clipped.ansi"),
                    &details::fit(&screen, 18),
                );
                // The old clipping fixtures remain historical bytes. The
                // production summary must now retain both agents and values.
                let short = theme.viewport(&[&left, &right], &footer, cols, 18);
                for value in [
                    "Claude Code",
                    "Codex",
                    "7%",
                    "87%",
                    "95%",
                    "20%",
                    "Saved resets",
                    "Credits",
                    "12.50",
                    "q or esc to close",
                ] {
                    assert!(
                        short.contains(value),
                        "missing {value} from {layout} summary"
                    );
                }
                assert!(short.split('\n').count() <= 18);
                if let Some(dir) = std::env::var_os("AGENT_USAGE_CAPTURE_DIR") {
                    fs::create_dir_all(&dir).unwrap();
                    fs::write(
                        Path::new(&dir).join(format!("gruvbox-{layout}-18.ansi")),
                        short,
                    )
                    .unwrap();
                }
            }
        }
    }
}
