mod access;
mod cache;
mod claude;
mod claude_resets;
mod cli;
mod clock;
mod codex;
mod details;
mod localtime;
mod paths;
mod popup_layout;
mod popup_theme;
mod process;
mod provider;
mod render;
mod resets;
mod style;
mod terminal;

#[cfg(test)]
#[path = "../tests/support/goldens.rs"]
mod baseline_goldens;
#[cfg(test)]
#[path = "../tests/support/contract.rs"]
mod contract_tests;
#[cfg(test)]
#[path = "../tests/support/live_popup.rs"]
mod live_popup_tests;
#[cfg(test)]
#[path = "../tests/support/popup.rs"]
mod popup_tests;
#[cfg(test)]
#[path = "../tests/support/robustness.rs"]
mod robustness_support;
#[cfg(test)]
#[path = "../tests/support/mod.rs"]
mod test_support;
#[cfg(test)]
#[path = "../tests/support/theme.rs"]
mod theme_tests;
#[cfg(test)]
#[path = "../tests/support/tmux.rs"]
mod tmux_tests_support;

use cli::{PopupOptions, Request};
use clock::{Clock, SystemClock};
use provider::{AgentId, Usage};
use std::env;
use std::io::{self, Read, Write};
use std::process::{ExitCode, Stdio};
use std::sync::mpsc;
use std::thread;

fn main() -> ExitCode {
    let arguments: Vec<String> = env::args().skip(1).collect();
    let request = match cli::parse(&arguments) {
        Ok(request) => request,
        Err(error) => {
            eprintln!("tmux-agent-usage: {error}\n{}", cli::USAGE);
            return ExitCode::from(2);
        }
    };
    match execute(request, &SystemClock, &mut io::stdout()) {
        Ok(code) => ExitCode::from(code),
        Err(error) => {
            eprintln!("tmux-agent-usage: {error}");
            ExitCode::from(1)
        }
    }
}

fn execute(request: Request, clock: &dyn Clock, output: &mut dyn Write) -> io::Result<u8> {
    match request {
        Request::Status(agent, styles) => {
            let usage = agent.load();
            write!(
                output,
                "{}",
                render::body(usage.gauges().as_deref(), &styles, clock.now())
            )?;
        }
        Request::Popup {
            client,
            cols,
            rows,
            options,
        } => run_popup(&client, cols, rows, &options, clock)?,
        Request::Details { options, wait } => run_details(&options, wait, clock, output)?,
        Request::Tap { record, wrapped } => {
            return run_claude_tap(record, &wrapped, clock, &mut io::stdin());
        }
        Request::Inspect(agents) => run_inspect(&agents, clock.now(), output)?,
        Request::Help => writeln!(output, "{}", cli::USAGE)?,
    }
    output.flush()?;
    Ok(0)
}

fn load_selected(agents: &[AgentId]) -> Vec<Usage> {
    agents.iter().map(|agent| agent.load()).collect()
}

#[derive(Default)]
struct Answers {
    claude: Option<claude_resets::ClaudeResets>,
    codex: Option<resets::SavedResets>,
}

fn sections(data: &[Usage], options: &PopupOptions, answers: &Answers, time: i64) -> Vec<String> {
    data.iter()
        .map(|data| match data {
            Usage::Claude(limits) if options.claude_resets => {
                options
                    .theme
                    .claude(limits.as_ref(), answers.claude.as_ref(), time)
            }
            Usage::Claude(limits) => options.theme.claude_local(limits.as_ref(), time),
            Usage::Codex(status) => {
                options
                    .theme
                    .codex(status.as_ref(), answers.codex.as_ref(), time)
            }
        })
        .collect()
}

fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

