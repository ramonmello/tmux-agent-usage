//! The details popup (`tmux-agent-usage details --agents 'claude codex'`,
//! run inside `display-popup`). Plain terminal output with
//! explicit palette colors; every text shown is English.
//!
//! "Pace" compares the share of the quota used with the share of the window
//! that has passed (both known from `resets_at` and the window length). The
//! projections assume the current window's average rate keeps going, which
//! is crude early in a window, so nothing is projected until
//! `MIN_ELAPSED` of it has passed.

use crate::claude_resets::{ClaudeResets, Kind};
use crate::localtime::{date_time, short_time};
use crate::render::{FIVE_HOURS, Gauge, SEVEN_DAYS};
use crate::resets::{Credit, SavedResets};
use std::fmt::Write as _;
use unicode_width::UnicodeWidthChar;

use crate::popup_theme::{PopupTheme, Role};
use Role::{Accent, Critical, Dim, High, Muted, Ok, Text};

const HIGH_USAGE: f64 = 70.0;
const CRITICAL_USAGE: f64 = 90.0;
/// Below this share of the window passed, the pace is too noisy to judge.
const MIN_ELAPSED: f64 = 0.15;
/// Used/elapsed ratios within this band count as "on pace".
const ON_PACE_LOW: f64 = 0.85;
const ON_PACE_HIGH: f64 = 1.15;
const BAR_CELLS: usize = 16;
const LABEL: usize = 13;
const RULE: usize = 44;

const BOLD: &str = "\x1b[1m";
const RESET: &str = "\x1b[0m";

// ---------------------------------------------------------------- pace

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum PaceKind {
    /// The window already reset; no newer number seen.
    Renewed,
    TooEarly,
    Below,
    On,
    /// Runs out at the given time, before the reset.
    Above {
        runs_out_at: i64,
    },
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Pace {
    pub used: f64,
    /// 0..=1 share of the window that has passed.
    pub elapsed: f64,
    /// Percent per second, averaged over the window so far.
    pub rate: f64,
    pub kind: PaceKind,
}

pub fn pace(gauge: &Gauge, now: i64) -> Pace {
    let used = gauge.used_percent.clamp(0.0, 100.0);
    let resets_at = gauge.resets_at as i64;
    let window = gauge.window_secs;
    if resets_at <= now {
        return Pace {
            used: 0.0,
            elapsed: 0.0,
            rate: 0.0,
            kind: PaceKind::Renewed,
        };
    }
    if window <= 0 {
        return Pace {
            used,
            elapsed: 0.0,
            rate: 0.0,
            kind: PaceKind::TooEarly,
        };
    }
    let passed = (window - (resets_at - now)).clamp(0, window);
    let elapsed = passed as f64 / window as f64;
    let rate = if passed > 0 {
        used / passed as f64
    } else {
        0.0
    };
    let kind = if elapsed < MIN_ELAPSED {
        PaceKind::TooEarly
    } else {
        let ratio = (used / 100.0) / elapsed;
        if ratio < ON_PACE_LOW {
            PaceKind::Below
        } else if ratio <= ON_PACE_HIGH || rate <= 0.0 {
            PaceKind::On
        } else {
            let runs_out_at = now + ((100.0 - used) / rate) as i64;
            if runs_out_at < resets_at {
                PaceKind::Above { runs_out_at }
            } else {
                PaceKind::On
            }
        }
    };
    Pace {
        used,
        elapsed,
        rate,
        kind,
    }
}

// ------------------------------------------------------- saved resets

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Projection {
    pub first_run_out: Option<i64>,
    /// Resets the projection spends before they expire.
    pub used: usize,
    /// Resets with an expiry that would expire unspent.
    pub unused: usize,
}

/// Plays the current rate forward until the last expiry: whenever the quota
/// runs out before the window resets, the soonest-expiring saved reset is
/// spent (usage back to 0, window unchanged); without one, usage waits for
/// the window reset.
pub fn project(gauge: &Gauge, rate: f64, expiries: &[Option<i64>], now: i64) -> Projection {
    let finite: Vec<i64> = expiries.iter().flatten().copied().collect();
    let Some(&horizon) = finite.iter().max() else {
        return Projection {
            first_run_out: None,
            used: 0,
            unused: 0,
        };
    };
    let mut available: Vec<Option<i64>> = expiries.to_vec();
    available.sort_by_key(|expiry| expiry.unwrap_or(i64::MAX));

    let window = gauge.window_secs;
    let mut time = now;
    let mut usage = gauge.used_percent.clamp(0.0, 100.0);
    let mut resets_at = gauge.resets_at as i64;
    let mut first_run_out = None;
    let mut spent_finite = 0;
    let mut spent = 0;

    if rate > 0.0 && window > 0 {
        for _ in 0..10_000 {
            if time >= horizon {
                break;
            }
            if resets_at <= time {
                usage = 0.0;
                resets_at += window;
                continue;
            }
            let runs_out = time + ((100.0 - usage).max(0.0) / rate).ceil() as i64;
            if runs_out >= resets_at {
                time = resets_at;
                continue;
            }
            if runs_out >= horizon {
                break;
            }
            first_run_out.get_or_insert(runs_out);
            available.retain(|expiry| expiry.is_none_or(|at| at > runs_out));
            if available.is_empty() {
                time = resets_at;
                continue;
            }
            if available.remove(0).is_some() {
                spent_finite += 1;
            }
            spent += 1;
            usage = 0.0;
            time = runs_out;
        }
    }

    Projection {
        first_run_out,
        used: spent,
        unused: finite.len().saturating_sub(spent_finite),
    }
}

// ---------------------------------------------------------- formatting

/// `45m`, `2h 10m`, `4d 13h`.
pub fn long_duration(seconds: i64) -> String {
    let minutes = (seconds.max(0) + 59) / 60;
    if minutes < 60 {
        format!("{minutes}m")
    } else if minutes < 24 * 60 {
        match minutes % 60 {
            0 => format!("{}h", minutes / 60),
            rest => format!("{}h {rest}m", minutes / 60),
        }
    } else {
        match (minutes / 60) % 24 {
            0 => format!("{}d", minutes / (24 * 60)),
            hours => format!("{}d {hours}h", minutes / (24 * 60)),
        }
    }
}

fn window_name(window_secs: i64) -> String {
    match window_secs {
        FIVE_HOURS => "5 hours".to_owned(),
        SEVEN_DAYS => "Weekly".to_owned(),
        secs if secs > 0 && secs % 86_400 == 0 => format!("{} days", secs / 86_400),
        secs if secs > 0 => format!("{} hours", (secs + 1800) / 3600),
        _ => "Window".to_owned(),
    }
}

fn window_noun(window_secs: i64) -> &'static str {
    if window_secs == SEVEN_DAYS {
        "week"
    } else {
        "window"
    }
}

