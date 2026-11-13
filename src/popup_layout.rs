//! Fixed, adaptive popup layout. No user-selectable layout settings.
use crate::details::{COLUMN, side_by_side_width, visible_width};
use crate::popup_theme::{PopupTheme, Role};
use unicode_width::UnicodeWidthStr;

/// Split renderer output into printable cells and complete SGR sequences.
fn tokens(text: &str) -> impl Iterator<Item = &str> {
    let mut rest = text;
    std::iter::from_fn(move || {
        let first = rest.chars().next()?;
        let length = if first == '\x1b' {
            rest.find('m').map_or(rest.len(), |end| end + 1)
        } else {
            first.len_utf8()
        };
        let (token, remaining) = rest.split_at(length);
        rest = remaining;
        Some(token)
    })
}

fn plain(text: &str) -> String {
    tokens(text)
        .filter(|token| !token.starts_with('\x1b'))
        .collect()
}

/// Keep SGR state across a wrap without leaking it into the adjacent column.
fn wrap(text: &str, cols: usize) -> Vec<String> {
    let mut lines = Vec::new();
    let mut line = String::new();
    let mut style = String::new();
    let mut width = 0;
    for mut token in tokens(text) {
        if token.starts_with('\x1b') {
            if token == "\x1b[0m" {
                style.clear();
            } else {
                style.push_str(token);
            }
        } else {
            if token.width() > cols {
                token = "�"; // A wide glyph cannot fit a one-cell viewport.
            }
            let cells = token.width();
            if width + cells > cols {
                if !style.is_empty() {
                    line.push_str("\x1b[0m");
                }
                lines.push(line);
                line = style.clone();
                width = 0;
            }
            width += cells;
        }
        line.push_str(token);
    }
    lines.push(line);
    lines
}

/// Compact alignment spaces only; values, dates and calculations are reused.
fn compact(text: &str) -> String {
    let mut space = false;
    tokens(text)
        .filter(|token| {
            if token.starts_with('\x1b') {
                return true;
            }
            let keep = *token != " " || !space;
            space = *token == " ";
            keep
        })
        .collect()
}

fn content(section: &str, cols: usize, level: u8, extras: usize) -> Vec<String> {
    let mut optional = 0;
    section
        .trim_end_matches('\n')
        .lines()
        .enumerate()
        .filter(|(index, line)| {
            if level == 0 {
                return true;
            }
            let text = plain(line);
            // The fixed renderer's hierarchy: heading and labelled values
            // have one leading space; explanations/bars/reset entries are
            // indented beneath them. Rules and margins go first on short UIs.
            let rule_or_margin = text.trim().chars().all(|ch| ch == '─');
            if rule_or_margin {
                return false;
            }
            if level == 1 || *index == 0 || !text.starts_with("  ") {
                return true;
            }
            optional += 1;
            optional <= extras
        })
        .flat_map(|(_, line)| {
            let line = if level > 0 || (cols < COLUMN && visible_width(line) > cols) {
                compact(line)
            } else {
                line.to_owned()
            };
            // A rule is decoration, never several wrapped rows of content.
            if plain(&line).trim().chars().all(|ch| ch == '─') && visible_width(&line) > cols {
                return vec![];
            }
            wrap(&line, cols)
        })
        .collect()
}

fn compose(
    theme: &PopupTheme,
    sections: &[Vec<String>],
    columns: bool,
    margins: bool,
) -> Vec<String> {
    let mut lines = Vec::new();
    if margins {
        lines.push(String::new());
    }
    if columns {
        let separator = theme.paint(Role::Dim, "│");
        for row in 0..sections[0].len().max(sections[1].len()) {
            let left = sections[0].get(row).map_or("", String::as_str);
            let right = sections[1].get(row).map_or("", String::as_str);
            lines.push(format!(
                "{left}{}  {separator}  {right}",
                " ".repeat(COLUMN.saturating_sub(visible_width(left)))
            ));
        }
    } else {
        for (index, section) in sections.iter().enumerate() {
            if margins && index > 0 {
                lines.push(String::new());
            }
            lines.extend(section.iter().cloned());
        }
    }
    if margins {
        lines.push(String::new());
    }
    lines
}