/// Only the selected providers' local data and reset caches affect sizing.
fn run_popup(
    client: &str,
    client_cols: usize,
    client_rows: usize,
    options: &PopupOptions,
    clock: &dyn Clock,
) -> io::Result<()> {
    let time = clock.now();
    let data = load_selected(&options.agents);
    let mut answers = Answers::default();
    for agent in &options.agents {
        match agent {
            AgentId::Claude if options.claude_resets => {
                answers.claude =
                    Some(claude_resets::last_known(time).unwrap_or(claude_resets::PLACEHOLDER));
            }
            AgentId::Codex => {
                answers.codex = Some(resets::last_known().unwrap_or(resets::PLACEHOLDER))
            }
            _ => {}
        }
    }
    let sections = sections(&data, options, &answers, time);
    let sections: Vec<&str> = sections.iter().map(String::as_str).collect();
    let footer_entries: Vec<_> = data
        .iter()
        .map(|data| (data.id().title(), data.observed_at()))
        .collect();
    let footer = options.theme.selected_footer(&footer_entries, time);
    let (width, height) = options
        .theme
        .geometry(&sections, &footer, client_cols, client_rows);
    let program = env::current_exe()?;
    let mut child_args = vec![
        program.to_string_lossy().into_owned(),
        "details".to_owned(),
        "--agents".to_owned(),
        options
            .agents
            .iter()
            .map(|id| id.name())
            .collect::<Vec<_>>()
            .join(" "),
    ];
    if options.claude_resets {
        child_args.push("--claude-resets".to_owned());
    }
    child_args.extend(options.theme.child_flags());
    let child_command = child_args
        .iter()
        .map(|arg| shell_quote(arg))
        .collect::<Vec<_>>()
        .join(" ");
    let mut command = access::command("tmux");
    command.args(["display-popup", "-c", client, "-E", "-b", "rounded"]);
    if let Some(border) = &options.theme.border {
        command.args(["-S", border]);
    }
    let status = command
        .args([
            "-T",
            &options.theme.title(),
            "-w",
            &width.min(client_cols).to_string(),
            "-h",
            &height.min(client_rows).to_string(),
            &child_command,
        ])
        .status()?;
    if status.success() {
        Ok(())
    } else {
        Err(io::Error::other(format!(
            "tmux display-popup failed: {status}"
        )))
    }
}

fn run_details(
    options: &PopupOptions,
    wait: bool,
    clock: &dyn Clock,
    output: &mut dyn Write,
) -> io::Result<()> {
    let _terminal = terminal::Terminal::enter(wait)?;
    let mut screen = terminal::Screen::new(output);
    screen.start()?;
    let (rows, cols) = terminal::size().unwrap_or((usize::MAX, details::side_by_side_width()));
    let data = load_selected(&options.agents);
    let footer_entries: Vec<_> = data
        .iter()
        .map(|data| (data.id().title(), data.observed_at()))
        .collect();
    let footer = options.theme.selected_footer(&footer_entries, clock.now());
    let draw = |output: &mut dyn Write, answers: &Answers| -> io::Result<()> {
        let sections = sections(&data, options, answers, clock.now());
        let sections: Vec<&str> = sections.iter().map(String::as_str).collect();
        write!(
            output,
            "\x1b[H\x1b[2J{}",
            options.theme.viewport(&sections, &footer, cols, rows)
        )?;
        output.flush()
    };
    let mut answers = Answers::default();
    draw(screen.output, &answers)?;
    enum Answer {
        Claude(claude_resets::ClaudeResets),
        Codex(resets::SavedResets),
    }
    // Scoped workers finish cleanup before returning, including on draw errors
    // and signals. Process guards poll cancellation while doing bounded I/O.
    thread::scope(|scope| -> io::Result<()> {
        let (sender, receiver) = mpsc::channel();
        for agent in &options.agents {
            let time = clock.now();
            match agent {
                AgentId::Claude if options.claude_resets => {
                    let sender = sender.clone();
                    scope.spawn(move || {
                        let _ = sender.send(Answer::Claude(claude_resets::load(time)));
                    });
                }
                AgentId::Codex => {
                    let sender = sender.clone();
                    scope.spawn(move || {
                        let _ = sender.send(Answer::Codex(resets::load(time)));
                    });
                }
                _ => {}
            }
        }
        drop(sender);
        loop {
            if terminal::cancelled() {
                return Err(io::Error::new(
                    io::ErrorKind::Interrupted,
                    "details interrupted",
                ));
            }
            match receiver.recv_timeout(std::time::Duration::from_millis(50)) {
                Ok(Answer::Claude(saved)) => answers.claude = Some(saved),
                Ok(Answer::Codex(saved)) => answers.codex = Some(saved),
                Err(mpsc::RecvTimeoutError::Timeout) => continue,
                Err(mpsc::RecvTimeoutError::Disconnected) => break,
            }
            if let Err(error) = draw(screen.output, &answers) {
                terminal::cancel();
                return Err(error);
            }
        }
        Ok(())
    })?;
    if wait {
        terminal::wait_for_close(closes)?;
    }
    screen.finish()
}