fn rate_unit(window_secs: i64) -> String {
    match window_secs {
        SEVEN_DAYS => "week".to_owned(),
        FIVE_HOURS => "5h".to_owned(),
        secs if secs > 0 && secs % 86_400 == 0 => format!("{}d", secs / 86_400),
        secs => format!("{}h", (secs.max(3600) + 1800) / 3600),
    }
}

impl PopupTheme {
    fn usage_color(&self, used: f64) -> Role {
        if used >= CRITICAL_USAGE {
            Critical
        } else if used >= HIGH_USAGE {
            High
        } else {
            Text
        }
    }

    /// Filled cells = quota used; `│` = how much of the window has passed.
    fn bar(&self, pace: &Pace) -> String {
        let filled = ((pace.used / 100.0) * BAR_CELLS as f64).round() as usize;
        let marker = ((pace.elapsed * BAR_CELLS as f64).round() as usize).min(BAR_CELLS - 1);
        let fill = self.usage_color(pace.used);
        (0..BAR_CELLS)
            .map(|cell| {
                if cell == marker {
                    self.paint(Accent, "│")
                } else if cell < filled {
                    self.paint(fill, "█")
                } else {
                    self.paint(Dim, "░")
                }
            })
            .collect()
    }

    fn indent() -> String {
        " ".repeat(LABEL + 2)
    }

    fn header(&self, out: &mut String, title: &str, subtitle: Option<&str>) {
        let _ = write!(out, " {BOLD}{}{RESET}", self.paint(Text, title));
        if let Some(subtitle) = subtitle {
            let _ = write!(out, "{}", self.paint(Muted, &format!(" · {subtitle}")));
        }
        out.push('\n');
        let _ = writeln!(out, " {}", self.paint(Dim, &"─".repeat(RULE)));
    }

