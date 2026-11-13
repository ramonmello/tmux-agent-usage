use crate::popup_theme::PopupTheme;
use crate::provider::AgentId;
use crate::style::Styles;

pub const USAGE: &str = "usage:\n  tmux-agent-usage status <agent> [--style-{normal,high,critical,muted,reset-soon} STYLE]\n  tmux-agent-usage popup CLIENT [COLS ROWS] [--agents LIST] [--claude-resets] [POPUP_FLAGS]\n  tmux-agent-usage details [--no-wait] [--agents LIST] [--claude-resets] [POPUP_FLAGS]\n  tmux-agent-usage claude-tap [--no-record] [-- COMMAND [ARGS...]]\n  tmux-agent-usage inspect [--agents LIST]\n  tmux-agent-usage help | --help | -h\n\nPOPUP_FLAGS:\n  --popup-{text,muted,dim,ok,high,critical,accent} COLOR\n    One tmux color name, colour0..colour255 or #rrggbb. Defaults use ANSI.\n  --popup-border STYLE (popup only; otherwise inherit tmux border style)\n    Rounded border is fixed. Title uses an explicit --popup-text color.";

pub enum Request {
    Status(AgentId, Styles),
    Popup {
        client: String,
        cols: usize,
        rows: usize,
        options: PopupOptions,
    },
    Details {
        options: PopupOptions,
        wait: bool,
    },
    Tap {
        record: bool,
        wrapped: Vec<String>,
    },
    Inspect(Vec<AgentId>),
    Help,
}

pub struct PopupOptions {
    pub agents: Vec<AgentId>,
    pub claude_resets: bool,
    pub theme: PopupTheme,
}

impl Default for PopupOptions {
    fn default() -> Self {
        Self {
            agents: AgentId::ALL.to_vec(),
            claude_resets: false,
            theme: PopupTheme::default(),
        }
    }
}

fn selection(list: &str) -> Result<Vec<AgentId>, String> {
    let mut agents = Vec::new();
    for name in list.split_whitespace() {
        let agent = AgentId::parse(name)?;
        if agents.contains(&agent) {
            return Err(format!("duplicate agent {name:?}"));
        }
        agents.push(agent);
    }
    if agents.is_empty() {
        return Err("--agents requires at least one agent".to_owned());
    }
    Ok(agents)
}

fn value<'a>(arguments: &'a [String], index: &mut usize) -> Result<&'a str, String> {
    let flag = &arguments[*index];
    *index += 1;
    arguments
        .get(*index)
        .map(String::as_str)
        .ok_or_else(|| format!("missing value for {flag}"))
}

pub fn parse(arguments: &[String]) -> Result<Request, String> {
    let Some((command, args)) = arguments.split_first() else {
        return Err("missing command".to_owned());
    };
    match command.as_str() {
        "status" => {
            let Some((agent, flags)) = args.split_first() else {
                return Err("status requires an agent (claude or codex)".to_owned());
            };
            let agent = AgentId::parse(agent)?;
            let mut styles = Styles::default();
            let mut index = 0;
            while index < flags.len() {
                let flag = &flags[index];
                let style = value(flags, &mut index)?;
                styles.set(flag, style)?;
                index += 1;
            }
            Ok(Request::Status(agent, styles))
        }
        "popup" | "details" | "inspect" => {
            let mut index = 0;
            let mut client = String::new();
            let (mut cols, mut rows) = (80, 24);
            if command == "popup" {
                client = args
                    .first()
                    .filter(|client| !client.trim().is_empty() && !client.starts_with('-'))
                    .ok_or("popup requires a nonempty client")?
                    .clone();
                index += 1;
                if args.get(index).is_some_and(|arg| !arg.starts_with("--")) {
                    let dimension = |value: Option<&String>| {
                        value
                            .and_then(|value| value.parse::<usize>().ok())
                            .filter(|value| *value > 0)
                            .ok_or("popup requires positive integer COLS and ROWS")
                    };
                    cols = dimension(args.get(index))?;
                    rows = dimension(args.get(index + 1))?;
                    index += 2;
                }
            }
            let mut options = PopupOptions::default();
            let mut wait = true;
            while index < args.len() {
                match args[index].as_str() {
                    "--agents" => options.agents = selection(value(args, &mut index)?)?,
                    "--claude-resets" if command != "inspect" => options.claude_resets = true,
                    "--no-wait" if command == "details" => wait = false,
                    flag if command != "inspect"
                        && flag.starts_with("--popup-")
                        && (command == "popup" || flag != "--popup-border") =>
                    {
                        let flag = args[index].clone();
                        options.theme.set(&flag, value(args, &mut index)?)?;
                    }
                    flag => return Err(format!("unexpected {command} argument {flag:?}")),
                }
                index += 1;
            }
            match command.as_str() {
                "popup" => Ok(Request::Popup {
                    client,
                    cols,
                    rows,
                    options,
                }),
                "details" => Ok(Request::Details { options, wait }),
                _ => Ok(Request::Inspect(options.agents)),
            }
        }
        "claude-tap" => {
            let mut record = true;
            let mut index = 0;
            while let Some(arg) = args.get(index) {
                match arg.as_str() {
                    "--no-record" => record = false,
                    "--" => {
                        index += 1;
                        break;
                    }
                    flag if flag.starts_with('-') => {
                        return Err(format!("unknown claude-tap flag {flag:?}"));
                    }
                    _ => break,
                }
                index += 1;
            }
            Ok(Request::Tap {
                record,
                wrapped: args[index..].to_vec(),
            })
        }
        "help" | "--help" | "-h" if args.is_empty() => Ok(Request::Help),
        _ => Err(format!(
            "unknown command or unexpected arguments: {command:?}"
        )),
    }
}