/// Whether a chunk of input should close the popup.
fn closes(input: &[u8]) -> bool {
    let Some(mut rest) = input.strip_prefix(b"\x1b[<") else {
        return true; // a key
    };
    // One or more SGR mouse reports: `\x1b[<b;x;yM` (press) or `…m` (release).
    loop {
        let Some(end) = rest.iter().position(|byte| *byte == b'M' || *byte == b'm') else {
            return false;
        };
        let button: u32 = std::str::from_utf8(&rest[..end])
            .ok()
            .and_then(|report| report.split(';').next())
            .and_then(|button| button.parse().ok())
            .unwrap_or(64);
        // Press of a plain button (not wheel 64+, not motion 32+).
        if rest[end] == b'M' && button & (32 | 64) == 0 {
            return true;
        }
        match rest[end + 1..].strip_prefix(b"\x1b[<") {
            Some(next) => rest = next,
            None => return false,
        }
    }
}

/// Recording is independent of tmux selection and is always best effort.
fn run_claude_tap(
    record: bool,
    wrapped: &[String],
    clock: &dyn Clock,
    input: &mut dyn Read,
) -> io::Result<u8> {
    let mut payload = Vec::new();
    input
        .read_to_end(&mut payload)
        .map_err(|error| io::Error::other(format!("cannot read stdin: {error}")))?;
    if record {
        let _ = claude::record(&payload, clock.now());
    }
    let Some((program, rest)) = wrapped.split_first() else {
        return Ok(0);
    };
    let mut child = access::command(program)
        .args(rest)
        .stdin(Stdio::piped())
        .spawn()
        .map_err(|error| io::Error::other(format!("cannot run {program:?}: {error}")))?;
    if let Some(mut stdin) = child.stdin.take() {
        let _ = stdin.write_all(&payload);
    }
    Ok(child.wait()?.code().unwrap_or(1).clamp(0, 255) as u8)
}

fn run_inspect(agents: &[AgentId], time: i64, output: &mut dyn Write) -> io::Result<()> {
    let mut report = serde_json::Map::new();
    report.insert("now".to_owned(), time.into());
    for data in load_selected(agents) {
        match data {
            Usage::Claude(data) => {
                report.insert(
                    "claude_snapshot_status".to_owned(),
                    claude::snapshot_status(data.as_ref().map(|(limits, _)| limits)).into(),
                );
                report.insert(
                    "claude_snapshots".to_owned(),
                    serde_json::json!(claude::snapshots_dir()),
                );
                report.insert(
                    "claude".to_owned(),
                    serde_json::json!(data.map(|(limits, _)| limits)),
                );
            }
            Usage::Codex(data) => {
                report.insert(
                    "codex_sessions".to_owned(),
                    serde_json::json!(paths::codex_sessions_dir()),
                );
                report.insert(
                    "codex".to_owned(),
                    serde_json::json!(data.map(|status| status.windows)),
                );
            }
        }
    }
    writeln!(output, "{}", serde_json::to_string_pretty(&report)?)
}

#[cfg(test)]
mod tests {
    use super::closes;

    #[test]
    fn closes_on_keys_and_clicks_only() {
        assert!(closes(b"q"));
        assert!(closes(b"\x1b"));
        assert!(closes(b"\x1b[A"));
        assert!(closes(b"\x1b[<0;10;5M"));
        assert!(!closes(b"\x1b[<0;10;5m"));
        assert!(!closes(b"\x1b[<64;10;5M\x1b[<64;10;5M"));
        assert!(!closes(b"\x1b[<35;10;5M"));
        assert!(closes(b"\x1b[<64;1;1M\x1b[<0;1;1M"));
    }
}