    fn gauge_block(&self, out: &mut String, gauge: &Gauge, now: i64) {
        let name = window_name(gauge.window_secs);
        let pace = pace(gauge, now);
        let resets_at = gauge.resets_at as i64;

        if pace.kind == PaceKind::Renewed {
            let _ = writeln!(
                out,
                " {} {}   {}",
                self.paint(Muted, &format!("{name:<LABEL$}")),
                self.paint(Text, "  0%"),
                self.paint(Muted, &format!("renewed {}", short_time(resets_at, now)))
            );
            let _ = writeln!(
                out,
                "{}{}",
                Self::indent(),
                self.paint(Muted, "waiting for new data")
            );
            return;
        }

        let used = pace.used.round() as i64;
        let _ = writeln!(
            out,
            " {} {}   {}",
            self.paint(Muted, &format!("{name:<LABEL$}")),
            self.paint(self.usage_color(pace.used), &format!("{used:>3}%")),
            self.paint(Text, &format!("resets {}", date_time(resets_at, now)))
        );
        let _ = writeln!(
            out,
            "{}       {}",
            Self::indent(),
            self.paint(Muted, &format!("(in {})", long_duration(resets_at - now)))
        );
        let _ = writeln!(out, "{}{}", Self::indent(), self.bar(&pace));

        let passed = format!(
            "{}% of the {} passed",
            (pace.elapsed * 100.0).round() as i64,
            window_noun(gauge.window_secs)
        );
        let line = match pace.kind {
            PaceKind::TooEarly if gauge.window_secs > 0 => {
                // When `MIN_ELAPSED` of the window will have passed.
                let judged_at = resets_at - gauge.window_secs
                    + (MIN_ELAPSED * gauge.window_secs as f64).ceil() as i64;
                self.paint(
                    Muted,
                    &format!("pace in {} · {passed}", long_duration(judged_at - now)),
                )
            }
            PaceKind::TooEarly => self.paint(Muted, &passed),
            PaceKind::Below => format!(
                "{}{}",
                self.paint(Ok, "below pace"),
                self.paint(Muted, &format!(" · {passed}"))
            ),
            PaceKind::On => format!(
                "{}{}",
                self.paint(Text, "on pace"),
                self.paint(Muted, &format!(" · {passed}"))
            ),
            PaceKind::Above { runs_out_at } => format!(
                "{}{}",
                self.paint(
                    if pace.used >= CRITICAL_USAGE {
                        Critical
                    } else {
                        High
                    },
                    "above pace"
                ),
                self.paint(
                    Muted,
                    &format!(" · runs out ~{}", date_time(runs_out_at, now))
                )
            ),
            PaceKind::Renewed => unreachable!(),
        };
        let _ = writeln!(out, "{}{line}", Self::indent());
    }
}

// ------------------------------------------------------------- layout

/// Width of each agent column, in cells.
pub const COLUMN: usize = 54;
const GAP: &str = "  ";

/// Terminal cells taken by text, including external labels, ignoring SGR.
pub(crate) fn visible_width(text: &str) -> usize {
    let mut width = 0;
    let mut chars = text.chars();
    while let Some(character) = chars.next() {
        if character == '\x1b' {
            for next in chars.by_ref() {
                if next.is_ascii_alphabetic() {
                    break;
                }
            }
        } else {
            width += character.width().unwrap_or(0);
        }
    }
    width
}

/// Columns needed to show both agents side by side.
pub fn side_by_side_width() -> usize {
    COLUMN * 2 + GAP.len() * 2 + 1
}

/// Selected agents in caller order, in columns or stacked, plus one footer.
impl PopupTheme {
    /// Footer entries follow the selected agents, including when stacked.
    pub fn selected_footer(&self, entries: &[(&str, Option<i64>)], now: i64) -> String {
        let mut parts = Vec::new();
        for (title, at) in entries {
            if let Some(at) = at.filter(|at| *at > 0) {
                parts.push(format!("{title} as of {}", short_time(at, now)));
            }
        }
        parts.push("q or esc to close".to_owned());
        self.paint(Dim, &parts.join(" · "))
    }
}

#[cfg(test)]
/// Keeps `screen` within `rows` lines. Printing past the last row scrolls the
/// popup, which pushes the headers off the top; instead the body is cut and
/// the footer kept on the last row.
pub fn fit(screen: &str, rows: usize) -> String {
    let lines: Vec<&str> = screen.split('\n').collect();
    if lines.len() <= rows || rows == 0 {
        return screen.to_owned();
    }
    let footer = lines[lines.len() - 1];
    let mut kept: Vec<&str> = lines[..rows - 1].to_vec();
    kept.push(footer);
    kept.join("\n")
}