/// Retain the labelled values first, then spend remaining rows on the fixed
/// hierarchy's explanations and bars, alternating agents to avoid starvation.
fn summary(
    theme: &PopupTheme,
    sections: &[&str],
    width: usize,
    columns: bool,
    footer: &[String],
    hint: &str,
    rows: usize,
) -> Vec<String> {
    let render = |limits: &[usize]| {
        let sections: Vec<_> = sections
            .iter()
            .zip(limits)
            .map(|(section, limit)| content(section, width, 2, *limit))
            .collect();
        let mut lines = compose(theme, &sections, columns, false);
        lines.push(hint.to_owned());
        lines.extend(footer.iter().cloned());
        lines
    };
    let mut limits = vec![0; sections.len()];
    let mut stopped = vec![false; sections.len()];
    let mut best = render(&limits);
    if best.len() > rows {
        return best;
    }
    while stopped.iter().any(|stopped| !stopped) {
        for (index, stopped) in stopped.iter_mut().enumerate() {
            if *stopped {
                continue;
            }
            limits[index] += 1;
            let candidate = render(&limits);
            if candidate.len() > rows || candidate == best {
                limits[index] -= 1;
                *stopped = true;
            } else {
                best = candidate;
            }
        }
    }
    best
}

impl PopupTheme {
    /// Desired outer geometry includes the border and the actual wrapped
    /// footer. A long footer must not silently add a row and scroll a header.
    pub fn geometry(
        &self,
        sections: &[&str],
        footer: &str,
        cols: usize,
        rows: usize,
    ) -> (usize, usize) {
        let available = cols.saturating_sub(2).max(1);
        let inner_cols = if sections.len() == 2 && cols.saturating_sub(4) >= side_by_side_width() {
            side_by_side_width()
        } else {
            COLUMN.max(visible_width(footer) + 1).min(available)
        };
        let inner_rows = self
            .viewport(sections, footer, inner_cols, usize::MAX)
            .split('\n')
            .count();
        ((inner_cols + 2).min(cols), (inner_rows + 2).min(rows))
    }

    pub fn viewport(&self, sections: &[&str], footer: &str, cols: usize, rows: usize) -> String {
        let (cols, rows) = (cols.max(1), rows.max(1));
        let columns = sections.len() == 2 && cols >= side_by_side_width();
        let width = if columns { COLUMN } else { cols };
        let full_footer = wrap(&format!(" {footer}"), cols);
        let hint = if cols >= 18 {
            " q or esc to close"
        } else if cols >= 14 {
            "q/esc to close"
        } else {
            "q/esc"
        };
        let short_footer = wrap(&self.paint(Role::Dim, hint), cols);
        for level in 0..=1 {
            let sections: Vec<_> = sections
                .iter()
                .map(|section| content(section, width, level, 0))
                .collect();
            let mut lines = compose(self, &sections, columns, level == 0);
            let footer = if level == 0 || full_footer.len() == 1 {
                &full_footer
            } else {
                &short_footer
            };
            lines.extend(footer.iter().cloned());
            if lines.len() <= rows {
                return lines.join("\n");
            }
        }
        let footer = if full_footer.len() == 1 {
            &full_footer
        } else {
            &short_footer
        };
        let hint = self.paint(
            Role::Muted,
            &"… enlarge terminal for details"
                .chars()
                .take(cols)
                .collect::<String>(),
        );
        let lines = summary(self, sections, width, columns, footer, &hint, rows);
        if lines.len() <= rows {
            return lines.join("\n");
        }
        // Below the space needed for labelled values, keep both headings and
        // an explicit size hint rather than silently hiding an entire agent.
        let mut lines: Vec<_> = sections
            .iter()
            .map(|section| {
                plain(section.lines().next().unwrap_or(""))
                    .trim()
                    .to_owned()
            })
            .flat_map(|heading| wrap(&self.paint(Role::Text, &heading), cols))
            .collect();
        let footer_rows = short_footer.len().min(rows);
        lines.truncate(rows - footer_rows);
        if lines.len() + footer_rows < rows {
            lines.push(
                self.paint(
                    Role::Muted,
                    &"Enlarge terminal for details"
                        .chars()
                        .take(cols)
                        .collect::<String>(),
                ),
            );
        }
        lines.extend(short_footer.into_iter().take(footer_rows));
        lines.join("\n")
    }
}
