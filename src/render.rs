//! Agent bodies with optional semantic tmux styles. Presentation belongs to the consumer.

use crate::style::{Role, Styles};

/// From this percentage on, the gauge turns yellow and shows the reset time.
const HIGH_USAGE: i64 = 70;
/// From this percentage on, the gauge turns red.
const CRITICAL_USAGE: i64 = 90;
pub const FIVE_HOURS: i64 = 5 * 60 * 60;
pub const SEVEN_DAYS: i64 = 7 * 24 * 60 * 60;

/// How close to the reset counts as "near": 2h for 5h windows, 2d for 7d
/// windows, and the last 2/5 of any other window.
fn reset_soon_threshold(window_secs: i64) -> i64 {
    match window_secs {
        FIVE_HOURS => 2 * 60 * 60,
        SEVEN_DAYS => 2 * 24 * 60 * 60,
        other => other * 2 / 5,
    }
}

#[derive(Debug, Clone, Copy)]
pub struct Gauge {
    pub used_percent: f64,
    pub resets_at: f64,
    pub window_secs: i64,
}

pub fn body(gauges: Option<&[Option<Gauge>]>, styles: &Styles, now: i64) -> String {
    let body = match gauges {
        Some(gauges) if !gauges.is_empty() => gauges
            .iter()
            .map(|gauge| match gauge {
                Some(gauge) => render_gauge(gauge, styles, now),
                None => format!("{}–", styles.span(Role::Muted)),
            })
            .collect::<Vec<_>>()
            .join(&format!("{} · ", styles.span(Role::Muted))),
        _ => format!("{}–", styles.span(Role::Muted)),
    };
    format!("{body}{}", styles.span(Role::Normal))
}

fn render_gauge(gauge: &Gauge, styles: &Styles, now: i64) -> String {
    let remaining = gauge.resets_at as i64 - now;
    if remaining <= 0 {
        // The window already reset and nothing was used since.
        return format!("{}0%", styles.span(Role::Normal));
    }

    let used = gauge.used_percent.clamp(0.0, 100.0).round() as i64;
    let role = if used >= CRITICAL_USAGE {
        Role::Critical
    } else if used >= HIGH_USAGE {
        Role::High
    } else {
        Role::Normal
    };
    let mut text = format!("{}{used}%", styles.span(role));

    let reset_soon = gauge.window_secs > 0 && remaining <= reset_soon_threshold(gauge.window_secs);
    if used >= HIGH_USAGE {
        text.push_str(&format!(
            " {}↻{}",
            styles.span(Role::Muted),
            duration(remaining)
        ));
    } else if reset_soon {
        // Plenty left and it resets soon: safe to spend.
        text.push_str(&format!(
            " {}↻{}",
            styles.span(Role::ResetSoon),
            duration(remaining)
        ));
    }
    text
}

fn duration(seconds: i64) -> String {
    let minutes = (seconds + 59) / 60;
    if minutes < 60 {
        format!("{minutes}m")
    } else if minutes < 24 * 60 {
        match minutes % 60 {
            0 => format!("{}h", minutes / 60),
            rest => format!("{}h{rest:02}m", minutes / 60),
        }
    } else {
        match (minutes / 60) % 24 {
            0 => format!("{}d", minutes / (24 * 60)),
            hours => format!("{}d{hours}h", minutes / (24 * 60)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    const TEXT: &str = "default";
    const MUTED: &str = "brightblack";
    const GREEN: &str = "green";
    const YELLOW: &str = "yellow";
    const RED: &str = "red";

    fn render_gauge(gauge: &Gauge, now: i64) -> String {
        let mut styles = Styles::default();
        styles.set("--style-normal", "fg=default").unwrap();
        super::render_gauge(gauge, &styles, now)
    }

    const NOW: i64 = 1_000_000;

    fn gauge(used: f64, remaining: i64, window: i64) -> Gauge {
        Gauge {
            used_percent: used,
            resets_at: (NOW + remaining) as f64,
            window_secs: window,
        }
    }

    #[test]
    fn formats_durations() {
        assert_eq!(duration(30), "1m");
        assert_eq!(duration(45 * 60), "45m");
        assert_eq!(duration(80 * 60), "1h20m");
        assert_eq!(duration(65 * 60), "1h05m");
        assert_eq!(duration(2 * 3600), "2h");
        assert_eq!(duration(28 * 3600), "1d4h");
        assert_eq!(duration(3 * 86400), "3d");
    }

    #[test]
    fn plain_when_low_and_far_from_reset() {
        let text = render_gauge(&gauge(7.0, 3 * 3600, FIVE_HOURS), NOW);
        assert_eq!(text, format!("#[fg={TEXT}]7%"));
    }

    #[test]
    fn shows_reset_when_high() {
        let text = render_gauge(&gauge(87.0, 80 * 60, FIVE_HOURS), NOW);
        assert_eq!(text, format!("#[fg={YELLOW}]87% #[fg={MUTED}]↻1h20m"));
        let text = render_gauge(&gauge(95.0, 3 * 86400, SEVEN_DAYS), NOW);
        assert_eq!(text, format!("#[fg={RED}]95% #[fg={MUTED}]↻3d"));
    }

    #[test]
    fn shows_green_reset_when_low_and_near() {
        let text = render_gauge(&gauge(20.0, 40 * 60, FIVE_HOURS), NOW);
        assert_eq!(text, format!("#[fg={TEXT}]20% #[fg={GREEN}]↻40m"));
        let text = render_gauge(&gauge(20.0, 2 * 3600, FIVE_HOURS), NOW);
        assert_eq!(text, format!("#[fg={TEXT}]20% #[fg={GREEN}]↻2h"));
        let text = render_gauge(&gauge(20.0, 2 * 3600 + 60, FIVE_HOURS), NOW);
        assert_eq!(text, format!("#[fg={TEXT}]20%"));
        let text = render_gauge(&gauge(28.0, 2 * 86400, SEVEN_DAYS), NOW);
        assert_eq!(text, format!("#[fg={TEXT}]28% #[fg={GREEN}]↻2d"));
        let text = render_gauge(&gauge(28.0, 2 * 86400 + 60, SEVEN_DAYS), NOW);
        assert_eq!(text, format!("#[fg={TEXT}]28%"));
    }

    #[test]
    fn expired_window_is_zero() {
        let text = render_gauge(&gauge(80.0, -10, FIVE_HOURS), NOW);
        assert_eq!(text, format!("#[fg={TEXT}]0%"));
    }
}