// ------------------------------------------------------------- claude

/// The Claude Code column; `saved` is `None` while the saved resets are
/// loading.
impl PopupTheme {
    pub fn claude(
        &self,
        limits: Option<&(crate::claude::Limits, i64)>,
        saved: Option<&ClaudeResets>,
        now: i64,
    ) -> String {
        let mut out = self.claude_local(limits, now);
        out.push('\n');
        match saved {
            Some(saved) => out.push_str(&self.claude_resets(saved, now)),
            None => {
                out.push_str(&self.resets_pending());
                out.push('\n');
            }
        }
        out
    }

    pub fn claude_local(&self, limits: Option<&(crate::claude::Limits, i64)>, now: i64) -> String {
        let mut out = String::new();
        self.header(&mut out, "Claude Code", None);
        let Some((limits, _)) = limits else {
            let _ = writeln!(
                out,
                " {}",
                self.paint(Muted, "No data yet · send a message in Claude Code")
            );
            return out;
        };
        let windows = [
            (limits.five_hour, FIVE_HOURS),
            (limits.seven_day, SEVEN_DAYS),
        ];
        for (index, (window, window_secs)) in windows.into_iter().enumerate() {
            if index > 0 {
                out.push('\n');
            }
            match window {
                Some(window) => self.gauge_block(
                    &mut out,
                    &Gauge {
                        used_percent: window.used_percentage,
                        resets_at: window.resets_at,
                        window_secs,
                    },
                    now,
                ),
                None => {
                    let _ = writeln!(
                        out,
                        " {} {}",
                        self.paint(Muted, &format!("{:<LABEL$}", window_name(window_secs))),
                        self.paint(Muted, "unknown")
                    );
                }
            }
        }
        out
    }

