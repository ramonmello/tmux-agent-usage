//! Private tmux servers and PTY clients shared by runtime integration tests.
use crate::test_support::{EVENT_TIMEOUT, success};
use std::fs::{self, File};
use std::io::{Read, Write};
use std::os::fd::{AsRawFd, FromRawFd};
use std::os::unix::process::CommandExt;
use std::path::PathBuf;
use std::process::{Child, Command};
use std::time::Instant;

pub struct Server {
    pub program: PathBuf,
    pub socket: PathBuf,
    pub started: bool,
}
impl Server {
    pub fn command(&self) -> Command {
        let mut command = Command::new(&self.program);
        command
            .args(["-S"])
            .arg(&self.socket)
            .args(["-f", "/dev/null", "-u"]);
        command
    }
    pub fn run(&self, args: &[&str]) -> String {
        String::from_utf8(success(self.command().args(args).output().unwrap()).stdout).unwrap()
    }
}
impl Drop for Server {
    fn drop(&mut self) {
        if self.started {
            let _ = self.command().arg("kill-server").output();
        }
    }
}

pub struct Client {
    pub child: Child,
    master: File,
    pub parser: vt100::Parser,
    bytes: Vec<u8>,
}

impl Client {
    pub fn attach(server: &Server) -> Self {
        Self::attach_to(server, "popup-test", 36, 120)
    }
    pub fn attach_to(server: &Server, session: &str, rows: u16, cols: u16) -> Self {
        let (mut master, mut slave) = (-1, -1);
        let mut size = libc::winsize {
            ws_row: rows,
            ws_col: cols,
            ws_xpixel: 0,
            ws_ypixel: 0,
        };
        assert_eq!(
            unsafe {
                libc::openpty(
                    &mut master,
                    &mut slave,
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                    &mut size,
                )
            },
            0,
            "isolated PTY: {}",
            std::io::Error::last_os_error()
        );
        let master = unsafe { File::from_raw_fd(master) };
        let slave = unsafe { File::from_raw_fd(slave) };
        let mut command = server.command();
        command
            .args(["attach-session", "-t", session])
            .env("TERM", "xterm-256color")
            .env("LC_ALL", "en_US.UTF-8")
            .stdin(slave.try_clone().unwrap())
            .stdout(slave.try_clone().unwrap())
            .stderr(slave);
        unsafe {
            command.pre_exec(|| {
                if libc::setsid() < 0 || libc::ioctl(0, libc::TIOCSCTTY as _, 0) < 0 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
        let child = command.spawn().unwrap();
        Self {
            child,
            master,
            parser: vt100::Parser::new(rows, cols, 0),
            bytes: vec![],
        }
    }
    pub fn pump(&mut self) {
        let mut poll = libc::pollfd {
            fd: self.master.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        };
        if unsafe { libc::poll(&mut poll, 1, 20) } > 0 {
            let mut bytes = [0; 65536];
            let count = self.master.read(&mut bytes).unwrap_or(0);
            self.parser.process(&bytes[..count]);
            self.bytes.extend_from_slice(&bytes[..count]);
        }
    }
    pub fn until(&mut self, description: &str, mut predicate: impl FnMut(&vt100::Screen) -> bool) {
        let started = Instant::now();
        let deadline = started + EVENT_TIMEOUT;
        loop {
            self.pump();
            if predicate(self.parser.screen()) {
                if std::env::var_os("AGENT_USAGE_TRACE_TESTS").is_some() {
                    eprintln!("{description}: {:?}", started.elapsed());
                }
                return;
            }
            assert!(
                self.child.try_wait().unwrap().is_none(),
                "tmux client exited while {description}"
            );
            assert!(
                Instant::now() < deadline,
                "{description} after {:?}: {}",
                started.elapsed(),
                self.parser.screen().contents()
            );
        }
    }
    pub fn send(&mut self, input: &[u8]) {
        self.master.write_all(input).unwrap();
        self.master.flush().unwrap();
    }
    pub fn capture(&self, name: &str) {
        let Some(dir) = std::env::var_os("AGENT_USAGE_CAPTURE_DIR") else {
            return;
        };
        let dir = PathBuf::from(dir);
        fs::create_dir_all(&dir).unwrap();
        let screen = self.parser.screen();
        let (rows, cols) = screen.size();
        let cells: Vec<_> = (0..rows)
            .map(|row| {
                (0..cols)
                    .map(|col| {
                        let cell = screen.cell(row, col).unwrap();
                        let color = |color| match color {
                            vt100::Color::Default => serde_json::Value::Null,
                            vt100::Color::Idx(index) => serde_json::json!(index),
                            vt100::Color::Rgb(r, g, b) => serde_json::json!([r, g, b]),
                        };
                        serde_json::json!([
                            cell.contents(),
                            color(cell.fgcolor()),
                            color(cell.bgcolor()),
                            cell.bold()
                        ])
                    })
                    .collect::<Vec<_>>()
            })
            .collect();
        fs::write(
            dir.join(format!("{name}.json")),
            serde_json::to_vec(&serde_json::json!({ "rows": rows, "cols": cols, "cells": cells }))
                .unwrap(),
        )
        .unwrap();
        fs::write(dir.join(format!("{name}.ansi")), &self.bytes).unwrap();
    }
}
impl Drop for Client {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}
