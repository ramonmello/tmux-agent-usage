//! Explicit popup colors shared by the ANSI renderer and tmux launcher.

#[derive(Clone, Copy)]
pub enum Role {
    Text,
    Muted,
    Dim,
    Ok,
    High,
    Critical,
    Accent,
}

const FLAGS: [&str; 7] = [
    "--popup-text",
    "--popup-muted",
    "--popup-dim",
    "--popup-ok",
    "--popup-high",
    "--popup-critical",
    "--popup-accent",
];

#[derive(Default)]
pub struct PopupTheme {
    colors: [Option<String>; 7],
    pub border: Option<String>,
}

impl PopupTheme {
    pub fn set(&mut self, flag: &str, value: &str) -> Result<(), String> {
        if flag == "--popup-border" {
            if !crate::style::valid_style(value) {
                return Err(format!(
                    "invalid border style for {flag}: {value:?}; expected literal tmux colors and attributes"
                ));
            }
            self.border = Some(value.to_owned());
        } else {
            let index = FLAGS
                .iter()
                .position(|candidate| *candidate == flag)
                .ok_or_else(|| format!("unknown popup flag {flag:?}"))?;
            if !crate::style::valid_color(value) {
                return Err(format!(
                    "invalid color for {flag}: {value:?}; expected one tmux color name, colour0..colour255 or #rrggbb"
                ));
            }
            self.colors[index] = Some(value.to_owned());
        }
        Ok(())
    }

    pub fn child_flags(&self) -> Vec<String> {
        FLAGS
            .iter()
            .zip(&self.colors)
            .filter_map(|(flag, value)| {
                value
                    .as_ref()
                    .map(|value| [flag.to_string(), value.clone()])
            })
            .flatten()
            .collect()
    }

    pub fn title(&self) -> String {
        match &self.colors[Role::Text as usize] {
            Some(color) => format!("#[fg={color}] Agent usage "),
            None => " Agent usage ".to_owned(),
        }
    }

    pub fn paint(&self, role: Role, text: &str) -> String {
        let color = self.colors[role as usize].as_deref().unwrap_or(match role {
            Role::Text => "default",
            Role::Muted | Role::Dim => "brightblack",
            Role::Ok => "green",
            Role::High => "yellow",
            Role::Critical => "red",
            Role::Accent => "blue",
        });
        format!("{}{text}\x1b[0m", ansi(color))
    }
}

// Only validated literal colors reach this conversion. Named ANSI colors
// retain terminal palette semantics; explicit indexed colors stay indexed.
fn ansi(color: &str) -> String {
    if let Some(hex) = color.strip_prefix('#') {
        let value = u32::from_str_radix(hex, 16).expect("validated RGB color");
        return format!(
            "\x1b[38;2;{};{};{}m",
            (value >> 16) & 0xff,
            (value >> 8) & 0xff,
            value & 0xff
        );
    }
    if let Some(index) = color.strip_prefix("colour") {
        return format!("\x1b[38;5;{}m", index.parse::<u8>().unwrap());
    }
    if matches!(color, "default" | "terminal") {
        return "\x1b[39m".to_owned();
    }
    let (base, name) = color
        .strip_prefix("bright")
        .map_or((30, color), |name| (90, name));
    let index = [
        "black", "red", "green", "yellow", "blue", "magenta", "cyan", "white",
    ]
    .iter()
    .position(|candidate| *candidate == name)
    .expect("validated ANSI color");
    format!("\x1b[{}m", base + index)
}