    fn kind_name(&self, kind: Kind) -> &'static str {
        match kind {
            Kind::Full => "Full reset",
            Kind::Fable => "Fable reset",
            Kind::Session => "Session reset",
            Kind::Other => "Reset",
        }
    }

    fn kind_refills(&self, kind: Kind) -> Option<&'static str> {
        match kind {
            Kind::Full => Some("refills 5 hours and the week"),
            Kind::Fable => Some("refills 5 hours and Fable's weekly use"),
            Kind::Session => Some("refills 5 hours"),
            Kind::Other => None,
        }
    }

    /// The Claude Code saved resets section.
    fn claude_resets(&self, saved: &ClaudeResets, now: i64) -> String {
        let mut out = String::new();
        let label = self.paint(Muted, &format!("{:<LABEL$}", "Saved resets"));
        let (groups, cooldown_until) = match saved {
            ClaudeResets::Failed(_) => {
                let _ = writeln!(out, " {label} {}", self.paint(Dim, "couldn't check"));
                return out;
            }
            ClaudeResets::Blocked(reason) => {
                let _ = writeln!(out, " {label} {}", self.paint(Dim, "couldn't check"));
                let _ = writeln!(
                    out,
                    "   {}",
                    self.paint(Muted, &crate::terminal::safe_text(reason))
                );
                return out;
            }
            ClaudeResets::Unavailable => {
                let _ = writeln!(out, " {label} {}", self.paint(Muted, "unavailable"));
                return out;
            }
            ClaudeResets::Known { groups, .. } if groups.is_empty() => {
                let _ = writeln!(out, " {label} {}", self.paint(Text, "none"));
                return out;
            }
            ClaudeResets::Known {
                groups,
                cooldown_until,
            } => (groups, cooldown_until),
        };
        let total: u64 = groups.iter().map(|group| group.count).sum();
        let _ = writeln!(out, " {label} {}", self.paint(Text, &total.to_string()));
        for group in groups {
            let name = match group.count {
                1 => self.kind_name(group.kind).to_owned(),
                count => format!("{} ×{count}", self.kind_name(group.kind)),
            };
            // Far expiries in whole days keep the line within the column.
            let expiry = match group.expires_at {
                Some(at) => format!(
                    "{} {}",
                    self.paint(Text, &format!("expires {}", date_time(at, now))),
                    self.paint(
                        Muted,
                        &format!(
                            "(in {})",
                            if at - now >= 7 * 86_400 {
                                format!("{}d", (at - now) / 86_400)
                            } else {
                                long_duration(at - now)
                            }
                        )
                    )
                ),
                None => self.paint(Text, "never expires"),
            };
            let _ = writeln!(
                out,
                "   {}{}{expiry}",
                self.paint(Text, &name),
                self.paint(Muted, " · ")
            );
            if let Some(refills) = self.kind_refills(group.kind) {
                let _ = writeln!(out, "     {}", self.paint(Muted, refills));
            }
            if let Some(reason) = &group.blocked {
                let _ = writeln!(
                    out,
                    "     {}",
                    self.paint(High, &crate::terminal::safe_text(reason))
                );
            }
        }
        match cooldown_until {
            Some(at) => {
                let _ = writeln!(
                    out,
                    "   {}",
                    self.paint(High, &format!("cooling down until {}", date_time(*at, now)))
                );
            }
            None => {
                let _ = writeln!(
                    out,
                    "   {}",
                    self.paint(Muted, "→ /limit-reset in Claude Code to use one")
                );
            }
        }
        out
    }

    // -------------------------------------------------------------- codex

    /// Everything above the saved resets line.
    fn codex_top(&self, status: Option<&crate::codex::Status>, now: i64) -> String {
        let mut out = String::new();
        let plan = status
            .and_then(|status| status.plan_type.as_deref())
            .map(|plan| format!("{} plan", crate::terminal::safe_text(plan)));
        self.header(&mut out, "Codex", plan.as_deref());
        let Some(status) = status else {
            let _ = writeln!(
                out,
                " {}",
                self.paint(Muted, "No usage data yet · send a message in Codex")
            );
            out.push('\n');
            return out;
        };
        for (index, window) in status.windows.iter().enumerate() {
            if index > 0 {
                out.push('\n');
            }
            self.gauge_block(&mut out, &self.codex_gauge(window), now);
        }
        if let Some(credits) = &status.credits {
            let value = if credits.unlimited {
                "unlimited".to_owned()
            } else if credits.has_credits {
                credits
                    .balance
                    .as_deref()
                    .map(crate::terminal::safe_text)
                    .unwrap_or_else(|| "yes".to_owned())
            } else {
                "none".to_owned()
            };
            out.push('\n');
            let _ = writeln!(
                out,
                " {} {}",
                self.paint(Muted, &format!("{:<LABEL$}", "Credits")),
                self.paint(Text, &value)
            );
        }
        out.push('\n');
        out
    }

    /// The Codex column; `saved` is `None` while the saved resets are loading.
    pub fn codex(
        &self,
        status: Option<&crate::codex::Status>,
        saved: Option<&SavedResets>,
        now: i64,
    ) -> String {
        let mut out = self.codex_top(status, now);
        match saved {
            Some(saved) => out.push_str(&self.codex_resets(status, saved, now)),
            None => {
                out.push_str(&self.resets_pending());
                out.push('\n');
            }
        }
        out
    }

    fn codex_gauge(&self, window: &crate::codex::Window) -> Gauge {
        Gauge {
            used_percent: window.used_percent,
            resets_at: window.resets_at,
            window_secs: window.window_secs,
        }
    }

    fn resets_pending(&self) -> String {
        format!(
            " {} {}",
            self.paint(Muted, &format!("{:<LABEL$}", "Saved resets")),
            self.paint(Muted, "checking…")
        )
    }

    /// The saved resets section.
    fn codex_resets(
        &self,
        status: Option<&crate::codex::Status>,
        saved: &SavedResets,
        now: i64,
    ) -> String {
        let mut out = String::new();
        let label = self.paint(Muted, &format!("{:<LABEL$}", "Saved resets"));
        match saved {
            SavedResets::Failed(_) => {
                let _ = writeln!(out, " {label} {}", self.paint(Dim, "couldn't check"));
            }
            SavedResets::Unavailable => {
                let _ = writeln!(out, " {label} {}", self.paint(Muted, "unavailable"));
            }
            SavedResets::CountOnly { count: 0 } | SavedResets::Known { count: 0, .. } => {
                let _ = writeln!(out, " {label} {}", self.paint(Text, "none"));
            }
            SavedResets::CountOnly { count } => {
                let _ = writeln!(
                    out,
                    " {label} {}{}",
                    self.paint(Text, &count.to_string()),
                    self.paint(Muted, " · expiry unknown")
                );
            }
            SavedResets::Known { count, credits } => {
                let _ = writeln!(out, " {label} {}", self.paint(Text, &count.to_string()));
                for credit in credits {
                    let _ = writeln!(out, "   {}", self.credit_line(credit, now));
                }
                let missing = count.saturating_sub(credits.len() as u64);
                if missing > 0 {
                    let _ = writeln!(
                        out,
                        "   {}",
                        self.paint(Muted, &format!("+{missing} more · expiry unknown"))
                    );
                }
                self.advice(&mut out, status, credits, now);
            }
        }
        out
    }

    fn credit_line(&self, credit: &Credit, now: i64) -> String {
        match credit.expires_at {
            Some(at) if at > now => format!(
                "{}   {}",
                self.paint(Text, &format!("expires {}", date_time(at, now))),
                self.paint(Muted, &format!("(in {})", long_duration(at - now)))
            ),
            Some(_) => self.paint(Muted, "expired"),
            None => self.paint(Text, "never expires"),
        }
    }

    fn advice(
        &self,
        out: &mut String,
        status: Option<&crate::codex::Status>,
        credits: &[Credit],
        now: i64,
    ) {
        if credits.is_empty() {
            return;
        }
        if credits.iter().all(|credit| credit.expires_at.is_none()) {
            let _ = writeln!(
                out,
                "   {}",
                self.paint(
                    Muted,
                    "→ they don't expire: keep them for when you need one"
                )
            );
            return;
        }
        // Saved resets refill the whole quota; the longest window decides.
        let Some(window) = status.and_then(|status| status.windows.last()) else {
            return;
        };
        let gauge = self.codex_gauge(window);
        let pace = pace(&gauge, now);
        if matches!(pace.kind, PaceKind::TooEarly | PaceKind::Renewed) {
            let _ = writeln!(
                out,
                "   {}",
                self.paint(Muted, "not enough data yet to project")
            );
            return;
        }
        let expiries: Vec<Option<i64>> = credits.iter().map(|credit| credit.expires_at).collect();
        let projection = project(&gauge, pace.rate, &expiries, now);
        let per_window = (pace.rate * gauge.window_secs as f64).round() as i64;
        let rate = format!("pace ~{per_window}%/{}", rate_unit(gauge.window_secs));
        let several = credits.len() > 1;

        match projection.first_run_out {
            None => {
                let _ = writeln!(
                    out,
                    "   {}",
                    self.paint(
                        Muted,
                        &format!(
                            "{rate} · won't run out before {}",
                            if several { "they expire" } else { "it expires" }
                        )
                    )
                );
                let _ = writeln!(
                    out,
                    "   {}",
                    self.paint(
                        Ok,
                        &format!(
                            "→ you can use more: {} go unused",
                            if several {
                                "these resets would"
                            } else {
                                "this reset would"
                            }
                        )
                    )
                );
            }
            Some(first) => {
                let _ = writeln!(
                    out,
                    "   {}",
                    self.paint(
                        Muted,
                        &format!("{rate} · runs out ~{}", date_time(first, now))
                    )
                );
                let text = if projection.unused == 0 {
                    if several {
                        format!("→ you'd use all {} before they expire", projection.used)
                    } else {
                        "→ you'd use it before it expires".to_owned()
                    }
                } else {
                    format!(
                        "→ you'd use {}; {} would go unused",
                        projection.used, projection.unused
                    )
                };
                let _ = writeln!(out, "   {}", self.paint(High, &text));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOW: i64 = 1_000_000_000;
    const DAY: i64 = 86_400;

    fn weekly(used: f64, remaining: i64) -> Gauge {
        Gauge {
            used_percent: used,
            resets_at: (NOW + remaining) as f64,
            window_secs: SEVEN_DAYS,
        }
    }

    #[test]
    fn side_by_side_pads_left_column() {
        let text = PopupTheme::default().viewport(
            &["a\nbb\n", "x\n"],
            "f",
            side_by_side_width(),
            usize::MAX,
        );
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(visible_width(lines[1]), COLUMN + GAP.len() * 2 + 2);
        assert!(lines[2].ends_with(&format!(
            "{GAP}{}{GAP}",
            PopupTheme::default().paint(Dim, "│")
        )));
        assert_eq!(
            PopupTheme::default()
                .viewport(&["a\nbb\n", "x\n"], "", side_by_side_width(), usize::MAX)
                .split('\n')
                .count(),
            5
        );
        assert_eq!(
            PopupTheme::default()
                .viewport(&["a\nbb\n", "x\n"], "", 40, usize::MAX)
                .split('\n')
                .count(),
            7
        );
    }

    #[test]
    fn fit_keeps_headers_and_footer() {
        let text = "\nhead\na\nb\nc\n\nfoot";
        assert_eq!(fit(text, 10), text);
        assert_eq!(fit(text, 4), "\nhead\na\nfoot");
        assert_eq!(fit(text, 4).split('\n').count(), 4);
    }

    #[test]
    fn durations() {
        assert_eq!(long_duration(45 * 60), "45m");
        assert_eq!(long_duration(130 * 60), "2h 10m");
        assert_eq!(long_duration(2 * 3600), "2h");
        assert_eq!(long_duration(4 * DAY + 13 * 3600), "4d 13h");
        assert_eq!(long_duration(3 * DAY), "3d");
    }

    #[test]
    fn too_early_says_when_pace_starts() {
        // 5-hour window, 15 minutes in: judged at 45 minutes.
        let gauge = Gauge {
            used_percent: 1.0,
            resets_at: (NOW + FIVE_HOURS - 15 * 60) as f64,
            window_secs: FIVE_HOURS,
        };
        let mut out = String::new();
        PopupTheme::default().gauge_block(&mut out, &gauge, NOW);
        assert!(out.contains("pace in 30m · 5% of the window passed"));
    }

    #[test]
    fn pace_kinds() {
        // 35% of the week passed.
        let remaining = SEVEN_DAYS * 65 / 100;
        assert_eq!(pace(&weekly(10.0, remaining), NOW).kind, PaceKind::Below);
        assert_eq!(pace(&weekly(35.0, remaining), NOW).kind, PaceKind::On);
        assert!(matches!(
            pace(&weekly(80.0, remaining), NOW).kind,
            PaceKind::Above { runs_out_at } if runs_out_at < NOW + remaining
        ));
        // 5% passed: too early to judge.
        assert_eq!(
            pace(&weekly(50.0, SEVEN_DAYS * 95 / 100), NOW).kind,
            PaceKind::TooEarly
        );
        assert_eq!(pace(&weekly(50.0, -1), NOW).kind, PaceKind::Renewed);
    }

    #[test]
    fn slow_pace_leaves_resets_unused() {
        let remaining = SEVEN_DAYS * 65 / 100;
        let gauge = weekly(10.0, remaining);
        let rate = pace(&gauge, NOW).rate;
        let projection = project(
            &gauge,
            rate,
            &[Some(NOW + 21 * DAY), Some(NOW + 22 * DAY)],
            NOW,
        );
        assert_eq!(projection.first_run_out, None);
        assert_eq!(projection.used, 0);
        assert_eq!(projection.unused, 2);
    }

    #[test]
    fn fast_pace_spends_resets() {
        // 80% used in 2 days: 40%/day, runs out in half a day, again 2.5
        // days after each reset.
        let gauge = weekly(80.0, 5 * DAY);
        let rate = pace(&gauge, NOW).rate;
        let projection = project(
            &gauge,
            rate,
            &[Some(NOW + 21 * DAY), Some(NOW + 22 * DAY)],
            NOW,
        );
        assert_eq!(projection.first_run_out, Some(NOW + DAY / 2));
        assert_eq!(projection.used, 2);
        assert_eq!(projection.unused, 0);
    }

    #[test]
    fn reset_expiring_before_run_out_is_unused() {
        let gauge = weekly(80.0, 5 * DAY);
        let rate = pace(&gauge, NOW).rate;
        let projection = project(&gauge, rate, &[Some(NOW + 3600)], NOW);
        assert_eq!(projection.used, 0);
        assert_eq!(projection.unused, 1);
    }

    #[test]
    fn failure_never_shows_a_number() {
        let text = PopupTheme::default().codex_resets(None, &SavedResets::Failed("x".into()), NOW);
        assert!(text.contains("couldn't check"));
        assert!(!text.contains("none"));
        let text = PopupTheme::default().codex_resets(None, &SavedResets::Unavailable, NOW);
        assert!(text.contains("unavailable"));
        let text = PopupTheme::default().codex_resets(
            None,
            &SavedResets::Known {
                count: 0,
                credits: vec![],
            },
            NOW,
        );
        assert!(text.contains("none"));
    }
}
