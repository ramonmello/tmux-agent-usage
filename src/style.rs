//! Literal tmux foreground/background/attribute styles, never format strings.

#[derive(Default)]
pub struct Styles {
    values: Option<[String; 5]>,
}

#[derive(Clone, Copy)]
pub enum Role {
    Normal,
    High,
    Critical,
    Muted,
    ResetSoon,
}

impl Styles {
    pub fn set(&mut self, flag: &str, value: &str) -> Result<(), String> {
        let role = match flag {
            "--style-normal" => Role::Normal,
            "--style-high" => Role::High,
            "--style-critical" => Role::Critical,
            "--style-muted" => Role::Muted,
            "--style-reset-soon" => Role::ResetSoon,
            _ => return Err(format!("unknown status flag {flag:?}")),
        };
        if !valid_style(value) {
            return Err(format!(
                "invalid style for {flag}: {value:?}; expected literal tmux colors and attributes"
            ));
        }
        let values = self.values.get_or_insert_with(|| {
            [
                "fg=default",
                "fg=yellow",
                "fg=red",
                "fg=brightblack",
                "fg=green",
            ]
            .map(str::to_owned)
        });
        values[role as usize] = value.to_owned();
        Ok(())
    }

    pub fn span(&self, role: Role) -> String {
        self.values
            .as_ref()
            .map(|values| format!("#[{}]", values[role as usize]))
            .unwrap_or_default()
    }
}

pub(crate) fn valid_color(value: &str) -> bool {
    if value.len() == 7 && value.starts_with('#') {
        return value[1..].bytes().all(|byte| byte.is_ascii_hexdigit());
    }
    if let Some(number) = value.strip_prefix("colour") {
        return !number.is_empty()
            && number.bytes().all(|byte| byte.is_ascii_digit())
            && number.parse::<u8>().is_ok();
    }
    matches!(value, "default" | "terminal")
        || matches!(
            value.strip_prefix("bright").unwrap_or(value),
            "black" | "red" | "green" | "yellow" | "blue" | "magenta" | "cyan" | "white"
        )
}

pub(crate) fn valid_style(value: &str) -> bool {
    if value.is_empty() || value.chars().any(char::is_control) {
        return false;
    }
    let mut found = false;
    for comma_part in value.split(',') {
        if comma_part.trim().is_empty() {
            return false;
        }
        for part in comma_part.split(' ').filter(|part| !part.is_empty()) {
            found = true;
            if let Some((key, color)) = part.split_once('=') {
                if !matches!(key, "fg" | "bg" | "us") || !valid_color(color) {
                    return false;
                }
            } else if !matches!(part, "default" | "none" | "noattr")
                && !matches!(
                    part.strip_prefix("no").unwrap_or(part),
                    "acs"
                        | "bright"
                        | "bold"
                        | "dim"
                        | "underscore"
                        | "blink"
                        | "reverse"
                        | "hidden"
                        | "italics"
                        | "overline"
                        | "strikethrough"
                        | "double-underscore"
                        | "curly-underscore"
                        | "dotted-underscore"
                        | "dashed-underscore"
                )
            {
                return false;
            }
        }
    }
    found
}
